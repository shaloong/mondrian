//! Wait on the existing Child without changing process ownership or shutdown policy.
//! The OS wait can round up; only a try_wait result observed before the original
//! absolute deadline is accepted. This is deliberately stricter than checking
//! the clock only before a nonblocking query.

use super::shutdown_observation::{ChildObservation, IoFailure};
use std::io;
use std::process::{Child, ExitStatus};
use std::time::{Duration, Instant};

#[derive(Debug)]
pub(super) enum ExitWaitOutcome {
    Exited(ExitStatus),
    Deadline(Option<ChildObservation>),
    Io(IoFailure),
}

enum WaitObservation {
    Pending,
    Exited(ExitStatus),
}

pub(super) fn wait_for_exit(child: &mut Child, deadline: Instant) -> ExitWaitOutcome {
    wait_until(deadline, Instant::now, |remaining| {
        platform_wait(child, remaining)
    })
}

// The two closures are a private clock/result test seam, not another waiter or
// owner. Production supplies the real monotonic clock and the same Child.
fn wait_until(
    deadline: Instant,
    mut now: impl FnMut() -> Instant,
    mut wait: impl FnMut(Duration) -> io::Result<WaitObservation>,
) -> ExitWaitOutcome {
    let mut last_observation = None;
    loop {
        let Some(remaining) = deadline.checked_duration_since(now()).filter(|time| !time.is_zero())
        else {
            return ExitWaitOutcome::Deadline(last_observation);
        };
        match wait(remaining) {
            Ok(WaitObservation::Pending) => {
                last_observation = Some(ChildObservation::Running);
                // Early timeouts never renew the absolute deadline.
            }
            Ok(WaitObservation::Exited(status)) => {
                return if now() < deadline {
                    ExitWaitOutcome::Exited(status)
                } else {
                    // Retain the real late status as diagnosis, never as success.
                    ExitWaitOutcome::Deadline(Some(ChildObservation::Exited(status)))
                };
            }
            Err(error) => return ExitWaitOutcome::Io((&error).into()),
        }
    }
}

#[cfg(windows)]
fn platform_wait(child: &mut Child, remaining: Duration) -> io::Result<WaitObservation> {
    use std::os::windows::io::AsRawHandle;

    if windows::wait_for_handle(child.as_raw_handle(), windows::finite_millis(remaining))? {
        // The borrowed handle remains owned by Child throughout the wait and
        // this original try_wait. There is no duplicate handle or exit-code query.
        signaled_status(child.try_wait())
    } else {
        Ok(WaitObservation::Pending)
    }
}

#[cfg(windows)]
fn signaled_status(observed: io::Result<Option<ExitStatus>>) -> io::Result<WaitObservation> {
    match observed? {
        Some(status) => Ok(WaitObservation::Exited(status)),
        None => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "signaled Child did not provide an exit status",
        )),
    }
}

#[cfg(not(windows))]
fn platform_wait(child: &mut Child, remaining: Duration) -> io::Result<WaitObservation> {
    // Portable fallback only; this does not claim native-platform qualification.
    // A bounded sleep prevents the previous yield/try_wait busy loop.
    std::thread::sleep(remaining.min(Duration::from_millis(1)));
    Ok(match child.try_wait()? {
        Some(status) => WaitObservation::Exited(status),
        None => WaitObservation::Pending,
    })
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::ffi::c_void;

    const WAIT_OBJECT_0: u32 = 0;
    const WAIT_TIMEOUT: u32 = 258;
    const WAIT_FAILED: u32 = u32::MAX;
    const MAX_FINITE_MILLIS: u32 = u32::MAX - 1;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn WaitForSingleObject(handle: *mut c_void, milliseconds: u32) -> u32;
    }

    pub(super) fn finite_millis(remaining: Duration) -> u32 {
        // Ceil sub-ms waits, saturate below INFINITE. The caller still checks
        // its unchanged Instant deadline after observing an exit status.
        remaining.as_nanos().div_ceil(1_000_000).min(u128::from(MAX_FINITE_MILLIS)) as u32
    }

    pub(super) fn wait_for_handle(handle: *mut c_void, milliseconds: u32) -> io::Result<bool> {
        // SAFETY: production borrows the live process handle from Child, which
        // cannot be dropped or closed during this call. Windows validates raw
        // handles (including the deliberately invalid test value); Rust never
        // dereferences this pointer. This function neither closes nor owns it.
        let result = unsafe { WaitForSingleObject(handle, milliseconds) };
        classify_wait_result(result)
    }

    fn classify_wait_result(result: u32) -> io::Result<bool> {
        match result {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            WAIT_FAILED => Err(io::Error::last_os_error()),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected process wait result",
            )),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn native_exit_wait_milliseconds_are_finite_and_round_up_without_renewing_budget() {
            for (remaining, expected) in [
                (Duration::ZERO, 0),
                (Duration::from_nanos(1), 1),
                (Duration::from_micros(999), 1),
                (Duration::from_millis(1), 1),
                (Duration::from_micros(1_001), 2),
                (Duration::from_millis(100), 100),
                (
                    Duration::from_millis(u64::from(MAX_FINITE_MILLIS)),
                    MAX_FINITE_MILLIS,
                ),
                (
                    Duration::from_millis(u64::from(u32::MAX)),
                    MAX_FINITE_MILLIS,
                ),
                (Duration::MAX, MAX_FINITE_MILLIS),
            ] {
                assert_eq!(finite_millis(remaining), expected);
                assert_ne!(finite_millis(remaining), u32::MAX);
            }
        }

        #[test]
        fn native_exit_wait_only_process_signal_and_timeout_are_valid_wait_results() {
            assert!(classify_wait_result(WAIT_OBJECT_0).unwrap());
            assert!(!classify_wait_result(WAIT_TIMEOUT).unwrap());
            for unexpected in [1, 128, u32::MAX - 1] {
                assert_eq!(
                    classify_wait_result(unexpected).unwrap_err().kind(),
                    io::ErrorKind::InvalidData
                );
            }
        }

        #[test]
        fn native_exit_wait_invalid_raw_handle_preserves_wait_failed_os_error() {
            // No invalid BorrowedHandle is constructed: the kernel checks the
            // raw scalar, with no Rust ownership/validity promise attached.
            let error = wait_for_handle(std::ptr::null_mut(), 1).unwrap_err();
            assert_eq!(error.raw_os_error(), Some(6)); // ERROR_INVALID_HANDLE
        }
    }
}

#[cfg(all(test, any(windows, unix)))]
mod tests {
    use super::*;

    fn status(code: u32) -> ExitStatus {
        #[cfg(windows)]
        {
            use std::os::windows::process::ExitStatusExt;
            ExitStatus::from_raw(code)
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            ExitStatus::from_raw((code as i32) << 8)
        }
    }

    #[test]
    fn native_exit_wait_zero_budget_never_queries_or_accepts_an_exit() {
        let deadline = Instant::now();
        assert!(matches!(
            wait_until(deadline, || deadline, |_| panic!("zero budget cannot wait")),
            ExitWaitOutcome::Deadline(None)
        ));
    }

    #[test]
    fn native_exit_wait_early_timeouts_use_only_remaining_and_keep_nonzero_status() {
        let start = Instant::now();
        let mut times = [0, 1, 3, 4].into_iter();
        let mut waits = Vec::new();
        let result = wait_until(
            start + Duration::from_millis(10),
            || start + Duration::from_millis(times.next().expect("finite clock observations")),
            |remaining| {
                waits.push(remaining);
                Ok(if waits.len() < 3 {
                    WaitObservation::Pending
                } else {
                    WaitObservation::Exited(status(7))
                })
            },
        );
        assert_eq!(
            waits,
            [
                Duration::from_millis(10),
                Duration::from_millis(9),
                Duration::from_millis(7)
            ]
        );
        assert!(
            matches!(result, ExitWaitOutcome::Exited(actual) if actual.code() == Some(7) && !actual.success())
        );
        assert!(times.next().is_none());
    }

    #[test]
    fn native_exit_wait_timeout_at_deadline_does_not_poll_again() {
        let start = Instant::now();
        let deadline = start + Duration::from_millis(1);
        let mut times = [start, deadline].into_iter();
        let mut calls = 0;
        let result = wait_until(
            deadline,
            || times.next().expect("no clock query after deadline"),
            |remaining| {
                calls += 1;
                assert_eq!(remaining, Duration::from_millis(1));
                Ok(WaitObservation::Pending)
            },
        );
        assert_eq!(calls, 1);
        assert!(matches!(
            result,
            ExitWaitOutcome::Deadline(Some(ChildObservation::Running))
        ));
        assert!(times.next().is_none());
    }

    #[test]
    fn native_exit_wait_signaled_status_at_or_after_deadline_stays_failed() {
        let start = Instant::now();
        let deadline = start + Duration::from_micros(500);
        for late in [deadline, deadline + Duration::from_nanos(1)] {
            let mut times = [start, late].into_iter();
            let result = wait_until(
                deadline,
                || times.next().unwrap(),
                |remaining| {
                    assert_eq!(remaining, Duration::from_micros(500));
                    Ok(WaitObservation::Exited(status(0)))
                },
            );
            assert!(
                matches!(result, ExitWaitOutcome::Deadline(Some(ChildObservation::Exited(actual))) if actual.success())
            );
            assert!(times.next().is_none());
        }
    }

    #[test]
    fn native_exit_wait_io_failure_is_immediate_and_retains_os_error() {
        let start = Instant::now();
        let mut clocks = 0;
        let result = wait_until(
            start + Duration::from_secs(1),
            || {
                clocks += 1;
                start
            },
            |_| Err(io::Error::from_raw_os_error(6)),
        );
        assert_eq!(clocks, 1);
        let ExitWaitOutcome::Io(error) = result else {
            panic!("original I/O failure")
        };
        assert_eq!(error, (&io::Error::from_raw_os_error(6)).into());
    }

    #[cfg(windows)]
    #[test]
    fn native_exit_wait_signaled_without_status_is_io_not_success_or_retry() {
        let error = signaled_status(Ok(None)).err().expect("impossible status is a refusal");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        let actual = status(7);
        assert!(
            matches!(signaled_status(Ok(Some(actual))).unwrap(), WaitObservation::Exited(value) if value == actual)
        );
        let original = io::Error::from_raw_os_error(6);
        assert_eq!(
            signaled_status(Err(original)).err().unwrap().raw_os_error(),
            Some(6)
        );
    }

    #[test]
    fn native_exit_wait_late_exit_zero_settlement_cannot_replace_primary_deadline() {
        use super::super::shutdown_observation::{ShutdownFailure, ShutdownReport, Timed};
        use super::super::ProcessState;

        let mut report = ShutdownReport::new(1, ProcessState::Retiring, Duration::from_millis(100));
        report.primary_failure = Some(ShutdownFailure::ExitObservationDeadline);
        report.exit_loop = Some(Timed::new(
            report.started,
            report.started,
            ChildObservation::Exited(status(0)),
        ));
        report.cleanup.initial_observe = Some(Timed::new(
            report.started,
            report.started,
            ChildObservation::Exited(status(0)),
        ));
        let report = report.finish(false, ProcessState::Settled);
        assert!(!report.successful());
        assert_eq!(
            report.primary_failure,
            Some(ShutdownFailure::ExitObservationDeadline)
        );
        assert!(report.validation_result().is_err());
    }
}
