//! Bounded, cancellable ownership of external media processes.
//!
//! This is the only general-purpose `std::process::Child` lifecycle used by
//! FFmpeg/FFprobe CLI adapters. Pipe drains start immediately after spawn,
//! retained output is bounded, stdin is written on an owned pump thread, and
//! every terminal path kills when necessary and reaps the child.

use mondrian_core::ExecutionCancellationToken;
use std::collections::VecDeque;
use std::fmt;
use std::io::{self, BufWriter, Read, Write};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use thiserror::Error;

const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(2);
const DEFAULT_STDIN_CHUNK_BYTES: usize = 64 * 1024;

/// External-process lifecycle stage at which execution terminated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisedProcessStage {
    /// The operating-system process was being created.
    Spawn,
    /// A bounded stdin chunk was being delivered.
    StdinWrite,
    /// The stdin stream was being closed to publish EOF.
    StdinClose,
    /// The parent was waiting for process termination.
    Wait,
    /// The stdout drain was being settled.
    StdoutDrain,
    /// The stderr drain was being settled.
    StderrDrain,
}

impl fmt::Display for SupervisedProcessStage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Spawn => "spawn",
            Self::StdinWrite => "stdin-write",
            Self::StdinClose => "stdin-close",
            Self::Wait => "wait",
            Self::StdoutDrain => "stdout-drain",
            Self::StderrDrain => "stderr-drain",
        })
    }
}

/// Captured child stream whose configured memory limit was exceeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisedProcessStream {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
}

impl fmt::Display for SupervisedProcessStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        })
    }
}

/// Typed terminal error from an externally supervised media process.
#[derive(Debug, Error)]
pub enum SupervisedProcessError {
    /// The owning execution generation was invalidated.
    #[error("external media process canceled during {stage}")]
    Canceled {
        /// Stage that observed cancellation.
        stage: SupervisedProcessStage,
    },
    /// The process exceeded its admitted monotonic deadline.
    #[error("external media process deadline exceeded during {stage}")]
    DeadlineExceeded {
        /// Stage that observed the elapsed deadline.
        stage: SupervisedProcessStage,
    },
    /// A strict retained-output cap was exceeded.
    #[error(
        "external media process {stream} exceeded its {limit_bytes}-byte output limit during {stage}"
    )]
    OutputLimitExceeded {
        /// Drain stage that observed the excess.
        stage: SupervisedProcessStage,
        /// Stream that exceeded its cap.
        stream: SupervisedProcessStream,
        /// Configured maximum retained bytes.
        limit_bytes: usize,
    },
    /// A required pipe was not available after spawn.
    #[error("external media process did not expose required {stream} pipe")]
    MissingPipe {
        /// Missing pipe name.
        stream: &'static str,
    },
    /// A process or pipe operation failed.
    #[error("external media process I/O failed during {stage}: {source}")]
    Io {
        /// Failing lifecycle stage.
        stage: SupervisedProcessStage,
        /// Underlying operating-system error.
        #[source]
        source: io::Error,
    },
    /// A bounded pipe or stdin worker panicked.
    #[error("external media process worker panicked during {stage}")]
    WorkerPanicked {
        /// Worker lifecycle stage.
        stage: SupervisedProcessStage,
    },
}

impl SupervisedProcessError {
    /// Return whether this terminal outcome was caused by cancellation.
    pub const fn is_canceled(&self) -> bool {
        matches!(self, Self::Canceled { .. })
    }

    /// Return whether this terminal outcome was caused by an elapsed deadline.
    pub const fn is_deadline_exceeded(&self) -> bool {
        matches!(self, Self::DeadlineExceeded { .. })
    }
}

/// Bounded capture policy for one child output stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisedStreamCapture {
    /// Drain all bytes without retaining them.
    Drain,
    /// Retain the first `limit_bytes`; optionally fail closed on any excess.
    Head {
        /// Maximum number of bytes retained in memory.
        limit_bytes: usize,
        /// Whether producing additional bytes is a terminal process error.
        reject_excess: bool,
    },
    /// Retain only the latest `limit_bytes`, continuously draining earlier data.
    Tail {
        /// Maximum number of latest bytes retained in memory.
        limit_bytes: usize,
    },
}

impl SupervisedStreamCapture {
    fn strict_limit(self) -> Option<usize> {
        match self {
            Self::Head { limit_bytes, reject_excess: true } => Some(limit_bytes),
            Self::Drain | Self::Head { .. } | Self::Tail { .. } => None,
        }
    }
}

/// Admission policy for one external media process.
#[derive(Debug, Clone)]
pub struct SupervisedProcessPolicy {
    /// Whether the supervisor owns a piped stdin writer.
    pub pipe_stdin: bool,
    /// Bounded stdout capture policy.
    pub stdout: SupervisedStreamCapture,
    /// Bounded stderr capture policy.
    pub stderr: SupervisedStreamCapture,
    /// Absolute monotonic deadline for the whole child lifecycle.
    pub deadline: Option<Instant>,
    /// Maximum write syscall slice used by the stdin pump.
    pub stdin_chunk_bytes: usize,
    /// Parent-side cancellation/deadline observation cadence.
    pub poll_interval: Duration,
}

impl Default for SupervisedProcessPolicy {
    fn default() -> Self {
        Self {
            pipe_stdin: false,
            stdout: SupervisedStreamCapture::Head {
                limit_bytes: 16 * 1024 * 1024,
                reject_excess: true,
            },
            stderr: SupervisedStreamCapture::Tail { limit_bytes: 64 * 1024 },
            deadline: None,
            stdin_chunk_bytes: DEFAULT_STDIN_CHUNK_BYTES,
            poll_interval: DEFAULT_POLL_INTERVAL,
        }
    }
}

/// Bounded output and exit status from a fully reaped process.
#[derive(Debug)]
pub struct SupervisedProcessOutput {
    /// Child exit status.
    pub status: ExitStatus,
    /// Bounded stdout evidence.
    pub stdout: Vec<u8>,
    /// Bounded stderr evidence.
    pub stderr: Vec<u8>,
    /// Whether stdout produced more bytes than were retained.
    pub stdout_truncated: bool,
    /// Whether stderr produced more bytes than were retained.
    pub stderr_truncated: bool,
}

struct PipeDrain {
    handle: Option<JoinHandle<io::Result<CapturedPipe>>>,
    exceeded: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
    capture: SupervisedStreamCapture,
    stream: SupervisedProcessStream,
}

struct CapturedPipe {
    retained: Vec<u8>,
    exceeded: bool,
}

enum StdinCommand {
    Write {
        bytes: Vec<u8>,
        cancel_signal: Arc<AtomicBool>,
        deadline: Option<Instant>,
        completion: SyncSender<StdinWriteCompletion>,
    },
    Close {
        completion: SyncSender<io::Result<()>>,
    },
}

struct StdinWriteCompletion {
    bytes: Vec<u8>,
    result: Result<(), StdinWriteFailure>,
}

enum StdinWriteFailure {
    Canceled,
    DeadlineExceeded,
    Io(io::Error),
}

/// Running external process with exclusive ownership of all child pipes.
///
/// Callers may stream owned buffers with [`Self::write_owned`], then call
/// [`Self::finish`]. Dropping an unfinished instance force-terminates and reaps
/// the child.
pub struct SupervisedChild {
    child: Option<Child>,
    stdin_tx: Option<SyncSender<StdinCommand>>,
    stdin_thread: Option<JoinHandle<()>>,
    stdout: PipeDrain,
    stderr: PipeDrain,
    policy: SupervisedProcessPolicy,
}

impl SupervisedChild {
    /// Spawn a child and immediately start bounded stdout/stderr drains.
    pub fn spawn(
        command: &mut Command,
        policy: SupervisedProcessPolicy,
    ) -> Result<Self, SupervisedProcessError> {
        command
            .stdin(if policy.pipe_stdin {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        configure_hidden_child(command);
        let mut child = command.spawn().map_err(|source| SupervisedProcessError::Io {
            stage: SupervisedProcessStage::Spawn,
            source,
        })?;

        let stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => {
                terminate_and_reap(&mut child);
                return Err(SupervisedProcessError::MissingPipe { stream: "stdout" });
            }
        };
        let stderr = match child.stderr.take() {
            Some(stderr) => stderr,
            None => {
                terminate_and_reap(&mut child);
                return Err(SupervisedProcessError::MissingPipe { stream: "stderr" });
            }
        };

        let stdout_drain = spawn_pipe_drain(stdout, policy.stdout, SupervisedProcessStream::Stdout)
            .map_err(|source| {
                terminate_and_reap(&mut child);
                SupervisedProcessError::Io { stage: SupervisedProcessStage::StdoutDrain, source }
            })?;
        let stderr_drain =
            match spawn_pipe_drain(stderr, policy.stderr, SupervisedProcessStream::Stderr) {
                Ok(drain) => drain,
                Err(source) => {
                    terminate_and_reap(&mut child);
                    let _ = join_pipe_drain(stdout_drain, SupervisedProcessStage::StdoutDrain);
                    return Err(SupervisedProcessError::Io {
                        stage: SupervisedProcessStage::StderrDrain,
                        source,
                    });
                }
            };

        let (stdin_tx, stdin_thread) = if policy.pipe_stdin {
            let stdin = match child.stdin.take() {
                Some(stdin) => stdin,
                None => {
                    terminate_and_reap(&mut child);
                    let _ = join_pipe_drain(stdout_drain, SupervisedProcessStage::StdoutDrain);
                    let _ = join_pipe_drain(stderr_drain, SupervisedProcessStage::StderrDrain);
                    return Err(SupervisedProcessError::MissingPipe { stream: "stdin" });
                }
            };
            let (sender, receiver) = mpsc::sync_channel(1);
            let chunk_bytes = policy.stdin_chunk_bytes.max(1);
            let handle = match thread::Builder::new()
                .name("mondrian-media-process-stdin".to_owned())
                .spawn(move || pump_stdin(stdin, receiver, chunk_bytes))
            {
                Ok(handle) => handle,
                Err(source) => {
                    terminate_and_reap(&mut child);
                    let _ = join_pipe_drain(stdout_drain, SupervisedProcessStage::StdoutDrain);
                    let _ = join_pipe_drain(stderr_drain, SupervisedProcessStage::StderrDrain);
                    return Err(SupervisedProcessError::Io {
                        stage: SupervisedProcessStage::StdinWrite,
                        source,
                    });
                }
            };
            (Some(sender), Some(handle))
        } else {
            (None, None)
        };

        Ok(Self {
            child: Some(child),
            stdin_tx,
            stdin_thread,
            stdout: stdout_drain,
            stderr: stderr_drain,
            policy,
        })
    }

    /// Write one owned buffer while retaining cancellation authority.
    ///
    /// Ownership is returned on success so high-throughput callers can reuse
    /// the same allocation without copying. Cancellation or deadline expiry
    /// kills and reaps the process even if the writer is blocked in an OS pipe.
    pub fn write_owned(
        &mut self,
        bytes: Vec<u8>,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<Vec<u8>, SupervisedProcessError> {
        self.write_owned_while(bytes, &|| cancellation.is_canceled())
    }

    /// Write one owned buffer using an Adapter-provided cancellation probe.
    pub fn write_owned_while(
        &mut self,
        bytes: Vec<u8>,
        should_cancel: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<Vec<u8>, SupervisedProcessError> {
        if let Err(error) =
            self.check_terminal_while(SupervisedProcessStage::StdinWrite, should_cancel)
        {
            self.abort_and_settle();
            return Err(error);
        }
        let Some(stdin_tx) = self.stdin_tx.as_ref() else {
            return Err(SupervisedProcessError::MissingPipe { stream: "stdin" });
        };
        let (completion_tx, completion_rx) = mpsc::sync_channel(1);
        let write_cancel = Arc::new(AtomicBool::new(false));
        if stdin_tx
            .send(StdinCommand::Write {
                bytes,
                cancel_signal: Arc::clone(&write_cancel),
                deadline: self.policy.deadline,
                completion: completion_tx,
            })
            .is_err()
        {
            let error = SupervisedProcessError::Io {
                stage: SupervisedProcessStage::StdinWrite,
                source: io::Error::new(io::ErrorKind::BrokenPipe, "stdin pump is closed"),
            };
            self.abort_and_settle();
            return Err(error);
        }

        loop {
            match completion_rx.recv_timeout(self.policy.poll_interval) {
                Ok(completion) => {
                    if let Err(error) =
                        self.check_terminal_while(SupervisedProcessStage::StdinWrite, should_cancel)
                    {
                        write_cancel.store(true, Ordering::Release);
                        self.abort_and_settle();
                        return Err(error);
                    }
                    return match completion.result {
                        Ok(()) => Ok(completion.bytes),
                        Err(StdinWriteFailure::Canceled) => {
                            let error = SupervisedProcessError::Canceled {
                                stage: SupervisedProcessStage::StdinWrite,
                            };
                            self.abort_and_settle();
                            Err(error)
                        }
                        Err(StdinWriteFailure::DeadlineExceeded) => {
                            let error = SupervisedProcessError::DeadlineExceeded {
                                stage: SupervisedProcessStage::StdinWrite,
                            };
                            self.abort_and_settle();
                            Err(error)
                        }
                        Err(StdinWriteFailure::Io(source)) => {
                            let error = SupervisedProcessError::Io {
                                stage: SupervisedProcessStage::StdinWrite,
                                source,
                            };
                            self.abort_and_settle();
                            Err(error)
                        }
                    };
                }
                Err(RecvTimeoutError::Timeout) => {
                    if let Err(error) =
                        self.check_terminal_while(SupervisedProcessStage::StdinWrite, should_cancel)
                    {
                        write_cancel.store(true, Ordering::Release);
                        self.abort_and_settle();
                        return Err(error);
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    let error = SupervisedProcessError::WorkerPanicked {
                        stage: SupervisedProcessStage::StdinWrite,
                    };
                    self.abort_and_settle();
                    return Err(error);
                }
            }
        }
    }

    /// Close stdin, wait cancellably, drain bounded evidence, and reap.
    pub fn finish(
        self,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<SupervisedProcessOutput, SupervisedProcessError> {
        self.finish_while(&|| cancellation.is_canceled())
    }

    /// Close stdin and settle the process using an Adapter cancellation probe.
    pub fn finish_while(
        mut self,
        should_cancel: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<SupervisedProcessOutput, SupervisedProcessError> {
        if let Err(error) = self.close_stdin_while(should_cancel) {
            self.abort_and_settle();
            return Err(error);
        }
        let status = loop {
            if let Err(error) =
                self.check_terminal_while(SupervisedProcessStage::Wait, should_cancel)
            {
                self.abort_and_settle();
                return Err(error);
            }
            let Some(child) = self.child.as_mut() else {
                return Err(SupervisedProcessError::Io {
                    stage: SupervisedProcessStage::Wait,
                    source: io::Error::other("child was already reaped"),
                });
            };
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => thread::sleep(self.policy.poll_interval),
                Err(source) => {
                    let error =
                        SupervisedProcessError::Io { stage: SupervisedProcessStage::Wait, source };
                    self.abort_and_settle();
                    return Err(error);
                }
            }
        };
        self.child.take();
        self.stdin_tx.take();
        let stdin_error = self.stdin_thread.take().and_then(|handle| {
            handle.join().err().map(|_| SupervisedProcessError::WorkerPanicked {
                stage: SupervisedProcessStage::StdinClose,
            })
        });
        let stdout_result = join_pipe_drain(
            std::mem::replace(
                &mut self.stdout,
                empty_pipe_drain(SupervisedProcessStream::Stdout),
            ),
            SupervisedProcessStage::StdoutDrain,
        );
        let stderr_result = join_pipe_drain(
            std::mem::replace(
                &mut self.stderr,
                empty_pipe_drain(SupervisedProcessStream::Stderr),
            ),
            SupervisedProcessStage::StderrDrain,
        );
        if let Some(error) = stdin_error {
            return Err(error);
        }
        let stdout = stdout_result?;
        let stderr = stderr_result?;
        if should_cancel() {
            return Err(SupervisedProcessError::Canceled { stage: SupervisedProcessStage::Wait });
        }
        if self.policy.deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(SupervisedProcessError::DeadlineExceeded {
                stage: SupervisedProcessStage::Wait,
            });
        }
        for (stream, capture, exceeded) in [
            (
                SupervisedProcessStream::Stdout,
                self.policy.stdout,
                stdout.exceeded,
            ),
            (
                SupervisedProcessStream::Stderr,
                self.policy.stderr,
                stderr.exceeded,
            ),
        ] {
            if exceeded {
                if let Some(limit_bytes) = capture.strict_limit() {
                    return Err(SupervisedProcessError::OutputLimitExceeded {
                        stage: match stream {
                            SupervisedProcessStream::Stdout => SupervisedProcessStage::StdoutDrain,
                            SupervisedProcessStream::Stderr => SupervisedProcessStage::StderrDrain,
                        },
                        stream,
                        limit_bytes,
                    });
                }
            }
        }
        Ok(SupervisedProcessOutput {
            status,
            stdout: stdout.retained,
            stderr: stderr.retained,
            stdout_truncated: stdout.exceeded,
            stderr_truncated: stderr.exceeded,
        })
    }

    fn close_stdin_while(
        &mut self,
        should_cancel: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<(), SupervisedProcessError> {
        let Some(stdin_tx) = self.stdin_tx.take() else {
            return Ok(());
        };
        self.check_terminal_while(SupervisedProcessStage::StdinClose, should_cancel)?;
        let (completion_tx, completion_rx) = mpsc::sync_channel(1);
        stdin_tx.send(StdinCommand::Close { completion: completion_tx }).map_err(|_| {
            SupervisedProcessError::Io {
                stage: SupervisedProcessStage::StdinClose,
                source: io::Error::new(io::ErrorKind::BrokenPipe, "stdin pump is closed"),
            }
        })?;
        drop(stdin_tx);
        loop {
            match completion_rx.recv_timeout(self.policy.poll_interval) {
                Ok(Ok(())) => break,
                Ok(Err(source)) => {
                    return Err(SupervisedProcessError::Io {
                        stage: SupervisedProcessStage::StdinClose,
                        source,
                    });
                }
                Err(RecvTimeoutError::Timeout) => {
                    self.check_terminal_while(SupervisedProcessStage::StdinClose, should_cancel)?;
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(SupervisedProcessError::WorkerPanicked {
                        stage: SupervisedProcessStage::StdinClose,
                    });
                }
            }
        }
        if let Some(handle) = self.stdin_thread.take() {
            handle.join().map_err(|_| SupervisedProcessError::WorkerPanicked {
                stage: SupervisedProcessStage::StdinClose,
            })?;
        }
        Ok(())
    }

    fn check_terminal_while(
        &self,
        stage: SupervisedProcessStage,
        should_cancel: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<(), SupervisedProcessError> {
        if should_cancel() {
            return Err(SupervisedProcessError::Canceled { stage });
        }
        if self.policy.deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(SupervisedProcessError::DeadlineExceeded { stage });
        }
        for drain in [&self.stdout, &self.stderr] {
            if drain.failed.load(Ordering::Acquire) {
                return Err(SupervisedProcessError::Io {
                    stage: match drain.stream {
                        SupervisedProcessStream::Stdout => SupervisedProcessStage::StdoutDrain,
                        SupervisedProcessStream::Stderr => SupervisedProcessStage::StderrDrain,
                    },
                    source: io::Error::other(format!("{} drain failed", drain.stream)),
                });
            }
            if drain.exceeded.load(Ordering::Acquire) {
                if let Some(limit_bytes) = drain.capture.strict_limit() {
                    return Err(SupervisedProcessError::OutputLimitExceeded {
                        stage: match drain.stream {
                            SupervisedProcessStream::Stdout => SupervisedProcessStage::StdoutDrain,
                            SupervisedProcessStream::Stderr => SupervisedProcessStage::StderrDrain,
                        },
                        stream: drain.stream,
                        limit_bytes,
                    });
                }
            }
        }
        Ok(())
    }

    fn abort_and_settle(&mut self) {
        if let Some(child) = self.child.as_mut() {
            terminate_and_reap(child);
        }
        self.child.take();
        self.stdin_tx.take();
        if let Some(handle) = self.stdin_thread.take() {
            let _ = handle.join();
        }
        let stdout = std::mem::replace(
            &mut self.stdout,
            empty_pipe_drain(SupervisedProcessStream::Stdout),
        );
        let stderr = std::mem::replace(
            &mut self.stderr,
            empty_pipe_drain(SupervisedProcessStream::Stderr),
        );
        let _ = join_pipe_drain(stdout, SupervisedProcessStage::StdoutDrain);
        let _ = join_pipe_drain(stderr, SupervisedProcessStage::StderrDrain);
    }
}

impl Drop for SupervisedChild {
    fn drop(&mut self) {
        if self.child.is_some() {
            self.abort_and_settle();
        }
    }
}

/// Run one bounded external command with optional owned stdin.
pub fn run_supervised_command(
    command: &mut Command,
    stdin: Option<Vec<u8>>,
    mut policy: SupervisedProcessPolicy,
    cancellation: &ExecutionCancellationToken,
) -> Result<SupervisedProcessOutput, SupervisedProcessError> {
    policy.pipe_stdin = stdin.is_some();
    let mut child = SupervisedChild::spawn(command, policy)?;
    if let Some(stdin) = stdin {
        let _ = child.write_owned(stdin, cancellation)?;
    }
    child.finish(cancellation)
}

/// Run one bounded external command with an Adapter cancellation probe.
pub fn run_supervised_command_while(
    command: &mut Command,
    stdin: Option<Vec<u8>>,
    mut policy: SupervisedProcessPolicy,
    should_cancel: &(dyn Fn() -> bool + Send + Sync),
) -> Result<SupervisedProcessOutput, SupervisedProcessError> {
    policy.pipe_stdin = stdin.is_some();
    let mut child = SupervisedChild::spawn(command, policy)?;
    if let Some(stdin) = stdin {
        let _ = child.write_owned_while(stdin, should_cancel)?;
    }
    child.finish_while(should_cancel)
}

fn spawn_pipe_drain(
    pipe: impl Read + Send + 'static,
    capture: SupervisedStreamCapture,
    stream: SupervisedProcessStream,
) -> io::Result<PipeDrain> {
    let exceeded = Arc::new(AtomicBool::new(false));
    let failed = Arc::new(AtomicBool::new(false));
    let reader_exceeded = Arc::clone(&exceeded);
    let reader_failed = Arc::clone(&failed);
    let handle = thread::Builder::new()
        .name(format!("mondrian-media-process-{stream}"))
        .spawn(move || drain_pipe(pipe, capture, reader_exceeded, reader_failed))?;
    Ok(PipeDrain {
        handle: Some(handle),
        exceeded,
        failed,
        capture,
        stream,
    })
}

fn drain_pipe(
    mut pipe: impl Read,
    capture: SupervisedStreamCapture,
    exceeded: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
) -> io::Result<CapturedPipe> {
    let mut head = Vec::new();
    let mut tail = VecDeque::new();
    let mut truncated = false;
    let mut chunk = [0_u8; 16 * 1024];
    loop {
        let read = match pipe.read(&mut chunk) {
            Ok(read) => read,
            Err(error) => {
                failed.store(true, Ordering::Release);
                return Err(error);
            }
        };
        if read == 0 {
            break;
        }
        match capture {
            SupervisedStreamCapture::Drain => {}
            SupervisedStreamCapture::Head { limit_bytes, .. } => {
                let remaining = limit_bytes.saturating_sub(head.len());
                if read > remaining {
                    truncated = true;
                    exceeded.store(true, Ordering::Release);
                }
                head.extend_from_slice(&chunk[..read.min(remaining)]);
            }
            SupervisedStreamCapture::Tail { limit_bytes } => {
                if read > limit_bytes.saturating_sub(tail.len()) {
                    truncated = true;
                    exceeded.store(true, Ordering::Release);
                }
                for byte in &chunk[..read] {
                    if tail.len() == limit_bytes {
                        tail.pop_front();
                    }
                    if limit_bytes > 0 {
                        tail.push_back(*byte);
                    }
                }
            }
        }
    }
    let retained = match capture {
        SupervisedStreamCapture::Tail { .. } => tail.into_iter().collect(),
        SupervisedStreamCapture::Drain | SupervisedStreamCapture::Head { .. } => head,
    };
    Ok(CapturedPipe { retained, exceeded: truncated })
}

fn join_pipe_drain(
    mut drain: PipeDrain,
    stage: SupervisedProcessStage,
) -> Result<CapturedPipe, SupervisedProcessError> {
    let Some(handle) = drain.handle.take() else {
        return Ok(CapturedPipe { retained: Vec::new(), exceeded: false });
    };
    handle
        .join()
        .map_err(|_| SupervisedProcessError::WorkerPanicked { stage })?
        .map_err(|source| SupervisedProcessError::Io { stage, source })
}

fn empty_pipe_drain(stream: SupervisedProcessStream) -> PipeDrain {
    PipeDrain {
        handle: None,
        exceeded: Arc::new(AtomicBool::new(false)),
        failed: Arc::new(AtomicBool::new(false)),
        capture: SupervisedStreamCapture::Drain,
        stream,
    }
}

fn pump_stdin(stdin: ChildStdin, receiver: Receiver<StdinCommand>, chunk_bytes: usize) {
    let mut writer = BufWriter::with_capacity(chunk_bytes, stdin);
    while let Ok(command) = receiver.recv() {
        match command {
            StdinCommand::Write { mut bytes, cancel_signal, deadline, completion } => {
                let result = write_stdin_chunks(
                    &mut writer,
                    bytes.as_slice(),
                    chunk_bytes,
                    cancel_signal.as_ref(),
                    deadline,
                );
                let _ = completion
                    .send(StdinWriteCompletion { bytes: std::mem::take(&mut bytes), result });
            }
            StdinCommand::Close { completion } => {
                let result = writer.flush();
                drop(writer);
                let _ = completion.send(result);
                return;
            }
        }
    }
}

fn write_stdin_chunks(
    writer: &mut impl Write,
    bytes: &[u8],
    chunk_bytes: usize,
    cancel_signal: &AtomicBool,
    deadline: Option<Instant>,
) -> Result<(), StdinWriteFailure> {
    for chunk in bytes.chunks(chunk_bytes.max(1)) {
        if cancel_signal.load(Ordering::Acquire) {
            return Err(StdinWriteFailure::Canceled);
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(StdinWriteFailure::DeadlineExceeded);
        }
        writer.write_all(chunk).map_err(StdinWriteFailure::Io)?;
    }
    Ok(())
}

fn terminate_and_reap(child: &mut Child) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    const CHILD_MODE_ENV: &str = "MONDRIAN_PROCESS_SUPERVISOR_TEST_CHILD";

    fn child_command(mode: &str) -> Command {
        let executable = env::current_exe().expect("current test executable");
        let mut command = Command::new(executable);
        command
            .arg("--exact")
            .arg("process_supervisor::tests::supervisor_child_entry")
            .arg("--nocapture")
            .env(CHILD_MODE_ENV, mode);
        command
    }

    fn test_policy(pipe_stdin: bool) -> SupervisedProcessPolicy {
        SupervisedProcessPolicy {
            pipe_stdin,
            stdout: SupervisedStreamCapture::Head { limit_bytes: 1024 * 1024, reject_excess: true },
            stderr: SupervisedStreamCapture::Tail { limit_bytes: 32 * 1024 },
            deadline: Some(Instant::now() + Duration::from_secs(10)),
            stdin_chunk_bytes: 4 * 1024,
            poll_interval: Duration::from_millis(1),
        }
    }

    #[test]
    fn supervisor_child_entry() {
        let Ok(mode) = env::var(CHILD_MODE_ENV) else {
            return;
        };
        match mode.as_str() {
            "stderr-before-stdin" => {
                let evidence = vec![b'e'; 512 * 1024];
                std::io::stderr().write_all(&evidence).expect("write stderr");
                std::io::stderr().flush().expect("flush stderr");
                let mut input = Vec::new();
                std::io::stdin().read_to_end(&mut input).expect("read stdin");
                assert_eq!(input.len(), 1024 * 1024);
            }
            "block-stdin" | "sleep" => thread::sleep(Duration::from_secs(30)),
            "stdout-overflow" => {
                let output = vec![b'o'; 128 * 1024];
                std::io::stdout().write_all(&output).expect("write stdout");
                std::io::stdout().flush().expect("flush stdout");
            }
            other => panic!("unknown supervisor child mode {other}"),
        }
    }

    #[test]
    fn drains_full_stderr_before_a_child_reads_stdin() {
        let mut command = child_command("stderr-before-stdin");
        let output = run_supervised_command(
            &mut command,
            Some(vec![b'i'; 1024 * 1024]),
            test_policy(true),
            &ExecutionCancellationToken::new(),
        )
        .expect("stderr must drain concurrently with stdin");

        assert!(output.status.success());
        assert!(output.stderr_truncated);
        assert_eq!(output.stderr.len(), 32 * 1024);
    }

    #[test]
    fn cancellation_interrupts_a_writer_blocked_on_child_stdin() {
        let mut command = child_command("block-stdin");
        let mut child =
            SupervisedChild::spawn(&mut command, test_policy(true)).expect("spawn blocked child");
        let cancellation = ExecutionCancellationToken::new();
        let trigger = cancellation.clone();
        let cancel_thread = thread::spawn(move || {
            thread::sleep(Duration::from_millis(30));
            trigger.cancel();
        });
        let started = Instant::now();
        let error = child
            .write_owned(vec![0_u8; 8 * 1024 * 1024], &cancellation)
            .expect_err("blocked stdin must be cancellable");
        cancel_thread.join().expect("cancel trigger");

        assert!(matches!(
            error,
            SupervisedProcessError::Canceled { stage: SupervisedProcessStage::StdinWrite }
        ));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn deadline_terminates_and_reaps_a_running_child() {
        let mut command = child_command("sleep");
        let mut policy = test_policy(false);
        policy.deadline = Some(Instant::now() + Duration::from_millis(40));
        let started = Instant::now();
        let error = run_supervised_command(
            &mut command,
            None,
            policy,
            &ExecutionCancellationToken::new(),
        )
        .expect_err("deadline must terminate child");

        assert!(matches!(
            error,
            SupervisedProcessError::DeadlineExceeded { stage: SupervisedProcessStage::Wait }
        ));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn strict_stdout_limit_fails_closed() {
        let mut command = child_command("stdout-overflow");
        let mut policy = test_policy(false);
        policy.stdout =
            SupervisedStreamCapture::Head { limit_bytes: 4 * 1024, reject_excess: true };
        let error = run_supervised_command(
            &mut command,
            None,
            policy,
            &ExecutionCancellationToken::new(),
        )
        .expect_err("strict output cap must fail");

        assert!(matches!(
            error,
            SupervisedProcessError::OutputLimitExceeded {
                stage: SupervisedProcessStage::StdoutDrain,
                stream: SupervisedProcessStream::Stdout,
                limit_bytes: 4096
            }
        ));
    }
}
