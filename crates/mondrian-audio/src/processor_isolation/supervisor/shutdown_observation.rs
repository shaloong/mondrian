//! Local observations of the existing shutdown calls, never another lifecycle.
//! Completed acknowledges the worker's pre-ACK scope join/main finish. A later
//! exit delay does not identify plugin Drop, runtime teardown, or another cause.

use super::{worker_failed_detail, ProcessState, WorkerResponse};
use crate::AudioProcessorHostError;
use std::io;
use std::process::ExitStatus;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct IoFailure {
    pub(super) kind: io::ErrorKind,
    raw_os: Option<i32>,
}

impl From<&io::Error> for IoFailure {
    fn from(error: &io::Error) -> Self {
        Self { kind: error.kind(), raw_os: error.raw_os_error() }
    }
}

impl IoFailure {
    pub(super) fn into_io_error(self) -> io::Error {
        match self.raw_os {
            Some(code) => io::Error::from_raw_os_error(code),
            None => io::Error::new(self.kind, "native child exit observation failed"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Timed<T> {
    pub(super) after: Duration,
    elapsed: Duration,
    pub(super) outcome: T,
}

impl<T> Timed<T> {
    pub(super) fn new(origin: Instant, started: Instant, outcome: T) -> Self {
        let finished = Instant::now();
        Self {
            after: finished.duration_since(origin),
            elapsed: finished.duration_since(started),
            outcome,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ChildObservation {
    Running,
    Exited(ExitStatus),
    Io(IoFailure),
}

impl From<&io::Result<Option<ExitStatus>>> for ChildObservation {
    fn from(observed: &io::Result<Option<ExitStatus>>) -> Self {
        match observed {
            Ok(Some(status)) => Self::Exited(*status),
            Ok(None) => Self::Running,
            Err(error) => Self::Io(error.into()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ShutdownFailure {
    AlreadyRetiring,
    ConfigureControlTimeout,
    CommandWrite,
    ResponseRead,
    NonCompletedResponse,
    NonzeroExit,
    ExitObservationIo,
    ExitObservationDeadline,
}

// Every field is bounded host metadata. In particular a Failed response's text
// is neither retained nor copied into this diagnostic.
pub(super) fn response_tag(response: &WorkerResponse) -> u32 {
    match response {
        WorkerResponse::Completed => 0,
        WorkerResponse::Failed(_) => 1,
        WorkerResponse::CompletedWithPending(_) => 2,
        WorkerResponse::DeferredForConfiguration(_) => 3,
        WorkerResponse::NotesCompleted { .. } => 4,
    }
}

#[derive(Debug)]
pub(super) struct CleanupObservation {
    pub(super) origin: Instant,
    pub(super) initial_observe: Option<Timed<ChildObservation>>,
    pub(super) kill: Option<Timed<Result<(), IoFailure>>>,
    pub(super) final_observe: Option<Timed<ChildObservation>>,
    pub(super) wait: Option<Timed<Result<ExitStatus, IoFailure>>>,
}

impl CleanupObservation {
    fn new(origin: Instant) -> Self {
        Self {
            origin,
            initial_observe: None,
            kill: None,
            final_observe: None,
            wait: None,
        }
    }
}

#[derive(Debug)]
pub(in crate::processor_isolation) struct ShutdownReport {
    pub(super) started: Instant,
    pid: u32,
    initial_state: ProcessState,
    final_state: ProcessState,
    operation_budget: Duration,
    control_budget: Duration,
    exit_budget: Duration,
    pub(super) configure: Option<Timed<Result<(), IoFailure>>>,
    pub(super) command_write: Option<Timed<Result<(), IoFailure>>>,
    pub(super) response_read: Option<Timed<Result<u32, IoFailure>>>,
    // Timed.after marks when the whole bounded exit-wait span was recorded
    // relative to shutdown start; elapsed covers that span, not one syscall.
    // Running may be a wait timeout; Exited always comes from Child.try_wait.
    // Io may be the platform wait or try_wait error. None means no observation.
    pub(super) exit_loop: Option<Timed<ChildObservation>>,
    pub(super) primary_failure: Option<ShutdownFailure>,
    pub(super) cleanup: CleanupObservation,
    elapsed: Duration,
    successful: bool,
}

impl ShutdownReport {
    pub(super) fn new(pid: u32, initial_state: ProcessState, operation_budget: Duration) -> Self {
        let started = Instant::now();
        let exit_budget = operation_budget.min(Duration::from_millis(100));
        Self {
            started,
            pid,
            initial_state,
            final_state: initial_state,
            operation_budget,
            control_budget: exit_budget.max(Duration::from_millis(1)),
            exit_budget,
            configure: None,
            command_write: None,
            response_read: None,
            exit_loop: None,
            primary_failure: None,
            cleanup: CleanupObservation::new(started),
            elapsed: Duration::ZERO,
            successful: false,
        }
    }

    pub(super) fn finish(mut self, successful: bool, final_state: ProcessState) -> Self {
        self.successful = successful;
        self.final_state = final_state;
        self.elapsed = self.started.elapsed();
        self
    }

    pub(in crate::processor_isolation) fn successful(&self) -> bool {
        self.successful
    }

    pub(in crate::processor_isolation) fn validation_result(
        &self,
    ) -> Result<(), AudioProcessorHostError> {
        if self.successful {
            return Ok(());
        }
        Err(worker_failed_detail("validation shutdown", &format!(
            "Worker did not acknowledge and exit within the bounded shutdown; pid={}; initial={:?}; final={:?}; primary={:?}; operation_budget={:?}; control_budget={:?}; exit_budget={:?}; elapsed={:?}; configure={:?}; command_write={:?}; response_read={:?}; exit_loop={:?}; cleanup_initial={:?}; cleanup_kill={:?}; cleanup_final={:?}; cleanup_wait={:?}",
            self.pid, self.initial_state, self.final_state, self.primary_failure,
            self.operation_budget, self.control_budget, self.exit_budget, self.elapsed,
            self.configure, self.command_write, self.response_read, self.exit_loop,
            self.cleanup.initial_observe, self.cleanup.kill, self.cleanup.final_observe, self.cleanup.wait,
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_shutdown_report_keeps_actual_io_metadata_and_excludes_untrusted_error_text() {
        let mut report = ShutdownReport::new(123, ProcessState::Running, Duration::from_secs(30));
        let error = io::Error::new(
            io::ErrorKind::PermissionDenied,
            "untrusted input".repeat(4096),
        );
        report.configure = Some(Timed::new(
            report.started,
            report.started,
            Err((&error).into()),
        ));
        report.primary_failure = Some(ShutdownFailure::ConfigureControlTimeout);
        let report = report.finish(false, ProcessState::Retiring);
        let Err(AudioProcessorHostError::WorkerFailed { operation, detail }) =
            report.validation_result()
        else {
            panic!("typed error");
        };
        assert_eq!(operation, "validation shutdown");
        assert!(detail.contains("ConfigureControlTimeout") && detail.contains("PermissionDenied"));
        assert!(
            detail.contains("operation_budget=30s")
                && detail.contains("control_budget=100ms")
                && detail.contains("exit_budget=100ms")
        );
        assert!(!detail.contains("untrusted input"));
        assert!(detail.len() < super::super::super::protocol::MAX_WORKER_DETAIL_BYTES);
        let actual = io::Error::from_raw_os_error(5);
        let captured = IoFailure::from(&actual);
        assert_eq!(captured.kind, actual.kind());
        assert_eq!(captured.raw_os, actual.raw_os_error());
    }

    #[test]
    fn native_shutdown_report_full_primitive_observation_fits_existing_detail_bound() {
        let mut report = ShutdownReport::new(u32::MAX, ProcessState::Running, Duration::MAX);
        let failure = IoFailure::from(&io::Error::from_raw_os_error(i32::MIN));
        let io = Timed {
            after: Duration::MAX,
            elapsed: Duration::MAX,
            outcome: Err(failure),
        };
        let child = Timed {
            after: Duration::MAX,
            elapsed: Duration::MAX,
            outcome: ChildObservation::Io(failure),
        };
        report.configure = Some(io);
        report.command_write = Some(io);
        report.response_read = Some(Timed {
            after: Duration::MAX,
            elapsed: Duration::MAX,
            outcome: Err(failure),
        });
        report.exit_loop = Some(child);
        report.primary_failure = Some(ShutdownFailure::ExitObservationIo);
        report.cleanup.initial_observe = Some(child);
        report.cleanup.kill = Some(io);
        report.cleanup.final_observe = Some(child);
        report.cleanup.wait = Some(Timed {
            after: Duration::MAX,
            elapsed: Duration::MAX,
            outcome: Err(failure),
        });
        let mut report = report.finish(false, ProcessState::Retiring);
        report.elapsed = Duration::MAX;
        let Err(AudioProcessorHostError::WorkerFailed { detail, .. }) = report.validation_result()
        else {
            panic!("bounded typed error");
        };
        assert!(
            detail.len() < super::super::super::protocol::MAX_WORKER_DETAIL_BYTES,
            "all original fields fit before the original detail truncation"
        );
        assert!(detail.contains("cleanup_wait=Some(Timed"));
        assert!(
            detail.ends_with("raw_os: Some(-2147483648) }) })"),
            "complete last phase: {detail}"
        );
    }

    #[test]
    fn native_shutdown_response_tags_preserve_each_original_wire_variant_without_payload() {
        use mondrian_core::NativeAudioReprepareRequest::ClapRestart;
        for (response, expected) in [
            (WorkerResponse::Completed, 0),
            (WorkerResponse::Failed("private".repeat(4096)), 1),
            (
                WorkerResponse::CompletedWithPending(crate::NativeObservedRequest::RequestOnly(
                    ClapRestart,
                )),
                2,
            ),
            (
                WorkerResponse::DeferredForConfiguration(
                    crate::NativeObservedRequest::RequestOnly(ClapRestart),
                ),
                3,
            ),
            (
                WorkerResponse::NotesCompleted { note_end_count: 0, pending: None },
                4,
            ),
        ] {
            assert_eq!(response_tag(&response), expected);
        }
    }
}
