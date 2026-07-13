//! 音频缓冲区与混合器

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_queue::ArrayQueue;
use mondrian_core::{MondrianError, Result};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 原始 PCM 音频缓冲区（f32 交错格式）
#[derive(Debug, Clone)]
pub struct AudioBuffer {
    /// 交错 PCM f32 样本 [L0, R0, L1, R1, ...]
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub channels: u8,
}

impl AudioBuffer {
    pub fn silent(sample_rate: u32, channels: u8, frames: usize) -> Self {
        Self {
            samples: vec![0.0; frames * channels as usize],
            sample_rate,
            channels,
        }
    }

    /// 帧数（样本数 / 声道数）
    pub fn frame_count(&self) -> usize {
        self.samples.len() / self.channels as usize
    }

    /// 时长（秒）
    pub fn duration_secs(&self) -> f64 {
        self.frame_count() as f64 / self.sample_rate as f64
    }

    /// 就地增益调整（线性乘数）
    pub fn apply_gain(&mut self, gain: f32) {
        for s in &mut self.samples {
            *s *= gain;
        }
    }

    /// 混合另一个缓冲区（加法混合，无响度归一化）
    pub fn mix_from(&mut self, other: &Self, gain: f32) {
        let len = self.samples.len().min(other.samples.len());
        for i in 0..len {
            self.samples[i] += other.samples[i] * gain;
        }
    }

    pub fn slice_frames(&self, start_frame: usize, frame_count: usize) -> Self {
        let channels = self.channels as usize;
        let total_frames = self.frame_count();
        if start_frame >= total_frames || frame_count == 0 {
            return Self::silent(self.sample_rate, self.channels, 0);
        }

        let end_frame = (start_frame + frame_count).min(total_frames);
        let start = start_frame * channels;
        let end = end_frame * channels;

        Self {
            samples: self.samples[start..end].to_vec(),
            sample_rate: self.sample_rate,
            channels: self.channels,
        }
    }
}

/// 多轨音频混合器
pub struct AudioMixer {
    pub output_sample_rate: u32,
    pub output_channels: u8,
}

pub struct RealtimeAudioOutput {
    sample_rate: u32,
    channels: u8,
    queue: Arc<ArrayQueue<f32>>,
    muted: Arc<AtomicBool>,
    active: Arc<AtomicBool>,
    activation_consumed_frames: Arc<AtomicU64>,
    activation_elapsed_ns: Arc<AtomicU64>,
    telemetry: Arc<RealtimeAudioOutputTelemetry>,
    _stream: cpal::Stream,
}

/// Sendable control/observation handle for a stream owned by its device thread.
#[derive(Clone)]
pub(crate) struct RealtimeAudioOutputHandle {
    sample_rate: u32,
    channels: u8,
    queue: Arc<ArrayQueue<f32>>,
    muted: Arc<AtomicBool>,
    active: Arc<AtomicBool>,
    activation_consumed_frames: Arc<AtomicU64>,
    activation_elapsed_ns: Arc<AtomicU64>,
    telemetry: Arc<RealtimeAudioOutputTelemetry>,
}

/// Callback-derived audio output evidence. This is not an exact hardware playhead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RealtimeAudioOutputSnapshot {
    /// Concrete CPAL stream generation.
    pub stream_generation: u64,
    /// Configured device sample rate.
    pub sample_rate: u32,
    /// Configured interleaved channel count.
    pub channels: u8,
    /// Frames requested by callbacks since stream creation.
    pub callback_consumed_frames: u64,
    /// Callback-consumed frames since the output was most recently activated.
    pub active_callback_consumed_frames: u64,
    /// Monotonic wall time since the current consumption interval began.
    pub active_duration: Option<Duration>,
    /// Number of output callbacks observed.
    pub callback_count: u64,
    /// Active callback frames filled with silence because PCM was unavailable.
    pub underrun_frames: u64,
    /// Frame count requested by the latest callback.
    pub last_callback_frames: u32,
    /// Runtime age of the latest callback, or `None` before the first callback.
    pub last_callback_age: Option<Duration>,
    /// PCM frames currently waiting in the output queue.
    pub buffered_frames: usize,
    /// Whether CPAL reported an asynchronous stream error.
    pub stream_failed: bool,
    /// Whether playback consumption is currently enabled.
    pub active: bool,
}

struct RealtimeAudioOutputTelemetry {
    stream_generation: u64,
    origin: Instant,
    callback_consumed_frames: AtomicU64,
    callback_count: AtomicU64,
    underrun_frames: AtomicU64,
    last_callback_frames: AtomicU64,
    last_callback_elapsed_ns: AtomicU64,
    stream_failed: AtomicBool,
}

impl RealtimeAudioOutputTelemetry {
    fn new() -> Self {
        static NEXT_STREAM_GENERATION: AtomicU64 = AtomicU64::new(1);
        Self {
            stream_generation: NEXT_STREAM_GENERATION.fetch_add(1, Ordering::Relaxed),
            origin: Instant::now(),
            callback_consumed_frames: AtomicU64::new(0),
            callback_count: AtomicU64::new(0),
            underrun_frames: AtomicU64::new(0),
            last_callback_frames: AtomicU64::new(0),
            last_callback_elapsed_ns: AtomicU64::new(0),
            stream_failed: AtomicBool::new(false),
        }
    }

    fn record_callback(&self, frames: usize, underrun_frames: usize) {
        self.callback_consumed_frames.fetch_add(frames as u64, Ordering::Relaxed);
        self.callback_count.fetch_add(1, Ordering::Relaxed);
        self.underrun_frames.fetch_add(underrun_frames as u64, Ordering::Relaxed);
        self.last_callback_frames.store(frames as u64, Ordering::Relaxed);
        let elapsed_ns = self.origin.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        self.last_callback_elapsed_ns.store(elapsed_ns, Ordering::Release);
    }
}

pub struct AudioSourceCache {
    sample_rate: u32,
    channels: u8,
    decoded: Mutex<HashMap<PathBuf, Arc<AudioBuffer>>>,
}

/// Monotonic cursor used to choose audio render windows; never a Clock Master.
#[derive(Debug, Clone)]
pub struct AudioRenderCursor {
    pub sample_rate: u32,
    started_at: Instant,
    offset_samples: i64,
}

impl AudioRenderCursor {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            started_at: Instant::now(),
            offset_samples: 0,
        }
    }

    pub fn now_samples(&self) -> i64 {
        let elapsed = self.started_at.elapsed().as_secs_f64();
        self.offset_samples + (elapsed * self.sample_rate as f64).round() as i64
    }

    pub fn now_seconds(&self) -> f64 {
        self.now_samples() as f64 / self.sample_rate as f64
    }

    pub fn seek_to_samples(&mut self, samples: i64) {
        self.started_at = Instant::now();
        self.offset_samples = samples;
    }
}

impl AudioMixer {
    pub fn new(sample_rate: u32, channels: u8) -> Self {
        Self {
            output_sample_rate: sample_rate,
            output_channels: channels,
        }
    }

    /// 混合多个音频轨道
    pub fn mix(&self, tracks: &[AudioTrackData]) -> AudioBuffer {
        if tracks.is_empty() {
            return AudioBuffer::silent(self.output_sample_rate, self.output_channels, 0);
        }

        let has_solo = tracks.iter().any(|t| t.config.is_solo && !t.config.is_muted);

        let max_frames = tracks.iter().map(|t| t.buffer.frame_count()).max().unwrap_or(0);

        let mut output =
            AudioBuffer::silent(self.output_sample_rate, self.output_channels, max_frames);

        for track in tracks {
            if track.config.is_muted {
                continue;
            }
            if has_solo && !track.config.is_solo {
                continue;
            }
            self.mix_track_with_pan(&mut output, &track.buffer, &track.config);
        }

        self.soft_clip(&mut output);
        output
    }

    fn mix_track_with_pan(
        &self,
        output: &mut AudioBuffer,
        source: &AudioBuffer,
        config: &AudioTrackConfig,
    ) {
        let src_channels = source.channels as usize;
        let dst_channels = output.channels as usize;
        let frames = source.frame_count().min(output.frame_count());
        let pan = config.pan.clamp(-1.0, 1.0);
        let left_gain = ((1.0 - pan) * 0.5).sqrt();
        let right_gain = ((1.0 + pan) * 0.5).sqrt();

        for frame in 0..frames {
            let src_base = frame * src_channels;
            let dst_base = frame * dst_channels;

            let src_l = source.samples.get(src_base).copied().unwrap_or(0.0);
            let src_r = if src_channels > 1 {
                source.samples.get(src_base + 1).copied().unwrap_or(src_l)
            } else {
                src_l
            };

            if dst_channels >= 2 {
                output.samples[dst_base] += src_l * config.volume * left_gain;
                output.samples[dst_base + 1] += src_r * config.volume * right_gain;
            } else if dst_channels == 1 {
                output.samples[dst_base] += ((src_l + src_r) * 0.5) * config.volume;
            }
        }
    }

    fn soft_clip(&self, output: &mut AudioBuffer) {
        for sample in &mut output.samples {
            *sample = sample.tanh();
        }
    }
}

impl RealtimeAudioOutput {
    pub fn try_new(sample_rate: u32, channels: u8) -> Result<Self> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| MondrianError::Other(anyhow::anyhow!("未找到默认音频输出设备")))?;

        let config = cpal::StreamConfig {
            channels: channels.max(1) as u16,
            sample_rate: cpal::SampleRate(sample_rate.max(8_000)),
            buffer_size: cpal::BufferSize::Default,
        };

        let queue_capacity = sample_rate as usize * channels.max(1) as usize * 2;
        let queue = Arc::new(ArrayQueue::new(queue_capacity.max(1)));
        let queue_for_cb = Arc::clone(&queue);
        let muted = Arc::new(AtomicBool::new(false));
        let active = Arc::new(AtomicBool::new(false));
        let telemetry = Arc::new(RealtimeAudioOutputTelemetry::new());
        let telemetry_for_error = Arc::clone(&telemetry);
        let err_fn = move |_error| {
            telemetry_for_error.stream_failed.store(true, Ordering::Release);
        };

        let default_config = device
            .default_output_config()
            .map_err(|e| MondrianError::Other(anyhow::anyhow!("读取默认输出配置失败: {e}")))?;

        let stream = match default_config.sample_format() {
            cpal::SampleFormat::F32 => {
                let muted_for_cb = Arc::clone(&muted);
                build_f32_stream(
                    &device,
                    &config,
                    queue_for_cb,
                    muted_for_cb,
                    Arc::clone(&active),
                    Arc::clone(&telemetry),
                    err_fn,
                )
                .map_err(|e| MondrianError::Other(anyhow::anyhow!("创建 F32 输出流失败: {e}")))?
            }
            cpal::SampleFormat::I16 => {
                let queue_for_cb = Arc::clone(&queue);
                let muted_for_cb = Arc::clone(&muted);
                let active_for_cb = Arc::clone(&active);
                let telemetry_for_cb = Arc::clone(&telemetry);
                device
                    .build_output_stream(
                        &config,
                        move |data: &mut [i16], _| {
                            let frames = data.len() / channels.max(1) as usize;
                            if muted_for_cb.load(Ordering::Relaxed)
                                || !active_for_cb.load(Ordering::Relaxed)
                            {
                                data.fill(0);
                                telemetry_for_cb.record_callback(frames, 0);
                                return;
                            }
                            let mut missing_samples = 0usize;
                            for s in data {
                                let v = queue_for_cb
                                    .pop()
                                    .unwrap_or_else(|| {
                                        missing_samples = missing_samples.saturating_add(1);
                                        0.0
                                    })
                                    .clamp(-1.0, 1.0);
                                *s = (v * i16::MAX as f32) as i16;
                            }
                            telemetry_for_cb.record_callback(
                                frames,
                                missing_samples / channels.max(1) as usize,
                            );
                        },
                        err_fn,
                        None,
                    )
                    .map_err(|e| {
                        MondrianError::Other(anyhow::anyhow!("创建 I16 输出流失败: {e}"))
                    })?
            }
            cpal::SampleFormat::U16 => {
                let queue_for_cb = Arc::clone(&queue);
                let muted_for_cb = Arc::clone(&muted);
                let active_for_cb = Arc::clone(&active);
                let telemetry_for_cb = Arc::clone(&telemetry);
                device
                    .build_output_stream(
                        &config,
                        move |data: &mut [u16], _| {
                            let frames = data.len() / channels.max(1) as usize;
                            if muted_for_cb.load(Ordering::Relaxed)
                                || !active_for_cb.load(Ordering::Relaxed)
                            {
                                data.fill(u16::MAX / 2);
                                telemetry_for_cb.record_callback(frames, 0);
                                return;
                            }
                            let mut missing_samples = 0usize;
                            for s in data {
                                let v = queue_for_cb
                                    .pop()
                                    .unwrap_or_else(|| {
                                        missing_samples = missing_samples.saturating_add(1);
                                        0.0
                                    })
                                    .clamp(-1.0, 1.0);
                                *s = ((v * 0.5 + 0.5) * u16::MAX as f32) as u16;
                            }
                            telemetry_for_cb.record_callback(
                                frames,
                                missing_samples / channels.max(1) as usize,
                            );
                        },
                        err_fn,
                        None,
                    )
                    .map_err(|e| {
                        MondrianError::Other(anyhow::anyhow!("创建 U16 输出流失败: {e}"))
                    })?
            }
            _ => {
                return Err(MondrianError::Other(anyhow::anyhow!(
                    "当前音频设备采样格式不受支持"
                )));
            }
        };

        stream
            .play()
            .map_err(|e| MondrianError::Other(anyhow::anyhow!("启动音频输出流失败: {e}")))?;

        Ok(Self {
            sample_rate,
            channels,
            queue,
            muted,
            active,
            activation_consumed_frames: Arc::new(AtomicU64::new(0)),
            activation_elapsed_ns: Arc::new(AtomicU64::new(0)),
            telemetry,
            _stream: stream,
        })
    }

    pub(crate) fn handle(&self) -> RealtimeAudioOutputHandle {
        RealtimeAudioOutputHandle {
            sample_rate: self.sample_rate,
            channels: self.channels,
            queue: Arc::clone(&self.queue),
            muted: Arc::clone(&self.muted),
            active: Arc::clone(&self.active),
            activation_consumed_frames: Arc::clone(&self.activation_consumed_frames),
            activation_elapsed_ns: Arc::clone(&self.activation_elapsed_ns),
            telemetry: Arc::clone(&self.telemetry),
        }
    }

    pub fn enqueue(&self, buffer: &AudioBuffer) {
        if buffer.samples.is_empty() {
            return;
        }
        for sample in &buffer.samples {
            let mut pending = *sample;
            loop {
                match self.queue.push(pending) {
                    Ok(()) => break,
                    Err(returned) => {
                        pending = returned;
                        let _ = self.queue.pop();
                    }
                }
            }
        }
    }

    pub fn clear(&self) {
        while self.queue.pop().is_some() {}
    }

    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }

    /// Begin or end one playback-consumption interval.
    pub fn set_active(&self, active: bool) {
        let was_active = self.active.swap(active, Ordering::AcqRel);
        if active && !was_active {
            let elapsed_ns =
                self.telemetry.origin.elapsed().as_nanos().min(u64::MAX as u128) as u64;
            self.activation_consumed_frames.store(
                self.telemetry.callback_consumed_frames.load(Ordering::Acquire),
                Ordering::Release,
            );
            self.activation_elapsed_ns.store(elapsed_ns, Ordering::Release);
        }
    }

    pub fn buffered_frames(&self) -> usize {
        self.queue.len() / self.channels.max(1) as usize
    }

    /// Capture callback-consumption and health evidence without touching CPAL.
    pub fn snapshot(&self) -> RealtimeAudioOutputSnapshot {
        let last_elapsed_ns = self.telemetry.last_callback_elapsed_ns.load(Ordering::Acquire);
        let now_ns = self.telemetry.origin.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        let callback_consumed_frames =
            self.telemetry.callback_consumed_frames.load(Ordering::Acquire);
        let active = self.active.load(Ordering::Acquire);
        RealtimeAudioOutputSnapshot {
            stream_generation: self.telemetry.stream_generation,
            sample_rate: self.sample_rate,
            channels: self.channels,
            callback_consumed_frames,
            active_callback_consumed_frames: callback_consumed_frames
                .saturating_sub(self.activation_consumed_frames.load(Ordering::Acquire)),
            active_duration: active.then(|| {
                Duration::from_nanos(
                    now_ns.saturating_sub(self.activation_elapsed_ns.load(Ordering::Acquire)),
                )
            }),
            callback_count: self.telemetry.callback_count.load(Ordering::Relaxed),
            underrun_frames: self.telemetry.underrun_frames.load(Ordering::Relaxed),
            last_callback_frames: self
                .telemetry
                .last_callback_frames
                .load(Ordering::Relaxed)
                .min(u32::MAX as u64) as u32,
            last_callback_age: (last_elapsed_ns > 0)
                .then(|| Duration::from_nanos(now_ns.saturating_sub(last_elapsed_ns))),
            buffered_frames: self.buffered_frames(),
            stream_failed: self.telemetry.stream_failed.load(Ordering::Acquire),
            active,
        }
    }
}

impl RealtimeAudioOutputHandle {
    pub(crate) fn enqueue(&self, buffer: &AudioBuffer) {
        for sample in &buffer.samples {
            let mut pending = *sample;
            loop {
                match self.queue.push(pending) {
                    Ok(()) => break,
                    Err(returned) => {
                        pending = returned;
                        let _ = self.queue.pop();
                    }
                }
            }
        }
    }

    pub(crate) fn clear(&self) {
        while self.queue.pop().is_some() {}
    }

    pub(crate) fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }

    pub(crate) fn set_active(&self, active: bool) {
        let was_active = self.active.swap(active, Ordering::AcqRel);
        if active && !was_active {
            let elapsed_ns =
                self.telemetry.origin.elapsed().as_nanos().min(u64::MAX as u128) as u64;
            self.activation_consumed_frames.store(
                self.telemetry.callback_consumed_frames.load(Ordering::Acquire),
                Ordering::Release,
            );
            self.activation_elapsed_ns.store(elapsed_ns, Ordering::Release);
        }
    }

    pub(crate) fn buffered_frames(&self) -> usize {
        self.queue.len() / self.channels.max(1) as usize
    }

    pub(crate) fn snapshot(&self) -> RealtimeAudioOutputSnapshot {
        let last_elapsed_ns = self.telemetry.last_callback_elapsed_ns.load(Ordering::Acquire);
        let now_ns = self.telemetry.origin.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        let callback_consumed_frames =
            self.telemetry.callback_consumed_frames.load(Ordering::Acquire);
        let active = self.active.load(Ordering::Acquire);
        RealtimeAudioOutputSnapshot {
            stream_generation: self.telemetry.stream_generation,
            sample_rate: self.sample_rate,
            channels: self.channels,
            callback_consumed_frames,
            active_callback_consumed_frames: callback_consumed_frames
                .saturating_sub(self.activation_consumed_frames.load(Ordering::Acquire)),
            active_duration: active.then(|| {
                Duration::from_nanos(
                    now_ns.saturating_sub(self.activation_elapsed_ns.load(Ordering::Acquire)),
                )
            }),
            callback_count: self.telemetry.callback_count.load(Ordering::Relaxed),
            underrun_frames: self.telemetry.underrun_frames.load(Ordering::Relaxed),
            last_callback_frames: self
                .telemetry
                .last_callback_frames
                .load(Ordering::Relaxed)
                .min(u32::MAX as u64) as u32,
            last_callback_age: (last_elapsed_ns > 0)
                .then(|| Duration::from_nanos(now_ns.saturating_sub(last_elapsed_ns))),
            buffered_frames: self.buffered_frames(),
            stream_failed: self.telemetry.stream_failed.load(Ordering::Acquire),
            active,
        }
    }
}

impl AudioSourceCache {
    pub fn new(sample_rate: u32, channels: u8) -> Self {
        Self {
            sample_rate,
            channels: channels.max(1),
            decoded: Mutex::new(HashMap::new()),
        }
    }

    pub fn get_or_decode(&self, path: &Path) -> Result<Arc<AudioBuffer>> {
        if let Some(hit) = self.decoded.lock().get(path).cloned() {
            return Ok(hit);
        }

        let decoded = Arc::new(decode_audio_file_with_ffmpeg_cli(
            path,
            self.sample_rate,
            self.channels,
        )?);

        self.decoded.lock().insert(path.to_path_buf(), Arc::clone(&decoded));
        Ok(decoded)
    }

    pub fn cache_entry_count(&self) -> usize {
        self.decoded.lock().len()
    }

    pub fn clear(&self) {
        self.decoded.lock().clear();
    }
}

fn build_f32_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    queue: Arc<ArrayQueue<f32>>,
    muted: Arc<AtomicBool>,
    active: Arc<AtomicBool>,
    telemetry: Arc<RealtimeAudioOutputTelemetry>,
    err_fn: impl FnMut(cpal::StreamError) + Send + 'static,
) -> std::result::Result<cpal::Stream, cpal::BuildStreamError> {
    let channels = config.channels.max(1) as usize;
    device.build_output_stream(
        config,
        move |data: &mut [f32], _| {
            let frames = data.len() / channels;
            if muted.load(Ordering::Relaxed) || !active.load(Ordering::Relaxed) {
                data.fill(0.0);
                telemetry.record_callback(frames, 0);
                return;
            }
            let mut missing_samples = 0usize;
            for s in data {
                *s = queue.pop().unwrap_or_else(|| {
                    missing_samples = missing_samples.saturating_add(1);
                    0.0
                });
            }
            telemetry.record_callback(frames, missing_samples / channels);
        },
        err_fn,
        None,
    )
}

pub fn decode_audio_file_with_ffmpeg_cli(
    path: &Path,
    sample_rate: u32,
    channels: u8,
) -> Result<AudioBuffer> {
    let output = Command::new("ffmpeg")
        .arg("-v")
        .arg("error")
        .arg("-i")
        .arg(path)
        .arg("-vn")
        .arg("-sn")
        .arg("-dn")
        .arg("-f")
        .arg("f32le")
        .arg("-ac")
        .arg(channels.max(1).to_string())
        .arg("-ar")
        .arg(sample_rate.max(8_000).to_string())
        .arg("pipe:1")
        .output()
        .map_err(|e| MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: format!("调用 ffmpeg 失败: {e}"),
        })?;

    if !output.status.success() {
        return Err(MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: format!(
                "ffmpeg 解码失败: {}",
                String::from_utf8_lossy(&output.stderr)
            ),
        });
    }

    let mut samples = Vec::with_capacity(output.stdout.len() / 4);
    for chunk in output.stdout.chunks_exact(4) {
        samples.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }

    Ok(AudioBuffer {
        samples,
        sample_rate: sample_rate.max(8_000),
        channels: channels.max(1),
    })
}

/// 单轨音频数据
pub struct AudioTrackData {
    pub buffer: AudioBuffer,
    pub config: AudioTrackConfig,
}

/// 轨道配置（用于 serde 持久化）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioTrackConfig {
    pub volume: f32,
    pub pan: f32, // -1.0 左声道 ~ 1.0 右声道
    pub is_muted: bool,
    pub is_solo: bool,
}

impl Default for AudioTrackConfig {
    fn default() -> Self {
        Self {
            volume: 1.0,
            pan: 0.0,
            is_muted: false,
            is_solo: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn callback_telemetry_accumulates_consumption_and_underrun_without_locking() {
        let telemetry = RealtimeAudioOutputTelemetry::new();

        telemetry.record_callback(480, 0);
        telemetry.record_callback(480, 32);

        assert_eq!(
            telemetry.callback_consumed_frames.load(Ordering::Relaxed),
            960
        );
        assert_eq!(telemetry.callback_count.load(Ordering::Relaxed), 2);
        assert_eq!(telemetry.underrun_frames.load(Ordering::Relaxed), 32);
        assert_eq!(telemetry.last_callback_frames.load(Ordering::Relaxed), 480);
    }
}

#[cfg(test)]
mod perf_tests {
    use super::*;
    use serde::Serialize;
    use std::cmp;
    use std::f32::consts::PI;
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock};
    use std::time::Instant;

    #[derive(Debug, Serialize)]
    struct AudioMixPerfSimReport {
        scenario: &'static str,
        sample_rate: u32,
        channels: u8,
        tracks: usize,
        chunk_frames: usize,
        iterations: usize,
        first_chunk_ms: u128,
        first_chunk_threshold_ms: u128,
        chunk_ms_avg: f64,
        chunk_ms_p50: u128,
        chunk_ms_p95: u128,
        chunk_ms_max: u128,
        realtime_factor: f64,
        realtime_factor_min_threshold: f64,
        passed: bool,
    }

    fn perf_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn env_usize(key: &str, default: usize) -> usize {
        std::env::var(key)
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(default)
    }

    fn env_u128(key: &str, default: u128) -> u128 {
        std::env::var(key)
            .ok()
            .and_then(|v| v.trim().parse::<u128>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(default)
    }

    fn env_f64(key: &str, default: f64) -> f64 {
        std::env::var(key)
            .ok()
            .and_then(|v| v.trim().parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or(default)
    }

    fn report_output_path() -> Option<PathBuf> {
        std::env::var_os("MONDRIAN_AUDIO_SIM_OUTPUT").map(PathBuf::from)
    }

    fn write_report_if_needed(report_json: &str) {
        if let Some(path) = report_output_path() {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }

            if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
                let _ = writeln!(file, "{report_json}");
            }
        }
    }

    fn percentile_ms(values: &[u128], percentile: f64) -> u128 {
        if values.is_empty() {
            return 0;
        }
        let mut sorted = values.to_vec();
        sorted.sort_unstable();
        let p = percentile.clamp(0.0, 1.0);
        let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
        sorted[idx]
    }

    fn generate_sine_track(
        sample_rate: u32,
        channels: u8,
        frames: usize,
        freq_hz: f32,
        phase: f32,
    ) -> AudioBuffer {
        let channels = channels.max(1);
        let mut samples = vec![0.0f32; frames * channels as usize];
        for frame in 0..frames {
            let t = frame as f32 / sample_rate.max(1) as f32;
            let amp = ((2.0 * PI * freq_hz * t) + phase).sin() * 0.45;
            let base = frame * channels as usize;
            if channels >= 2 {
                samples[base] = amp;
                samples[base + 1] = amp * 0.92;
            } else {
                samples[base] = amp;
            }
        }
        AudioBuffer { samples, sample_rate, channels }
    }

    fn run_audio_mix_simulation(
        scenario: &'static str,
        sample_rate: u32,
        channels: u8,
        track_count: usize,
        chunk_frames: usize,
        iterations: usize,
        first_chunk_threshold_ms: u128,
        realtime_factor_min_threshold: f64,
    ) -> anyhow::Result<AudioMixPerfSimReport> {
        let sample_rate = sample_rate.max(8_000);
        let channels = channels.max(1);
        let track_count = track_count.max(1);
        let chunk_frames = chunk_frames.max(64);
        let iterations = iterations.max(1);
        let mixer = AudioMixer::new(sample_rate, channels);

        let mut tracks = Vec::with_capacity(track_count);
        for idx in 0..track_count {
            let freq = 220.0 + idx as f32 * 13.0;
            let phase = idx as f32 * 0.37;
            let buffer = generate_sine_track(sample_rate, channels, chunk_frames, freq, phase);
            tracks.push(AudioTrackData {
                buffer,
                config: AudioTrackConfig {
                    volume: (0.92f32 - idx as f32 * 0.01).max(0.35),
                    pan: (((idx % 9) as f32) / 4.0 - 1.0).clamp(-1.0, 1.0),
                    is_muted: false,
                    is_solo: false,
                },
            });
        }

        let first_started = Instant::now();
        let _ = mixer.mix(&tracks);
        let first_chunk_ms = first_started.elapsed().as_millis();

        let mut chunk_samples_ms = Vec::with_capacity(iterations);
        let loop_started = Instant::now();
        for _ in 0..iterations {
            let started = Instant::now();
            let _mixed = mixer.mix(&tracks);
            chunk_samples_ms.push(started.elapsed().as_millis());
        }
        let elapsed_secs = loop_started.elapsed().as_secs_f64();

        let total_ms = chunk_samples_ms.iter().copied().sum::<u128>();
        let chunk_ms_avg = total_ms as f64 / cmp::max(chunk_samples_ms.len(), 1) as f64;
        let chunk_ms_p50 = percentile_ms(&chunk_samples_ms, 0.50);
        let chunk_ms_p95 = percentile_ms(&chunk_samples_ms, 0.95);
        let chunk_ms_max = chunk_samples_ms.iter().copied().max().unwrap_or(0);

        let simulated_audio_secs = iterations as f64 * (chunk_frames as f64 / sample_rate as f64);
        let realtime_factor = if elapsed_secs > 0.0 {
            simulated_audio_secs / elapsed_secs
        } else {
            0.0
        };

        let passed = first_chunk_ms <= first_chunk_threshold_ms
            && realtime_factor >= realtime_factor_min_threshold;

        Ok(AudioMixPerfSimReport {
            scenario,
            sample_rate,
            channels,
            tracks: track_count,
            chunk_frames,
            iterations,
            first_chunk_ms,
            first_chunk_threshold_ms,
            chunk_ms_avg,
            chunk_ms_p50,
            chunk_ms_p95,
            chunk_ms_max,
            realtime_factor,
            realtime_factor_min_threshold,
            passed,
        })
    }

    #[test]
    #[ignore = "development audio mix perf simulation test; run manually"]
    fn audio_mix_48k_stereo_simulated_perf() -> anyhow::Result<()> {
        let _guard = perf_lock().lock().expect("audio perf lock poisoned");

        let sample_rate =
            env_usize("MONDRIAN_AUDIO_SIM_SAMPLE_RATE", 48_000).clamp(8_000, 192_000) as u32;
        let channels = env_usize("MONDRIAN_AUDIO_SIM_CHANNELS", 2).clamp(1, 2) as u8;
        let tracks = env_usize("MONDRIAN_AUDIO_SIM_TRACKS", 12).clamp(1, 64);
        let chunk_frames = env_usize("MONDRIAN_AUDIO_SIM_CHUNK_FRAMES", 3_840).clamp(64, 96_000);
        let iterations = env_usize("MONDRIAN_AUDIO_SIM_ITERATIONS", 280).clamp(20, 6_000);

        let first_chunk_threshold_ms = env_u128("MONDRIAN_AUDIO_SIM_TTFF_MS", 120);
        let realtime_factor_min_threshold = env_f64("MONDRIAN_AUDIO_SIM_RTF_MIN", 8.0);

        let report = run_audio_mix_simulation(
            "audio-mix-48k-stereo-simulated",
            sample_rate,
            channels,
            tracks,
            chunk_frames,
            iterations,
            first_chunk_threshold_ms,
            realtime_factor_min_threshold,
        )?;

        let report_json = serde_json::to_string(&report)?;
        eprintln!("MONDRIAN_AUDIO_SIM_JSON={report_json}");
        write_report_if_needed(&report_json);

        if !report.passed {
            anyhow::bail!(
                "audio mix simulation perf test failed; report: {}",
                report_json
            );
        }

        Ok(())
    }
}
