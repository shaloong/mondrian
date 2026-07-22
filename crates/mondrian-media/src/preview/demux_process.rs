//! Recoverable parent-side boundary for one isolated Preview demux request.
//!
//! FFmpeg format work is allowed to block only in the child process. The
//! parent keeps codec and GPU state local, applies bounded IPC validation, and
//! owns cancellation by terminating and reaping the child.

use super::demux_protocol::{
    read_message, read_protocol_preamble, write_worker_request, DemuxProtocolMessage,
    DemuxStreamContract,
};
use mondrian_core::TimelineTime;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const IPC_POLL_INTERVAL: Duration = Duration::from_micros(250);
const IPC_QUEUE_CAPACITY: usize = 4;
const MAX_STDERR_EVIDENCE_BYTES: usize = 64 * 1024;
const CLEAN_REAP_GRACE: Duration = Duration::from_millis(10);

static NEXT_LAUNCH_NONCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PreviewDemuxWorkerConfig {
    executable: PathBuf,
}

impl PreviewDemuxWorkerConfig {
    pub(super) fn new(executable: PathBuf) -> Self {
        Self { executable }
    }
}

pub(super) struct IsolatedDemuxOpen {
    pub source: IsolatedDemuxRequest,
    pub stream: DemuxStreamContract,
}

pub(super) enum IsolatedDemuxRead {
    Packet(ffmpeg_next::Packet),
    End,
    Canceled,
}

pub(super) enum IsolatedDemuxOpenError {
    Canceled,
    Failed(String),
}

pub(super) struct IsolatedDemuxRequest {
    child: Child,
    messages: Option<Receiver<io::Result<DemuxProtocolMessage>>>,
    protocol_reader: Option<JoinHandle<()>>,
    stderr_reader: Option<JoinHandle<Vec<u8>>>,
    terminal: bool,
    canceled: bool,
    seek_target_pts: i64,
}

impl IsolatedDemuxRequest {
    pub(super) fn open(
        config: &PreviewDemuxWorkerConfig,
        path: &Path,
        source_time: TimelineTime,
        should_cancel: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<IsolatedDemuxOpen, IsolatedDemuxOpenError> {
        let nonce = launch_nonce();
        let mut command = Command::new(&config.executable);
        command
            .arg("--internal-demux-worker-v1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        configure_hidden_child(&mut command);
        let mut child = command.spawn().map_err(|error| {
            IsolatedDemuxOpenError::Failed(format!(
                "start Preview demux worker {}: {error}",
                config.executable.display()
            ))
        })?;

        let Some(mut stdin) = child.stdin.take() else {
            terminate_child(&mut child);
            return Err(IsolatedDemuxOpenError::Failed(
                "Preview demux worker stdin was not piped".to_owned(),
            ));
        };
        if let Err(error) =
            write_worker_request(&mut stdin, nonce, path, source_time).and_then(|()| stdin.flush())
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(IsolatedDemuxOpenError::Failed(format!(
                "send Preview demux worker request: {error}"
            )));
        }
        drop(stdin);

        let Some(stdout) = child.stdout.take() else {
            terminate_child(&mut child);
            return Err(IsolatedDemuxOpenError::Failed(
                "Preview demux worker stdout was not piped".to_owned(),
            ));
        };
        let Some(stderr) = child.stderr.take() else {
            terminate_child(&mut child);
            return Err(IsolatedDemuxOpenError::Failed(
                "Preview demux worker stderr was not piped".to_owned(),
            ));
        };
        let (message_tx, message_rx) = mpsc::sync_channel(IPC_QUEUE_CAPACITY);
        let protocol_reader = thread::Builder::new()
            .name("mondrian-preview-demux-ipc".to_owned())
            .spawn(move || read_protocol_stream(stdout, nonce, message_tx))
            .map_err(|error| {
                let _ = child.kill();
                let _ = child.wait();
                IsolatedDemuxOpenError::Failed(format!(
                    "start Preview demux protocol reader: {error}"
                ))
            })?;
        let stderr_reader = match thread::Builder::new()
            .name("mondrian-preview-demux-stderr".to_owned())
            .spawn(move || drain_bounded_stderr(stderr))
        {
            Ok(reader) => reader,
            Err(error) => {
                terminate_child(&mut child);
                let _ = protocol_reader.join();
                return Err(IsolatedDemuxOpenError::Failed(format!(
                    "start Preview demux stderr reader: {error}"
                )));
            }
        };

        let mut source = Self {
            child,
            messages: Some(message_rx),
            protocol_reader: Some(protocol_reader),
            stderr_reader: Some(stderr_reader),
            terminal: false,
            canceled: false,
            seek_target_pts: 0,
        };
        let first = match source.wait_for_message(should_cancel) {
            Ok(message) => message,
            Err(error) => {
                source.terminate();
                return Err(error);
            }
        };
        let stream = match first {
            DemuxProtocolMessage::Stream(stream) => stream,
            DemuxProtocolMessage::Error(message) => {
                let message = source.failure_with_stderr(message);
                source.terminate();
                return Err(IsolatedDemuxOpenError::Failed(message));
            }
            _ => {
                source.terminate();
                return Err(IsolatedDemuxOpenError::Failed(
                    "Preview demux worker did not begin with exactly one stream contract"
                        .to_owned(),
                ));
            }
        };
        source.seek_target_pts = stream.seek_target_pts;
        Ok(IsolatedDemuxOpen { source, stream })
    }

    pub(super) fn seek_target_pts(&self) -> i64 {
        self.seek_target_pts
    }

    pub(super) fn read_next(
        &mut self,
        should_cancel: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<IsolatedDemuxRead, String> {
        if self.terminal {
            return Ok(IsolatedDemuxRead::End);
        }
        match self.wait_for_message(should_cancel) {
            Ok(DemuxProtocolMessage::Packet(packet)) => {
                Ok(IsolatedDemuxRead::Packet(packet.packet))
            }
            Ok(DemuxProtocolMessage::End) => {
                self.terminal = true;
                self.reap_completed();
                Ok(IsolatedDemuxRead::End)
            }
            Ok(DemuxProtocolMessage::Error(message)) => {
                self.terminal = true;
                let message = self.failure_with_stderr(message);
                Err(message)
            }
            Ok(DemuxProtocolMessage::Stream(_)) => {
                Err("Preview demux worker emitted a duplicate stream contract".to_owned())
            }
            Err(IsolatedDemuxOpenError::Canceled) => {
                self.terminal = true;
                self.canceled = true;
                self.terminate();
                Ok(IsolatedDemuxRead::Canceled)
            }
            Err(IsolatedDemuxOpenError::Failed(message)) => {
                self.terminal = true;
                self.terminate();
                Err(message)
            }
        }
    }

    pub(super) fn is_terminal(&self) -> bool {
        self.terminal
    }

    pub(super) fn was_canceled(&self) -> bool {
        self.canceled
    }

    pub(super) fn finish_one_shot(&mut self) {
        if !self.terminal {
            self.terminal = true;
            self.terminate();
        }
    }

    fn wait_for_message(
        &mut self,
        should_cancel: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<DemuxProtocolMessage, IsolatedDemuxOpenError> {
        loop {
            if should_cancel() {
                return Err(IsolatedDemuxOpenError::Canceled);
            }
            let Some(messages) = self.messages.as_ref() else {
                return Err(IsolatedDemuxOpenError::Failed(
                    "Preview demux worker response channel is closed".to_owned(),
                ));
            };
            match messages.recv_timeout(IPC_POLL_INTERVAL) {
                Ok(Ok(message)) => return Ok(message),
                Ok(Err(error)) => {
                    return Err(IsolatedDemuxOpenError::Failed(format!(
                        "Preview demux protocol failed: {error}"
                    )));
                }
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    let status = self.child.try_wait().ok().flatten();
                    return Err(IsolatedDemuxOpenError::Failed(format!(
                        "Preview demux worker exited before a terminal message{}",
                        status.map_or_else(String::new, |status| format!(" ({status})"))
                    )));
                }
            }
        }
    }

    fn failure_with_stderr(&mut self, message: String) -> String {
        self.reap_child_bounded();
        self.messages.take();
        if let Some(reader) = self.protocol_reader.take() {
            let _ = reader.join();
        }
        let stderr = self
            .stderr_reader
            .take()
            .and_then(|reader| reader.join().ok())
            .unwrap_or_default();
        if stderr.is_empty() {
            message
        } else {
            format!("{message}; stderr: {}", String::from_utf8_lossy(&stderr))
        }
    }

    fn reap_completed(&mut self) {
        self.reap_child_bounded();
        self.messages.take();
        if let Some(reader) = self.protocol_reader.take() {
            let _ = reader.join();
        }
        if let Some(reader) = self.stderr_reader.take() {
            let _ = reader.join();
        }
    }

    fn terminate(&mut self) {
        self.messages.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.protocol_reader.take() {
            let _ = reader.join();
        }
        if let Some(reader) = self.stderr_reader.take() {
            let _ = reader.join();
        }
    }

    fn reap_child_bounded(&mut self) {
        let started = std::time::Instant::now();
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if started.elapsed() < CLEAN_REAP_GRACE => {
                    thread::sleep(IPC_POLL_INTERVAL);
                }
                Ok(None) | Err(_) => break,
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for IsolatedDemuxRequest {
    fn drop(&mut self) {
        self.terminate();
    }
}

fn read_protocol_stream(
    mut stdout: impl Read,
    nonce: [u8; 16],
    messages: SyncSender<io::Result<DemuxProtocolMessage>>,
) {
    if let Err(error) = read_protocol_preamble(&mut stdout, nonce) {
        let _ = messages.send(Err(error));
        return;
    }
    let mut stream_seen = false;
    loop {
        let message = read_message(&mut stdout);
        match &message {
            Ok(DemuxProtocolMessage::Stream(_)) if stream_seen => {
                let _ = messages.send(Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "duplicate Preview demux stream contract",
                )));
                return;
            }
            Ok(DemuxProtocolMessage::Stream(_)) => stream_seen = true,
            Ok(DemuxProtocolMessage::Packet(_) | DemuxProtocolMessage::End) if !stream_seen => {
                let _ = messages.send(Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Preview demux payload preceded its stream contract",
                )));
                return;
            }
            Ok(DemuxProtocolMessage::End | DemuxProtocolMessage::Error(_)) => {
                let _ = messages.send(message);
                return;
            }
            Err(_) => {
                let _ = messages.send(message);
                return;
            }
            _ => {}
        }
        if messages.send(message).is_err() {
            return;
        }
    }
}

fn drain_bounded_stderr(mut stderr: impl Read) -> Vec<u8> {
    let mut evidence = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        match stderr.read(&mut chunk) {
            Ok(0) | Err(_) => return evidence,
            Ok(read) => {
                let remaining = MAX_STDERR_EVIDENCE_BYTES.saturating_sub(evidence.len());
                evidence.extend_from_slice(&chunk[..read.min(remaining)]);
            }
        }
    }
}

fn launch_nonce() -> [u8; 16] {
    let sequence = NEXT_LAUNCH_NONCE.fetch_add(1, Ordering::Relaxed);
    let clock = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos() as u64;
    let mut nonce = [0_u8; 16];
    nonce[..8].copy_from_slice(&sequence.to_le_bytes());
    nonce[8..].copy_from_slice(&(clock ^ u64::from(std::process::id())).to_le_bytes());
    nonce
}

fn terminate_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(windows)]
fn configure_hidden_child(command: &mut Command) {
    use std::os::windows::process::CommandExt as _;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn configure_hidden_child(_command: &mut Command) {}
