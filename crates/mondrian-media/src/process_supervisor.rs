//! Bounded, cancellable ownership of external media processes.
//!
//! This is the only general-purpose `std::process::Child` lifecycle used by
//! FFmpeg/FFprobe CLI adapters. Pipe drains start immediately after spawn,
//! retained output is bounded, stdin is written on an owned pump thread, and
//! every terminal path kills when necessary and reaps the child.

use crate::{FfmpegChild as Child, SupervisedCommand};
use mondrian_core::ExecutionCancellationToken;
use std::collections::VecDeque;
use std::fmt;
use std::io::{self, BufWriter, Read, Write};
#[cfg(test)]
use std::process::Command;
use std::process::{ChildStdin, ExitStatus};
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
    /// The original operation failed and native cleanup independently failed.
    #[error("{primary}; external process cleanup: {cleanup:?}")]
    Cleanup {
        /// Original operation error, retained without string conversion.
        #[source]
        primary: Box<SupervisedProcessError>,
        /// Native process and pipe-worker closure facts.
        cleanup: Box<SupervisedProcessCleanupReceipt>,
    },
    /// A pipe worker could not be consumed within the original deadline.
    #[error("external media process worker did not settle during {stage}: {detail}")]
    WorkerClosure {
        /// Worker whose consuming close failed.
        stage: SupervisedProcessStage,
        /// Independent timeout or panic fact.
        detail: String,
    },
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
    pub fn is_canceled(&self) -> bool {
        matches!(self.primary(), Self::Canceled { .. })
            || matches!(self.primary(), Self::Io { stage: SupervisedProcessStage::Spawn, source }
                if source.get_ref().is_some_and(|error|
                    error.is::<crate::approved_provider_command::ProviderPreparationCanceled>()))
    }

    /// Return whether this terminal outcome was caused by an elapsed deadline.
    pub fn is_deadline_exceeded(&self) -> bool {
        matches!(self.primary(), Self::DeadlineExceeded { .. })
            || matches!(self.primary(), Self::Io { stage: SupervisedProcessStage::Spawn, source } if source.kind() == io::ErrorKind::TimedOut)
    }

    /// Original process failure beneath any independent cleanup attachment.
    pub fn primary(&self) -> &Self {
        match self {
            Self::Cleanup { primary, .. } => primary.primary(),
            _ => self,
        }
    }
}

/// Native process termination and consuming pipe-worker cleanup evidence.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupervisedProcessCleanupReceipt {
    /// The native child and its owned Windows job descendants all exited (or none existed).
    pub native_exit_observed: bool,
    /// Failure returned by the native termination request.
    pub kill_error: Option<String>,
    /// Failure returned while observing/reaping native exit.
    pub wait_error: Option<String>,
    /// Native exit was unavailable at the original deadline.
    pub deadline_exceeded: bool,
    /// Stdin worker cleanup failure.
    pub stdin_error: Option<String>,
    /// Stdout worker cleanup failure.
    pub stdout_error: Option<String>,
    /// Stderr worker cleanup failure.
    pub stderr_error: Option<String>,
}

impl SupervisedProcessCleanupReceipt {
    fn empty() -> Self {
        Self {
            native_exit_observed: true,
            kill_error: None,
            wait_error: None,
            deadline_exceeded: false,
            stdin_error: None,
            stdout_error: None,
            stderr_error: None,
        }
    }
    /// Native exit and every pipe-worker return were observed without cleanup error.
    pub fn all_resources_released(&self) -> bool {
        self.native_exit_observed
            && self.kill_error.is_none()
            && self.wait_error.is_none()
            && !self.deadline_exceeded
            && self.stdin_error.is_none()
            && self.stdout_error.is_none()
            && self.stderr_error.is_none()
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
    /// Consuming closure evidence for this exact native process and its pipes.
    pub cleanup: SupervisedProcessCleanupReceipt,
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
    stdout_chunks: Option<Receiver<Vec<u8>>>,
}

impl SupervisedChild {
    /// Spawn a child and immediately start bounded stdout/stderr drains.
    pub fn spawn(
        command: &mut impl SupervisedCommand,
        policy: SupervisedProcessPolicy,
    ) -> Result<Self, SupervisedProcessError> {
        Self::spawn_inner(command, policy, false)
    }

    fn spawn_inner(
        command: &mut impl SupervisedCommand,
        policy: SupervisedProcessPolicy,
        stream_stdout: bool,
    ) -> Result<Self, SupervisedProcessError> {
        command.configure_supervised_streams(policy.pipe_stdin);
        let child = command.spawn_supervised_until(policy.deadline).map_err(|source| {
            #[cfg(windows)]
            let cleanup = source
                .get_ref()
                .and_then(|error| {
                    error.downcast_ref::<crate::ffmpeg_command::NativeProcessSpawnFailure>()
                })
                .map(|error| error.cleanup.clone());
            let primary =
                SupervisedProcessError::Io { stage: SupervisedProcessStage::Spawn, source };
            #[cfg(windows)]
            if let Some(cleanup) = cleanup {
                return SupervisedProcessError::Cleanup {
                    primary: Box::new(primary),
                    cleanup: Box::new(cleanup),
                };
            }
            primary
        })?;
        let mut owner = Self {
            child: Some(child),
            stdin_tx: None,
            stdin_thread: None,
            stdout: empty_pipe_drain(SupervisedProcessStream::Stdout),
            stderr: empty_pipe_drain(SupervisedProcessStream::Stderr),
            policy,
            stdout_chunks: None,
        };
        let startup = (|| {
            let child = owner
                .child
                .as_mut()
                .ok_or(SupervisedProcessError::MissingPipe { stream: "child" })?;
            let stdout = child
                .stdout
                .take()
                .ok_or(SupervisedProcessError::MissingPipe { stream: "stdout" })?;
            let stderr = child
                .stderr
                .take()
                .ok_or(SupervisedProcessError::MissingPipe { stream: "stderr" })?;
            let stdout_sender = if stream_stdout {
                let (sender, receiver) = mpsc::sync_channel(2);
                owner.stdout_chunks = Some(receiver);
                Some(sender)
            } else {
                None
            };
            owner.stdout = spawn_pipe_drain_inner(
                stdout,
                owner.policy.stdout,
                SupervisedProcessStream::Stdout,
                stdout_sender,
            )
            .map_err(|source| SupervisedProcessError::Io {
                stage: SupervisedProcessStage::StdoutDrain,
                source,
            })?;
            owner.stderr =
                spawn_pipe_drain(stderr, owner.policy.stderr, SupervisedProcessStream::Stderr)
                    .map_err(|source| SupervisedProcessError::Io {
                        stage: SupervisedProcessStage::StderrDrain,
                        source,
                    })?;
            if owner.policy.pipe_stdin {
                let stdin = child
                    .stdin
                    .take()
                    .ok_or(SupervisedProcessError::MissingPipe { stream: "stdin" })?;
                let (sender, receiver) = mpsc::sync_channel(1);
                let chunk_bytes = owner.policy.stdin_chunk_bytes.max(1);
                owner.stdin_thread = Some(
                    thread::Builder::new()
                        .name("mondrian-media-process-stdin".to_owned())
                        .spawn(move || pump_stdin(stdin, receiver, chunk_bytes))
                        .map_err(|source| SupervisedProcessError::Io {
                            stage: SupervisedProcessStage::StdinWrite,
                            source,
                        })?,
                );
                owner.stdin_tx = Some(sender);
            }
            Ok::<(), SupervisedProcessError>(())
        })();
        match startup {
            Ok(()) => Ok(owner),
            Err(primary) => Err(owner.fail(primary)),
        }
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
            return Err(self.fail(error));
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
            return Err(self.fail(error));
        }

        loop {
            match completion_rx.recv_timeout(self.policy.poll_interval) {
                Ok(completion) => {
                    if let Err(error) =
                        self.check_terminal_while(SupervisedProcessStage::StdinWrite, should_cancel)
                    {
                        write_cancel.store(true, Ordering::Release);
                        return Err(self.fail(error));
                    }
                    return match completion.result {
                        Ok(()) => Ok(completion.bytes),
                        Err(StdinWriteFailure::Canceled) => {
                            let error = SupervisedProcessError::Canceled {
                                stage: SupervisedProcessStage::StdinWrite,
                            };
                            Err(self.fail(error))
                        }
                        Err(StdinWriteFailure::DeadlineExceeded) => {
                            let error = SupervisedProcessError::DeadlineExceeded {
                                stage: SupervisedProcessStage::StdinWrite,
                            };
                            Err(self.fail(error))
                        }
                        Err(StdinWriteFailure::Io(source)) => {
                            let error = SupervisedProcessError::Io {
                                stage: SupervisedProcessStage::StdinWrite,
                                source,
                            };
                            Err(self.fail(error))
                        }
                    };
                }
                Err(RecvTimeoutError::Timeout) => {
                    if let Err(error) =
                        self.check_terminal_while(SupervisedProcessStage::StdinWrite, should_cancel)
                    {
                        write_cancel.store(true, Ordering::Release);
                        return Err(self.fail(error));
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    let error = SupervisedProcessError::WorkerPanicked {
                        stage: SupervisedProcessStage::StdinWrite,
                    };
                    return Err(self.fail(error));
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
        self,
        should_cancel: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<SupervisedProcessOutput, SupervisedProcessError> {
        self.finish_with_consumer(should_cancel, &mut |_| Ok(()))
    }

    fn finish_with_consumer(
        mut self,
        should_cancel: &(dyn Fn() -> bool + Send + Sync),
        consumer: &mut impl FnMut(&[u8]) -> io::Result<()>,
    ) -> Result<SupervisedProcessOutput, SupervisedProcessError> {
        if let Err(error) = self.close_stdin_while(should_cancel) {
            return Err(self.fail(error));
        }
        let mut native_status = None;
        let status = loop {
            if let Err(error) =
                self.check_terminal_while(SupervisedProcessStage::Wait, should_cancel)
            {
                return Err(self.fail(error));
            }
            // Bounded work per poll preserves native cancellation/deadline authority.
            let mut progressed = false;
            for _ in 0..8 {
                let next = self.stdout_chunks.as_ref().map(Receiver::try_recv);
                match next {
                    Some(Ok(bytes)) => {
                        progressed = true;
                        let consumed =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                consumer(&bytes)
                            }));
                        let failure = match consumed {
                            Ok(Ok(())) => None,
                            Ok(Err(source)) => Some(SupervisedProcessError::Io {
                                stage: SupervisedProcessStage::StdoutDrain,
                                source,
                            }),
                            Err(_) => Some(SupervisedProcessError::WorkerPanicked {
                                stage: SupervisedProcessStage::StdoutDrain,
                            }),
                        };
                        if let Some(error) = failure {
                            return Err(self.fail(error));
                        }
                    }
                    Some(Err(mpsc::TryRecvError::Disconnected)) => {
                        self.stdout_chunks.take();
                        break;
                    }
                    Some(Err(mpsc::TryRecvError::Empty)) | None => break,
                }
            }
            if let Some(status) = native_status {
                if self.stdout_chunks.is_none() {
                    break status;
                }
                if !progressed {
                    thread::sleep(self.policy.poll_interval);
                }
                continue;
            }
            let Some(child) = self.child.as_mut() else {
                return Err(SupervisedProcessError::Io {
                    stage: SupervisedProcessStage::Wait,
                    source: io::Error::other("child was already reaped"),
                });
            };
            match child.try_wait() {
                Ok(Some(status)) => native_status = Some(status),
                Ok(None) => {
                    if !progressed {
                        thread::sleep(self.policy.poll_interval);
                    }
                }
                Err(source) => {
                    let error =
                        SupervisedProcessError::Io { stage: SupervisedProcessStage::Wait, source };
                    return Err(self.fail(error));
                }
            }
        };
        let child_pid = self.child.as_ref().map(|child| child.id());
        self.child.take();
        self.stdin_tx.take();
        let deadline = self.cleanup_deadline();
        let stdin_error = self.stdin_thread.take().and_then(|handle| {
            join_worker_until(handle, deadline).err().map(|detail| {
                SupervisedProcessError::WorkerClosure {
                    stage: SupervisedProcessStage::StdinClose,
                    detail,
                }
            })
        });
        let stdout_result = join_pipe_drain(
            std::mem::replace(
                &mut self.stdout,
                empty_pipe_drain(SupervisedProcessStream::Stdout),
            ),
            SupervisedProcessStage::StdoutDrain,
            deadline,
        );
        let stderr_result = join_pipe_drain(
            std::mem::replace(
                &mut self.stderr,
                empty_pipe_drain(SupervisedProcessStream::Stderr),
            ),
            SupervisedProcessStage::StderrDrain,
            deadline,
        );
        let cleanup = SupervisedProcessCleanupReceipt {
            stdin_error: stdin_error.as_ref().map(ToString::to_string),
            stdout_error: stdout_result.as_ref().err().map(ToString::to_string),
            stderr_error: stderr_result.as_ref().err().map(ToString::to_string),
            ..SupervisedProcessCleanupReceipt::empty()
        };
        if let Some(pid) = child_pid {
            crate::ffmpeg_command::record_native_cleanup(pid, &cleanup);
        }
        let (stdout, stderr) = match (stdin_error, stdout_result, stderr_result) {
            (Some(primary), _, _) | (None, Err(primary), _) | (None, Ok(_), Err(primary)) => {
                return Err(SupervisedProcessError::Cleanup {
                    primary: Box::new(primary),
                    cleanup: Box::new(cleanup),
                });
            }
            (None, Ok(stdout), Ok(stderr)) => (stdout, stderr),
        };
        if should_cancel() {
            return Err(SupervisedProcessError::Cleanup {
                primary: Box::new(SupervisedProcessError::Canceled {
                    stage: SupervisedProcessStage::Wait,
                }),
                cleanup: Box::new(cleanup),
            });
        }
        if self.policy.deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(SupervisedProcessError::Cleanup {
                primary: Box::new(SupervisedProcessError::DeadlineExceeded {
                    stage: SupervisedProcessStage::Wait,
                }),
                cleanup: Box::new(cleanup),
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
            if exceeded && let Some(limit_bytes) = capture.strict_limit() {
                return Err(SupervisedProcessError::Cleanup {
                    primary: Box::new(SupervisedProcessError::OutputLimitExceeded {
                        stage: match stream {
                            SupervisedProcessStream::Stdout => SupervisedProcessStage::StdoutDrain,
                            SupervisedProcessStream::Stderr => SupervisedProcessStage::StderrDrain,
                        },
                        stream,
                        limit_bytes,
                    }),
                    cleanup: Box::new(cleanup),
                });
            }
        }
        Ok(SupervisedProcessOutput {
            cleanup,
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
            join_worker_until(handle, self.cleanup_deadline()).map_err(|detail| {
                SupervisedProcessError::WorkerClosure {
                    stage: SupervisedProcessStage::StdinClose,
                    detail,
                }
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
            if drain.exceeded.load(Ordering::Acquire)
                && let Some(limit_bytes) = drain.capture.strict_limit()
            {
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
        Ok(())
    }

    fn cleanup_deadline(&self) -> Instant {
        self.policy.deadline.unwrap_or_else(|| Instant::now() + Duration::from_secs(5))
    }

    fn fail(&mut self, primary: SupervisedProcessError) -> SupervisedProcessError {
        let cleanup = self.abort_and_settle();
        SupervisedProcessError::Cleanup {
            primary: Box::new(primary),
            cleanup: Box::new(cleanup),
        }
    }

    fn abort_and_settle(&mut self) -> SupervisedProcessCleanupReceipt {
        // Unblock the bounded stdout sender before joining or terminating the child.
        self.stdout_chunks.take();
        let deadline = self.cleanup_deadline();
        let mut receipt = self
            .child
            .as_mut()
            .map_or_else(SupervisedProcessCleanupReceipt::empty, |child| {
                terminate_and_reap(child, deadline)
            });
        self.stdin_tx.take();
        if let Some(handle) = self.stdin_thread.take()
            && let Err(error) = join_worker_until(handle, deadline)
        {
            receipt.stdin_error = Some(error);
        }
        for (drain, error) in [
            (&mut self.stdout, &mut receipt.stdout_error),
            (&mut self.stderr, &mut receipt.stderr_error),
        ] {
            if let Some(handle) = drain.handle.take() {
                match join_worker_until(handle, deadline) {
                    Ok(Ok(_)) => {}
                    Ok(Err(failure)) => *error = Some(failure.to_string()),
                    Err(failure) => *error = Some(failure),
                }
            }
        }
        if let Some(child) = self.child.as_ref() {
            crate::ffmpeg_command::record_native_cleanup(child.id(), &receipt);
        }
        self.child.take();
        receipt
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
    command: &mut impl SupervisedCommand,
    stdin: Option<Vec<u8>>,
    mut policy: SupervisedProcessPolicy,
    cancellation: &ExecutionCancellationToken,
) -> Result<SupervisedProcessOutput, SupervisedProcessError> {
    if cancellation.is_canceled() {
        return Err(SupervisedProcessError::Canceled { stage: SupervisedProcessStage::Spawn });
    }
    policy.pipe_stdin = stdin.is_some();
    let mut child = SupervisedChild::spawn(command, policy)?;
    if let Some(stdin) = stdin {
        let _ = child.write_owned(stdin, cancellation)?;
    }
    child.finish(cancellation)
}

/// Run one bounded external command with an Adapter cancellation probe.
pub fn run_supervised_command_while(
    command: &mut impl SupervisedCommand,
    stdin: Option<Vec<u8>>,
    mut policy: SupervisedProcessPolicy,
    should_cancel: &(dyn Fn() -> bool + Send + Sync),
) -> Result<SupervisedProcessOutput, SupervisedProcessError> {
    if should_cancel() {
        return Err(SupervisedProcessError::Canceled { stage: SupervisedProcessStage::Spawn });
    }
    policy.pipe_stdin = stdin.is_some();
    let mut child = SupervisedChild::spawn(command, policy)?;
    if let Some(stdin) = stdin {
        let _ = child.write_owned_while(stdin, should_cancel)?;
    }
    child.finish_while(should_cancel)
}

/// Consume stdout incrementally on the calling thread with two bounded 16 KiB chunks.
///
/// The consumer must return promptly. Consumer errors and panics terminate and
/// consume the same native process and pipe owners, preserving cleanup evidence.
/// Stdin is disabled; stdout is streamed without retained capture or disk spooling.
pub fn run_supervised_command_streaming_stdout(
    command: &mut impl SupervisedCommand,
    mut policy: SupervisedProcessPolicy,
    should_cancel: &(dyn Fn() -> bool + Send + Sync),
    mut consumer: impl FnMut(&[u8]) -> io::Result<()>,
) -> Result<SupervisedProcessOutput, SupervisedProcessError> {
    if should_cancel() {
        return Err(SupervisedProcessError::Canceled { stage: SupervisedProcessStage::Spawn });
    }
    policy.pipe_stdin = false;
    policy.stdout = SupervisedStreamCapture::Drain;
    SupervisedChild::spawn_inner(command, policy, true)?
        .finish_with_consumer(should_cancel, &mut consumer)
}

fn spawn_pipe_drain(
    pipe: impl Read + Send + 'static,
    capture: SupervisedStreamCapture,
    stream: SupervisedProcessStream,
) -> io::Result<PipeDrain> {
    spawn_pipe_drain_inner(pipe, capture, stream, None)
}

fn spawn_pipe_drain_inner(
    pipe: impl Read + Send + 'static,
    capture: SupervisedStreamCapture,
    stream: SupervisedProcessStream,
    sender: Option<SyncSender<Vec<u8>>>,
) -> io::Result<PipeDrain> {
    let exceeded = Arc::new(AtomicBool::new(false));
    let failed = Arc::new(AtomicBool::new(false));
    let reader_exceeded = Arc::clone(&exceeded);
    let reader_failed = Arc::clone(&failed);
    let handle = thread::Builder::new()
        .name(format!("mondrian-media-process-{stream}"))
        .spawn(move || drain_pipe(pipe, capture, reader_exceeded, reader_failed, sender))?;
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
    sender: Option<SyncSender<Vec<u8>>>,
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
        if let Some(sender) = &sender {
            // A disconnected consumer means its owner is already closing this drain.
            if sender.send(chunk[..read].to_vec()).is_err() {
                break;
            }
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
    deadline: Instant,
) -> Result<CapturedPipe, SupervisedProcessError> {
    let Some(handle) = drain.handle.take() else {
        return Ok(CapturedPipe { retained: Vec::new(), exceeded: false });
    };
    join_worker_until(handle, deadline)
        .map_err(|detail| SupervisedProcessError::WorkerClosure { stage, detail })?
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

pub(crate) fn terminate_and_reap(
    child: &mut Child,
    deadline: Instant,
) -> SupervisedProcessCleanupReceipt {
    let mut receipt = SupervisedProcessCleanupReceipt::empty();
    match child.try_wait() {
        Ok(Some(_)) => return receipt,
        Ok(None) => {}
        Err(error) => receipt.wait_error = Some(error.to_string()),
    }
    receipt.native_exit_observed = false;
    receipt.kill_error = child.kill().err().map(|error| error.to_string());
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                receipt.native_exit_observed = true;
                break;
            }
            Ok(None) => {}
            Err(error) => {
                receipt.wait_error = Some(error.to_string());
                break;
            }
        }
        if Instant::now() >= deadline {
            receipt.deadline_exceeded = true;
            break;
        }
        thread::sleep(
            Duration::from_millis(1).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
    receipt
}

pub(crate) fn join_worker_until<T>(handle: JoinHandle<T>, deadline: Instant) -> Result<T, String> {
    while !handle.is_finished() {
        if Instant::now() >= deadline {
            return Err("worker did not return by the original process deadline".to_owned());
        }
        thread::sleep(
            Duration::from_millis(1).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
    handle.join().map_err(|payload| {
        // Opaque destructor execution is not permitted on the closure stack.
        std::mem::forget(payload);
        "process worker panicked (payload abandoned)".to_owned()
    })
}

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
            error.primary(),
            SupervisedProcessError::Canceled { stage: SupervisedProcessStage::StdinWrite }
        ));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn original_deadline_covers_native_spawn_wait_and_retirement() {
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

        // Native Job admission now shares the original deadline. On a busy
        // Windows host this budget may expire before the suspended child resumes;
        // that is still a deadline failure and must retain its raw cleanup.
        assert!(error.is_deadline_exceeded(), "{error:?}");
        match &error {
            SupervisedProcessError::Cleanup { cleanup, .. } => {
                assert!(
                    cleanup.native_exit_observed || cleanup.deadline_exceeded,
                    "{cleanup:?}"
                );
            }
            // Admission may reject the expired budget before a child exists.
            SupervisedProcessError::Io { stage: SupervisedProcessStage::Spawn, source }
                if source.kind() == io::ErrorKind::TimedOut => {}
            _ => panic!("an admitted child must preserve native cleanup: {error:?}"),
        }
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn provider_cancel_race_keeps_typed_cause_without_reclassifying_os_interruptions() {
        let token = ExecutionCancellationToken::new();
        token.cancel();
        let source = crate::approved_provider_command::boundary(
            Instant::now() + Duration::from_secs(1),
            &token,
        )
        .expect_err("provider owner observed cancellation after entry admission");
        assert_eq!(source.kind(), io::ErrorKind::Interrupted);
        let canceled = SupervisedProcessError::Io { stage: SupervisedProcessStage::Spawn, source };
        assert!(canceled.is_canceled());
        let wrapped = SupervisedProcessError::Cleanup {
            primary: Box::new(canceled),
            cleanup: Box::new(SupervisedProcessCleanupReceipt::empty()),
        };
        assert!(wrapped.is_canceled());
        assert!(matches!(
            wrapped.primary(),
            SupervisedProcessError::Io { .. }
        ));
        let ordinary = SupervisedProcessError::Io {
            stage: SupervisedProcessStage::Spawn,
            source: io::Error::new(io::ErrorKind::Interrupted, "ordinary OS interruption"),
        };
        assert!(!ordinary.is_canceled());
    }

    #[test]
    fn precanceled_commands_reject_before_spawn_in_all_output_modes() {
        let cancel = ExecutionCancellationToken::new();
        cancel.cancel();
        let mut command = child_command("stdout-overflow");
        let ordinary = run_supervised_command(&mut command, None, test_policy(false), &cancel);
        let probed = run_supervised_command_while(&mut command, None, test_policy(false), &|| true);
        let streamed = run_supervised_command_streaming_stdout(
            &mut command,
            test_policy(false),
            &|| true,
            |_| panic!("a precanceled command must never reach the output consumer"),
        );
        for result in [ordinary, probed, streamed] {
            let error = result.expect_err("precanceled command must not start");
            assert!(
                matches!(
                    error,
                    SupervisedProcessError::Canceled { stage: SupervisedProcessStage::Spawn }
                ),
                "{error:?}"
            );
            assert!(error.is_canceled());
        }
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
            error.primary(),
            SupervisedProcessError::OutputLimitExceeded {
                stage: SupervisedProcessStage::StdoutDrain,
                stream: SupervisedProcessStream::Stdout,
                limit_bytes: 4096
            }
        ));
    }
    #[test]
    fn simultaneous_pipe_panics_preserve_both_raw_cleanup_failures() {
        let stdout = thread::spawn(|| -> io::Result<CapturedPipe> { panic!("stdout failure") });
        let stderr = thread::spawn(|| -> io::Result<CapturedPipe> { panic!("stderr failure") });
        let mut owner = SupervisedChild {
            child: None,
            stdin_tx: None,
            stdin_thread: None,
            stdout: empty_pipe_drain(SupervisedProcessStream::Stdout),
            stderr: empty_pipe_drain(SupervisedProcessStream::Stderr),
            policy: test_policy(false),
            stdout_chunks: None,
        };
        owner.stdout.handle = Some(stdout);
        owner.stderr.handle = Some(stderr);
        let primary = SupervisedProcessError::Canceled { stage: SupervisedProcessStage::Wait };
        let error = owner.fail(primary);
        assert!(error.is_canceled());
        let SupervisedProcessError::Cleanup { cleanup, .. } = error else {
            panic!("raw closure required")
        };
        assert!(cleanup.stdout_error.is_some());
        assert!(cleanup.stderr_error.is_some());
        assert!(!cleanup.all_resources_released());
    }

    #[test]
    fn expired_cleanup_deadline_does_not_block_on_a_nonreturning_pipe_worker() {
        let (release, blocked) = mpsc::channel();
        let worker = thread::spawn(move || {
            let _ = blocked.recv();
        });
        let started = Instant::now();
        let result = join_worker_until(worker, started);
        assert!(result.is_err());
        assert!(started.elapsed() < Duration::from_secs(1));
        let _ = release.send(());
    }

    #[test]
    fn streamed_stdout_is_complete_without_retained_capture() {
        let mut count = 0;
        let output = run_supervised_command_streaming_stdout(
            &mut child_command("stdout-overflow"),
            test_policy(false),
            &|| false,
            |bytes| {
                assert!(bytes.len() <= 16 * 1024);
                count += bytes.iter().filter(|&&byte| byte == b'o').count();
                Ok(())
            },
        )
        .expect("stream must consume all queued data after native exit");
        assert!(output.status.success());
        assert!(output.cleanup.all_resources_released());
        assert!(output.stdout.is_empty());
        assert!(count >= 128 * 1024);
    }

    #[test]
    fn streamed_consumer_error_panic_and_cancellation_consume_all_owners() {
        for mode in 0..3 {
            let canceled = AtomicBool::new(false);
            let error = run_supervised_command_streaming_stdout(
                &mut child_command("stdout-overflow"),
                test_policy(false),
                &|| canceled.load(Ordering::Acquire),
                |_| match mode {
                    0 => Err(io::Error::other("consumer rejected content")),
                    1 => panic!("consumer panic"),
                    _ => {
                        canceled.store(true, Ordering::Release);
                        Ok(())
                    }
                },
            )
            .expect_err("consumer must terminate the operation");
            let SupervisedProcessError::Cleanup { cleanup, .. } = error else {
                panic!("raw cleanup required")
            };
            assert!(cleanup.all_resources_released(), "{cleanup:?}");
        }
    }
}
