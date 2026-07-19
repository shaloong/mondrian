//! Bounded persistent FFmpeg child-process sessions for decoded source windows.

use super::{
    audio_frame_timestamp, canceled_audio_decode, mapping::standard_pan_filter,
    AudioSourceIdentity, AudioWindowDecoder, AudioWindowDecoderDiagnostics,
};
use crate::audio::AudioBuffer;
use mondrian_core::{AudioChannelLayout, ExecutionCancellationToken, MondrianError, Result};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::io::Read;
use std::process::{Child, ChildStderr, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const SESSION_CAPACITY: usize = 8;
const EXACT_SEEK_PREROLL_SECONDS: i64 = 10;
const PUMP_CHUNK_BYTES: usize = 64 * 1024;
const PUMP_LOOKAHEAD_CHUNKS: usize = 2;
const STDERR_TAIL_BYTES: usize = 64 * 1024;
const CANCELLATION_POLL: Duration = Duration::from_millis(5);

/// Product decoder that reuses one bounded FFmpeg stream per active source contract.
pub(super) struct PersistentFfmpegAudioWindowDecoder {
    state: Mutex<DecoderState>,
    session_capacity: usize,
}

impl Default for PersistentFfmpegAudioWindowDecoder {
    fn default() -> Self {
        Self {
            state: Mutex::new(DecoderState::default()),
            session_capacity: SESSION_CAPACITY,
        }
    }
}

#[derive(Default)]
struct DecoderState {
    entries: VecDeque<DecoderEntry>,
    peak_sessions: usize,
    session_opens: u64,
    sequential_reuses: u64,
    random_seek_restarts: u64,
    session_evictions: u64,
    cancellations: u64,
    cold_window_max_duration_us: u64,
    sequential_window_max_duration_us: u64,
    random_seek_window_max_duration_us: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionKey {
    source: AudioSourceIdentity,
    sample_rate: u32,
    channel_layout: AudioChannelLayout,
}

struct DecoderEntry {
    key: SessionKey,
    slot: Arc<Mutex<Option<DecodeSession>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowKind {
    Cold,
    Sequential,
    RandomSeek,
}

impl AudioWindowDecoder for PersistentFfmpegAudioWindowDecoder {
    fn decode_window(
        &self,
        source: &AudioSourceIdentity,
        start_frame: i64,
        frame_count: usize,
        sample_rate: u32,
        channel_layout: AudioChannelLayout,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<AudioBuffer> {
        if cancellation.is_canceled() {
            return Err(canceled_audio_decode(&source.path));
        }

        let key = SessionKey {
            source: source.clone(),
            sample_rate: sample_rate.max(8_000),
            channel_layout,
        };
        let slot = self.acquire_slot(key.clone(), cancellation)?;
        let mut session = self.lock_slot(&slot, source, cancellation)?;
        let start_frame = start_frame.max(0);
        let kind = match session.as_ref() {
            None => WindowKind::Cold,
            Some(existing) if existing.next_frame == start_frame => WindowKind::Sequential,
            Some(_) => WindowKind::RandomSeek,
        };

        if kind == WindowKind::RandomSeek {
            if let Some(mut previous) = session.take() {
                previous.terminate();
            }
        }

        let started = Instant::now();
        let mut opened = false;
        if session.is_none() {
            match DecodeSession::spawn(&key, start_frame) {
                Ok(created) => {
                    *session = Some(created);
                    opened = true;
                }
                Err(error) => {
                    drop(session);
                    self.remove_slot(&slot);
                    self.record_window(kind, false, started.elapsed(), cancellation.is_canceled());
                    return Err(error);
                }
            }
        }
        let mut result = match session.as_mut() {
            Some(active) => active.decode(frame_count, cancellation),
            None => Err(MondrianError::DecodeFailed {
                asset_id: source.path.display().to_string(),
                reason: "persistent audio session was not created".to_owned(),
            }),
        };
        if cancellation.is_canceled() && result.is_ok() {
            result = Err(canceled_audio_decode(&source.path));
        }
        let failed = result.is_err();
        if failed {
            if let Some(mut failed) = session.take() {
                failed.terminate();
            }
        }
        drop(session);
        if failed {
            self.remove_slot(&slot);
        }

        self.record_window(kind, opened, started.elapsed(), cancellation.is_canceled());
        result
    }

    fn diagnostics(&self) -> AudioWindowDecoderDiagnostics {
        let state = self.state.lock();
        AudioWindowDecoderDiagnostics {
            sessions: state.entries.len(),
            session_capacity: self.session_capacity,
            peak_sessions: state.peak_sessions,
            session_opens: state.session_opens,
            sequential_reuses: state.sequential_reuses,
            random_seek_restarts: state.random_seek_restarts,
            session_evictions: state.session_evictions,
            cancellations: state.cancellations,
            cold_window_max_duration_us: state.cold_window_max_duration_us,
            sequential_window_max_duration_us: state.sequential_window_max_duration_us,
            random_seek_window_max_duration_us: state.random_seek_window_max_duration_us,
        }
    }
}

impl PersistentFfmpegAudioWindowDecoder {
    fn record_window(&self, kind: WindowKind, opened: bool, duration: Duration, canceled: bool) {
        let duration_us = duration.as_micros().min(u64::MAX as u128) as u64;
        let mut state = self.state.lock();
        if opened {
            state.session_opens = state.session_opens.saturating_add(1);
        }
        match kind {
            WindowKind::Cold => {
                state.cold_window_max_duration_us =
                    state.cold_window_max_duration_us.max(duration_us);
            }
            WindowKind::Sequential => {
                state.sequential_reuses = state.sequential_reuses.saturating_add(1);
                state.sequential_window_max_duration_us =
                    state.sequential_window_max_duration_us.max(duration_us);
            }
            WindowKind::RandomSeek => {
                state.random_seek_restarts = state.random_seek_restarts.saturating_add(1);
                state.random_seek_window_max_duration_us =
                    state.random_seek_window_max_duration_us.max(duration_us);
            }
        }
        if canceled {
            state.cancellations = state.cancellations.saturating_add(1);
        }
    }

    fn remove_slot(&self, slot: &Arc<Mutex<Option<DecodeSession>>>) {
        self.state.lock().entries.retain(|entry| !Arc::ptr_eq(&entry.slot, slot));
    }

    fn acquire_slot(
        &self,
        key: SessionKey,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<Arc<Mutex<Option<DecodeSession>>>> {
        loop {
            if cancellation.is_canceled() {
                return Err(canceled_audio_decode(&key.source.path));
            }

            let mut evicted = None;
            let selected = {
                let mut state = self.state.lock();
                if let Some(index) = state.entries.iter().position(|entry| entry.key == key) {
                    state.entries.remove(index).map(|entry| {
                        let slot = Arc::clone(&entry.slot);
                        state.entries.push_front(entry);
                        slot
                    })
                } else if state.entries.len() < self.session_capacity {
                    let slot = Arc::new(Mutex::new(None));
                    state
                        .entries
                        .push_front(DecoderEntry { key: key.clone(), slot: Arc::clone(&slot) });
                    state.peak_sessions = state.peak_sessions.max(state.entries.len());
                    Some(slot)
                } else if let Some(index) =
                    state.entries.iter().rposition(|entry| Arc::strong_count(&entry.slot) == 1)
                {
                    evicted = state.entries.remove(index);
                    state.session_evictions = state.session_evictions.saturating_add(1);
                    let slot = Arc::new(Mutex::new(None));
                    state
                        .entries
                        .push_front(DecoderEntry { key: key.clone(), slot: Arc::clone(&slot) });
                    Some(slot)
                } else {
                    None
                }
            };

            if let Some(entry) = evicted {
                terminate_entry(entry);
            }
            if let Some(slot) = selected {
                return Ok(slot);
            }
            std::thread::sleep(CANCELLATION_POLL);
        }
    }

    fn lock_slot<'a>(
        &self,
        slot: &'a Mutex<Option<DecodeSession>>,
        source: &AudioSourceIdentity,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<parking_lot::MutexGuard<'a, Option<DecodeSession>>> {
        loop {
            if let Some(guard) = slot.try_lock() {
                return Ok(guard);
            }
            if cancellation.is_canceled() {
                return Err(canceled_audio_decode(&source.path));
            }
            std::thread::sleep(CANCELLATION_POLL);
        }
    }
}

impl Drop for PersistentFfmpegAudioWindowDecoder {
    fn drop(&mut self) {
        let entries = std::mem::take(&mut self.state.get_mut().entries);
        terminate_entries(entries);
    }
}

fn terminate_entries(entries: VecDeque<DecoderEntry>) {
    for entry in entries {
        terminate_entry(entry);
    }
}

fn terminate_entry(entry: DecoderEntry) {
    if let Some(mut session) = entry.slot.lock().take() {
        session.terminate();
    }
}

enum StdoutMessage {
    Data(Vec<u8>),
    Eof,
    Error(String),
}

struct DecodeSession {
    source_path: std::path::PathBuf,
    sample_rate: u32,
    channel_layout: AudioChannelLayout,
    next_frame: i64,
    child: Option<Child>,
    terminal_status: Option<ExitStatus>,
    stdout_rx: Option<Receiver<StdoutMessage>>,
    stderr_rx: Option<Receiver<Vec<u8>>>,
    stdout_thread: Option<JoinHandle<()>>,
    stderr_thread: Option<JoinHandle<()>>,
    pending: Vec<u8>,
    pending_offset: usize,
    ended: bool,
}

impl DecodeSession {
    fn spawn(key: &SessionKey, start_frame: i64) -> Result<Self> {
        let (input_start_frame, exact_trim_frames) =
            exact_seek_partition(start_frame, key.sample_rate);
        let pan_filter =
            standard_pan_filter(key.source.selection.source_layout(), key.channel_layout)
                .ok_or_else(|| MondrianError::DecodeFailed {
                    asset_id: key.source.path.display().to_string(),
                    reason: format!(
                        "no explicit standard channel mapping from {:?} to {:?}",
                        key.source.selection.source_layout(),
                        key.channel_layout
                    ),
                })?;
        let mut command = Command::new("ffmpeg");
        command.arg("-v").arg("error").arg("-nostdin");
        if input_start_frame > 0 {
            command
                .arg("-ss")
                .arg(audio_frame_timestamp(input_start_frame, key.sample_rate));
        }
        command.arg("-i").arg(&key.source.path);
        if exact_trim_frames > 0 {
            command
                .arg("-ss")
                .arg(audio_frame_timestamp(exact_trim_frames, key.sample_rate));
        }
        command
            .arg("-map")
            .arg(ffmpeg_stream_map(key.source.selection.stream_index()))
            .arg("-vn")
            .arg("-sn")
            .arg("-dn")
            .arg("-f")
            .arg("f32le")
            .arg("-acodec")
            .arg("pcm_f32le")
            .arg("-filter:a")
            .arg(pan_filter)
            .arg("-channel_layout")
            .arg(ffmpeg_channel_layout(key.channel_layout))
            .arg("-ac")
            .arg(key.channel_layout.channel_count().to_string())
            .arg("-ar")
            .arg(key.sample_rate.to_string())
            .arg("pipe:1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        hide_child_window(&mut command);
        let mut child = command.spawn().map_err(|error| MondrianError::DecodeFailed {
            asset_id: key.source.path.display().to_string(),
            reason: format!("启动持久音频解码 Session 失败: {error}"),
        })?;
        let stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => {
                terminate_child(&mut child);
                return Err(MondrianError::DecodeFailed {
                    asset_id: key.source.path.display().to_string(),
                    reason: "ffmpeg persistent session did not expose stdout".to_owned(),
                });
            }
        };
        let stderr = match child.stderr.take() {
            Some(stderr) => stderr,
            None => {
                terminate_child(&mut child);
                return Err(MondrianError::DecodeFailed {
                    asset_id: key.source.path.display().to_string(),
                    reason: "ffmpeg persistent session did not expose stderr".to_owned(),
                });
            }
        };
        let (stdout_tx, stdout_rx) = mpsc::sync_channel(PUMP_LOOKAHEAD_CHUNKS);
        let (stderr_tx, stderr_rx) = mpsc::sync_channel(1);
        let stdout_thread = match std::thread::Builder::new()
            .name("mondrian-audio-stdout".to_owned())
            .spawn(move || pump_stdout(stdout, stdout_tx))
        {
            Ok(thread) => thread,
            Err(error) => {
                terminate_child(&mut child);
                return Err(MondrianError::DecodeFailed {
                    asset_id: key.source.path.display().to_string(),
                    reason: format!("启动音频 stdout pump 失败: {error}"),
                });
            }
        };
        let stderr_thread = match std::thread::Builder::new()
            .name("mondrian-audio-stderr".to_owned())
            .spawn(move || pump_stderr(stderr, stderr_tx))
        {
            Ok(thread) => thread,
            Err(error) => {
                drop(stdout_rx);
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_thread.join();
                return Err(MondrianError::DecodeFailed {
                    asset_id: key.source.path.display().to_string(),
                    reason: format!("启动音频 stderr pump 失败: {error}"),
                });
            }
        };

        Ok(Self {
            source_path: key.source.path.clone(),
            sample_rate: key.sample_rate,
            channel_layout: key.channel_layout,
            next_frame: start_frame,
            child: Some(child),
            terminal_status: None,
            stdout_rx: Some(stdout_rx),
            stderr_rx: Some(stderr_rx),
            stdout_thread: Some(stdout_thread),
            stderr_thread: Some(stderr_thread),
            pending: Vec::new(),
            pending_offset: 0,
            ended: false,
        })
    }

    fn decode(
        &mut self,
        frame_count: usize,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<AudioBuffer> {
        let frame_bytes =
            self.channel_layout.channel_count().saturating_mul(std::mem::size_of::<f32>());
        let target_bytes = frame_count.checked_mul(frame_bytes).ok_or_else(|| {
            MondrianError::Other(anyhow::anyhow!("audio decode window extent overflow"))
        })?;
        let mut bytes = Vec::with_capacity(target_bytes);

        while bytes.len() < target_bytes {
            if cancellation.is_canceled() {
                self.terminate();
                return Err(canceled_audio_decode(&self.source_path));
            }
            self.consume_pending(&mut bytes, target_bytes);
            if bytes.len() >= target_bytes || self.ended {
                break;
            }
            let message = match self.stdout_rx.as_ref() {
                Some(receiver) => receiver.recv_timeout(CANCELLATION_POLL),
                None => break,
            };
            match message {
                Ok(StdoutMessage::Data(chunk)) => {
                    self.pending = chunk;
                    self.pending_offset = 0;
                }
                Ok(StdoutMessage::Eof) | Err(RecvTimeoutError::Disconnected) => {
                    self.finish_after_stdout()?;
                }
                Ok(StdoutMessage::Error(reason)) => {
                    self.terminate();
                    return Err(MondrianError::DecodeFailed {
                        asset_id: self.source_path.display().to_string(),
                        reason,
                    });
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
        self.capture_exit_status()?;

        if bytes.len() % frame_bytes != 0 {
            self.terminate();
            return Err(MondrianError::DecodeFailed {
                asset_id: self.source_path.display().to_string(),
                reason: "ffmpeg returned a truncated interleaved f32le audio frame".to_owned(),
            });
        }
        let mut samples = Vec::with_capacity(bytes.len() / std::mem::size_of::<f32>());
        for chunk in bytes.chunks_exact(std::mem::size_of::<f32>()) {
            samples.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
        }
        let decoded_frames = samples.len() / self.channel_layout.channel_count();
        self.next_frame = self.next_frame.saturating_add(decoded_frames as i64);
        Ok(AudioBuffer {
            samples,
            sample_rate: self.sample_rate,
            channel_layout: self.channel_layout,
        })
    }

    fn consume_pending(&mut self, destination: &mut Vec<u8>, target_bytes: usize) {
        let available = self.pending.len().saturating_sub(self.pending_offset);
        let count = available.min(target_bytes.saturating_sub(destination.len()));
        if count > 0 {
            destination.extend_from_slice(
                &self.pending[self.pending_offset..self.pending_offset.saturating_add(count)],
            );
            self.pending_offset = self.pending_offset.saturating_add(count);
        }
        if self.pending_offset >= self.pending.len() {
            self.pending.clear();
            self.pending_offset = 0;
        }
    }

    fn finish_after_stdout(&mut self) -> Result<()> {
        self.ended = true;
        self.stdout_rx.take();
        let status = match self.terminal_status.take() {
            Some(status) => status,
            None => match self.child.as_mut() {
                Some(child) => child.wait().map_err(|error| MondrianError::DecodeFailed {
                    asset_id: self.source_path.display().to_string(),
                    reason: format!("等待持久音频解码 Session 失败: {error}"),
                })?,
                None => return Ok(()),
            },
        };
        self.child.take();
        join_thread(self.stdout_thread.take());
        join_thread(self.stderr_thread.take());
        let stderr = self.take_stderr_tail();
        if !status.success() {
            return Err(MondrianError::DecodeFailed {
                asset_id: self.source_path.display().to_string(),
                reason: format!(
                    "ffmpeg 持久音频解码 Session 失败: {}",
                    String::from_utf8_lossy(&stderr)
                ),
            });
        }
        Ok(())
    }

    fn capture_exit_status(&mut self) -> Result<()> {
        if self.terminal_status.is_some() {
            return Ok(());
        }
        let status = match self.child.as_mut() {
            Some(child) => child.try_wait().map_err(|error| MondrianError::DecodeFailed {
                asset_id: self.source_path.display().to_string(),
                reason: format!("检查持久音频解码 Session 状态失败: {error}"),
            })?,
            None => None,
        };
        if let Some(status) = status {
            self.child.take();
            let failed = !status.success();
            self.terminal_status = Some(status);
            if failed {
                return self.finish_after_stdout();
            }
        }
        Ok(())
    }

    fn take_stderr_tail(&mut self) -> Vec<u8> {
        self.stderr_rx
            .take()
            .and_then(|receiver| receiver.try_recv().ok())
            .unwrap_or_default()
    }

    fn terminate(&mut self) {
        self.ended = true;
        self.stdout_rx.take();
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.child.take();
        self.terminal_status.take();
        join_thread(self.stdout_thread.take());
        join_thread(self.stderr_thread.take());
        self.take_stderr_tail();
        self.pending.clear();
        self.pending_offset = 0;
    }
}

fn ffmpeg_channel_layout(layout: AudioChannelLayout) -> &'static str {
    match layout {
        AudioChannelLayout::Mono => "mono",
        AudioChannelLayout::Stereo => "stereo",
        AudioChannelLayout::Surround51 => "5.1(side)",
    }
}

fn ffmpeg_stream_map(stream_index: u32) -> String {
    format!("0:{stream_index}")
}

impl Drop for DecodeSession {
    fn drop(&mut self) {
        self.terminate();
    }
}

fn pump_stdout(mut stdout: ChildStdout, sender: SyncSender<StdoutMessage>) {
    loop {
        let mut chunk = vec![0_u8; PUMP_CHUNK_BYTES];
        match stdout.read(&mut chunk) {
            Ok(0) => {
                let _ = sender.send(StdoutMessage::Eof);
                break;
            }
            Ok(count) => {
                chunk.truncate(count);
                if sender.send(StdoutMessage::Data(chunk)).is_err() {
                    break;
                }
            }
            Err(error) => {
                let _ = sender.send(StdoutMessage::Error(format!(
                    "读取 ffmpeg 持久音频输出失败: {error}"
                )));
                break;
            }
        }
    }
}

fn pump_stderr(mut stderr: ChildStderr, sender: SyncSender<Vec<u8>>) {
    let mut tail = VecDeque::with_capacity(STDERR_TAIL_BYTES);
    let mut chunk = [0_u8; 8 * 1024];
    loop {
        match stderr.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(count) => append_bounded_tail(&mut tail, &chunk[..count], STDERR_TAIL_BYTES),
        }
    }
    let _ = sender.send(tail.into_iter().collect());
}

fn append_bounded_tail(tail: &mut VecDeque<u8>, bytes: &[u8], capacity: usize) {
    if capacity == 0 {
        tail.clear();
        return;
    }
    let skip = bytes.len().saturating_sub(capacity);
    tail.extend(bytes[skip..].iter().copied());
    while tail.len() > capacity {
        tail.pop_front();
    }
}

fn exact_seek_partition(start_frame: i64, sample_rate: u32) -> (i64, i64) {
    let start_frame = start_frame.max(0);
    let preroll_frames = i64::from(sample_rate.max(1)).saturating_mul(EXACT_SEEK_PREROLL_SECONDS);
    let input_start_frame = start_frame.saturating_sub(preroll_frames).max(0);
    (
        input_start_frame,
        start_frame.saturating_sub(input_start_frame),
    )
}

fn join_thread(thread: Option<JoinHandle<()>>) {
    if let Some(thread) = thread {
        let _ = thread.join();
    }
}

fn terminate_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(windows)]
fn hide_child_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_child_window(_command: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stderr_tail_is_strictly_bounded_and_keeps_the_latest_bytes() {
        let mut tail = VecDeque::new();
        append_bounded_tail(&mut tail, b"abcdef", 4);
        append_bounded_tail(&mut tail, b"gh", 4);
        assert_eq!(tail.into_iter().collect::<Vec<_>>(), b"efgh");
    }

    #[test]
    fn exact_seek_partition_bounds_decode_preroll_without_shifting_target() {
        let sample_rate = 48_000;
        assert_eq!(exact_seek_partition(0, sample_rate), (0, 0));
        assert_eq!(
            exact_seek_partition(sample_rate.into(), sample_rate),
            (0, 48_000)
        );
        assert_eq!(exact_seek_partition(480_000, sample_rate), (0, 480_000));
        assert_eq!(
            exact_seek_partition(528_000, sample_rate),
            (48_000, 480_000)
        );
        let (input, trim) = exact_seek_partition(i64::MAX, sample_rate);
        assert_eq!(input.saturating_add(trim), i64::MAX);
        assert!(trim <= 480_000);
    }

    #[test]
    fn ffmpeg_layout_names_preserve_semantic_surround_positions() {
        assert_eq!(ffmpeg_channel_layout(AudioChannelLayout::Mono), "mono");
        assert_eq!(ffmpeg_channel_layout(AudioChannelLayout::Stereo), "stereo");
        assert_eq!(
            ffmpeg_channel_layout(AudioChannelLayout::Surround51),
            "5.1(side)"
        );
    }

    #[test]
    fn ffmpeg_stream_map_uses_the_absolute_container_index() {
        assert_eq!(ffmpeg_stream_map(0), "0:0");
        assert_eq!(ffmpeg_stream_map(7), "0:7");
    }
}
