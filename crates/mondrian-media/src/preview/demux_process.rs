//! Recoverable parent-side boundary for one Preview demux session.
//!
//! The helper owns one source and every `AVFormatContext` operation. The
//! parent retains codec and GPU state, pulls at most one packet per command,
//! and can recover a blocked format call by terminating and reaping exactly
//! this process.

use super::demux_protocol::{
    read_message, read_protocol_preamble, write_worker_command, write_worker_request,
    DemuxOpenPhase, DemuxProtocolMessage, DemuxStreamContract, DemuxWorkerCommand,
};
use super::execution_progress::{
    PreviewDecodeExecutionObserver, PreviewIsolatedDemuxSessionEvidence,
    PreviewIsolatedDemuxTermination,
};
use super::MediaFileFingerprint;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const IPC_POLL_INTERVAL: Duration = Duration::from_micros(250);
const IPC_QUEUE_CAPACITY: usize = 1;
const MAX_STDERR_EVIDENCE_BYTES: usize = 64 * 1024;
const CLEAN_CLOSE_GRACE: Duration = Duration::from_millis(100);

static NEXT_LAUNCH_NONCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone)]
pub(super) struct PreviewDemuxWorkerConfig {
    executable: PathBuf,
    execution_observer: PreviewDecodeExecutionObserver,
}

impl PreviewDemuxWorkerConfig {
    pub(super) fn new(
        executable: PathBuf,
        execution_observer: PreviewDecodeExecutionObserver,
    ) -> Self {
        Self { executable, execution_observer }
    }
}

pub(super) struct IsolatedDemuxOpen {
    pub source: IsolatedDemuxSession,
    pub stream: DemuxStreamContract,
}

pub(super) enum IsolatedDemuxSeek {
    Complete,
    Canceled,
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

/// A reusable, one-source demux process with one command in flight.
pub(super) struct IsolatedDemuxSession {
    child: Child,
    stdin: Option<ChildStdin>,
    messages: Option<Receiver<io::Result<DemuxProtocolMessage>>>,
    protocol_reader: Option<JoinHandle<()>>,
    stderr_reader: Option<JoinHandle<Vec<u8>>>,
    lifecycle: PreviewIsolatedDemuxSessionEvidence,
    next_command_id: u64,
    poisoned: bool,
}

impl IsolatedDemuxSession {
    pub(super) fn open(
        config: &PreviewDemuxWorkerConfig,
        path: &Path,
        source_revision: MediaFileFingerprint,
        should_cancel: &(dyn Fn() -> bool + Send + Sync),
        on_open_phase: &mut dyn FnMut(DemuxOpenPhase),
    ) -> Result<IsolatedDemuxOpen, IsolatedDemuxOpenError> {
        if should_cancel() {
            return Err(IsolatedDemuxOpenError::Canceled);
        }
        let nonce = launch_nonce();
        let mut command = Command::new(&config.executable);
        command
            .arg("--internal-demux-worker-v2")
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
        let mut lifecycle = config.execution_observer.begin_isolated_demux_session();

        let Some(mut stdin) = child.stdin.take() else {
            terminate_child(&mut child);
            lifecycle.settle(PreviewIsolatedDemuxTermination::Failed);
            return Err(IsolatedDemuxOpenError::Failed(
                "Preview demux worker stdin was not piped".to_owned(),
            ));
        };
        if let Err(error) = write_worker_request(&mut stdin, nonce, path, source_revision)
            .and_then(|()| stdin.flush())
        {
            terminate_child(&mut child);
            lifecycle.settle(PreviewIsolatedDemuxTermination::Failed);
            return Err(IsolatedDemuxOpenError::Failed(format!(
                "send Preview demux worker request: {error}"
            )));
        }

        let Some(stdout) = child.stdout.take() else {
            terminate_child(&mut child);
            lifecycle.settle(PreviewIsolatedDemuxTermination::Failed);
            return Err(IsolatedDemuxOpenError::Failed(
                "Preview demux worker stdout was not piped".to_owned(),
            ));
        };
        let Some(stderr) = child.stderr.take() else {
            terminate_child(&mut child);
            lifecycle.settle(PreviewIsolatedDemuxTermination::Failed);
            return Err(IsolatedDemuxOpenError::Failed(
                "Preview demux worker stderr was not piped".to_owned(),
            ));
        };
        let (message_tx, message_rx) = mpsc::sync_channel(IPC_QUEUE_CAPACITY);
        let protocol_reader = match thread::Builder::new()
            .name("mondrian-preview-demux-ipc".to_owned())
            .spawn(move || read_protocol_stream(stdout, nonce, message_tx))
        {
            Ok(reader) => reader,
            Err(error) => {
                terminate_child(&mut child);
                lifecycle.settle(PreviewIsolatedDemuxTermination::Failed);
                return Err(IsolatedDemuxOpenError::Failed(format!(
                    "start Preview demux protocol reader: {error}"
                )));
            }
        };
        let stderr_reader = match thread::Builder::new()
            .name("mondrian-preview-demux-stderr".to_owned())
            .spawn(move || drain_bounded_stderr(stderr))
        {
            Ok(reader) => reader,
            Err(error) => {
                terminate_child(&mut child);
                let _ = protocol_reader.join();
                lifecycle.settle(PreviewIsolatedDemuxTermination::Failed);
                return Err(IsolatedDemuxOpenError::Failed(format!(
                    "start Preview demux stderr reader: {error}"
                )));
            }
        };

        let mut source = Self {
            child,
            stdin: Some(stdin),
            messages: Some(message_rx),
            protocol_reader: Some(protocol_reader),
            stderr_reader: Some(stderr_reader),
            lifecycle,
            next_command_id: 1,
            poisoned: false,
        };
        let mut open_phases_seen = 0_u8;
        let stream = loop {
            match source.wait_for_message(should_cancel) {
                Ok(DemuxProtocolMessage::OpenPhase(DemuxOpenPhase::InputOpen))
                    if open_phases_seen == 0 =>
                {
                    on_open_phase(DemuxOpenPhase::InputOpen);
                    open_phases_seen = 1;
                }
                Ok(DemuxProtocolMessage::OpenPhase(DemuxOpenPhase::StreamInfo))
                    if open_phases_seen == 1 =>
                {
                    on_open_phase(DemuxOpenPhase::StreamInfo);
                    open_phases_seen = 2;
                }
                Ok(DemuxProtocolMessage::Stream(stream)) if open_phases_seen == 2 => break stream,
                Ok(DemuxProtocolMessage::Error { command_id: 0, message }) => {
                    let message = source.failure_with_stderr(message);
                    return Err(IsolatedDemuxOpenError::Failed(message));
                }
                Ok(_) => {
                    source.terminate(PreviewIsolatedDemuxTermination::Failed);
                    return Err(IsolatedDemuxOpenError::Failed(
                        "Preview demux worker violated open-phase ordering".to_owned(),
                    ));
                }
                Err(IsolatedDemuxOpenError::Canceled) => {
                    source.terminate(PreviewIsolatedDemuxTermination::Canceled);
                    return Err(IsolatedDemuxOpenError::Canceled);
                }
                Err(error) => {
                    source.terminate(PreviewIsolatedDemuxTermination::Failed);
                    return Err(error);
                }
            }
        };
        source.lifecycle.record_ready();
        Ok(IsolatedDemuxOpen { source, stream })
    }

    pub(super) fn is_healthy(&self) -> bool {
        !self.poisoned
    }

    pub(super) fn seek(
        &mut self,
        min_ts: i64,
        target_ts: i64,
        max_ts: i64,
        flags: i32,
        should_cancel: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<IsolatedDemuxSeek, String> {
        let command_id = self.send_command(DemuxWorkerCommand::Seek {
            command_id: self.next_command_id,
            min_ts,
            target_ts,
            max_ts,
            flags,
        })?;
        match self.wait_for_message(should_cancel) {
            Ok(DemuxProtocolMessage::SeekComplete { command_id: observed })
                if observed == command_id =>
            {
                self.lifecycle.record_seek_complete();
                Ok(IsolatedDemuxSeek::Complete)
            }
            Ok(DemuxProtocolMessage::Error { command_id: observed, message })
                if observed == command_id =>
            {
                Err(self.poison_with_stderr(message))
            }
            Ok(message) => Err(self.poison_protocol_mismatch(command_id, &message)),
            Err(IsolatedDemuxOpenError::Canceled) => {
                self.terminate(PreviewIsolatedDemuxTermination::Canceled);
                Ok(IsolatedDemuxSeek::Canceled)
            }
            Err(IsolatedDemuxOpenError::Failed(message)) => {
                self.terminate(PreviewIsolatedDemuxTermination::Failed);
                Err(message)
            }
        }
    }

    pub(super) fn read_next(
        &mut self,
        should_cancel: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<IsolatedDemuxRead, String> {
        let command_id =
            self.send_command(DemuxWorkerCommand::Read { command_id: self.next_command_id })?;
        match self.wait_for_message(should_cancel) {
            Ok(DemuxProtocolMessage::Packet(packet)) if packet.command_id == command_id => {
                self.lifecycle.record_packet();
                Ok(IsolatedDemuxRead::Packet(packet.packet))
            }
            Ok(DemuxProtocolMessage::End { command_id: observed }) if observed == command_id => {
                self.lifecycle.record_end();
                Ok(IsolatedDemuxRead::End)
            }
            Ok(DemuxProtocolMessage::Error { command_id: observed, message })
                if observed == command_id =>
            {
                Err(self.poison_with_stderr(message))
            }
            Ok(message) => Err(self.poison_protocol_mismatch(command_id, &message)),
            Err(IsolatedDemuxOpenError::Canceled) => {
                self.terminate(PreviewIsolatedDemuxTermination::Canceled);
                Ok(IsolatedDemuxRead::Canceled)
            }
            Err(IsolatedDemuxOpenError::Failed(message)) => {
                self.terminate(PreviewIsolatedDemuxTermination::Failed);
                Err(message)
            }
        }
    }

    fn send_command(&mut self, command: DemuxWorkerCommand) -> Result<u64, String> {
        if self.poisoned {
            return Err("Preview demux session is no longer reusable".to_owned());
        }
        let command_id = command.command_id();
        if command_id != self.next_command_id {
            return Err(format!(
                "Preview demux command id {command_id} did not match expected {}",
                self.next_command_id
            ));
        }
        let result = self
            .stdin
            .as_mut()
            .ok_or_else(|| "Preview demux worker command pipe is closed".to_owned())
            .and_then(|stdin| {
                write_worker_command(stdin, command)
                    .and_then(|()| stdin.flush())
                    .map_err(|error| format!("send Preview demux command {command_id}: {error}"))
            });
        if let Err(message) = result {
            self.terminate(PreviewIsolatedDemuxTermination::Failed);
            return Err(message);
        }
        let Some(next_command_id) = self.next_command_id.checked_add(1) else {
            self.terminate(PreviewIsolatedDemuxTermination::Failed);
            return Err("Preview demux command identifier overflow".to_owned());
        };
        self.next_command_id = next_command_id;
        Ok(command_id)
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
                    )))
                }
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    let status = self.child.try_wait().ok().flatten();
                    return Err(IsolatedDemuxOpenError::Failed(format!(
                        "Preview demux worker exited before a response{}",
                        status.map_or_else(String::new, |status| format!(" ({status})"))
                    )));
                }
            }
        }
    }

    fn poison_protocol_mismatch(
        &mut self,
        command_id: u64,
        message: &DemuxProtocolMessage,
    ) -> String {
        let observed = message_command_id(message)
            .map_or_else(|| "non-command response".to_owned(), |id| id.to_string());
        self.terminate(PreviewIsolatedDemuxTermination::Failed);
        format!("Preview demux response {observed} did not match command {command_id}")
    }

    fn poison_with_stderr(&mut self, message: String) -> String {
        self.poisoned = true;
        self.failure_with_stderr(message)
    }

    fn failure_with_stderr(&mut self, message: String) -> String {
        self.reap_child_bounded();
        self.stdin.take();
        self.messages.take();
        if let Some(reader) = self.protocol_reader.take() {
            let _ = reader.join();
        }
        let stderr = self
            .stderr_reader
            .take()
            .and_then(|reader| reader.join().ok())
            .unwrap_or_default();
        self.lifecycle.settle(PreviewIsolatedDemuxTermination::Failed);
        if stderr.is_empty() {
            message
        } else {
            format!("{message}; stderr: {}", String::from_utf8_lossy(&stderr))
        }
    }

    fn close(&mut self) {
        if self.poisoned {
            self.terminate(PreviewIsolatedDemuxTermination::Failed);
            return;
        }
        let command_id = self.next_command_id;
        let sent = self.send_command(DemuxWorkerCommand::Close { command_id }).is_ok();
        if sent {
            let deadline = Instant::now() + CLEAN_CLOSE_GRACE;
            while Instant::now() < deadline {
                let Some(messages) = self.messages.as_ref() else {
                    break;
                };
                match messages.recv_timeout(IPC_POLL_INTERVAL) {
                    Ok(Ok(DemuxProtocolMessage::Closed { command_id: observed }))
                        if observed == command_id =>
                    {
                        let exited_cleanly = self.reap_child_bounded();
                        self.stdin.take();
                        self.messages.take();
                        if let Some(reader) = self.protocol_reader.take() {
                            let _ = reader.join();
                        }
                        if let Some(reader) = self.stderr_reader.take() {
                            let _ = reader.join();
                        }
                        self.lifecycle.settle(if exited_cleanly {
                            PreviewIsolatedDemuxTermination::CleanClose
                        } else {
                            PreviewIsolatedDemuxTermination::ForcedClose
                        });
                        self.poisoned = true;
                        return;
                    }
                    Ok(_) | Err(RecvTimeoutError::Disconnected) => break,
                    Err(RecvTimeoutError::Timeout) => {}
                }
            }
        }
        self.terminate(PreviewIsolatedDemuxTermination::ForcedClose);
    }

    fn terminate(&mut self, termination: PreviewIsolatedDemuxTermination) {
        self.poisoned = true;
        self.stdin.take();
        self.messages.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.protocol_reader.take() {
            let _ = reader.join();
        }
        if let Some(reader) = self.stderr_reader.take() {
            let _ = reader.join();
        }
        self.lifecycle.settle(termination);
    }

    fn reap_child_bounded(&mut self) -> bool {
        let started = Instant::now();
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return true,
                Ok(None) if started.elapsed() < CLEAN_CLOSE_GRACE => {
                    thread::sleep(IPC_POLL_INTERVAL);
                }
                Ok(None) | Err(_) => break,
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        false
    }
}

impl Drop for IsolatedDemuxSession {
    fn drop(&mut self) {
        self.close();
    }
}

fn message_command_id(message: &DemuxProtocolMessage) -> Option<u64> {
    match message {
        DemuxProtocolMessage::OpenPhase(_) | DemuxProtocolMessage::Stream(_) => None,
        DemuxProtocolMessage::SeekComplete { command_id }
        | DemuxProtocolMessage::End { command_id }
        | DemuxProtocolMessage::Closed { command_id }
        | DemuxProtocolMessage::Error { command_id, .. } => Some(*command_id),
        DemuxProtocolMessage::Packet(packet) => Some(packet.command_id),
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
            Ok(DemuxProtocolMessage::OpenPhase(_)) if !stream_seen => {}
            Ok(DemuxProtocolMessage::Error { command_id: 0, .. }) if !stream_seen => {
                let _ = messages.send(message);
                return;
            }
            Ok(_) if !stream_seen => {
                let _ = messages.send(Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Preview demux response preceded its stream contract",
                )));
                return;
            }
            Ok(DemuxProtocolMessage::Closed { .. } | DemuxProtocolMessage::Error { .. }) => {
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
