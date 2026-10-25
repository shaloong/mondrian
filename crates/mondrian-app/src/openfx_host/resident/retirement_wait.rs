//! Finite Windows waiting on the original Child's borrowed process handle.
//! Deadline, graceful status and settlement remain the resident owner's policy.

use super::close_observation::ExitQueryProbe;
use std::ffi::c_void;
use std::io;
use std::os::windows::io::AsRawHandle;
use std::process::{Child, ExitStatus};
use std::time::Duration;

const WAIT_OBJECT_0: u32 = 0;
const WAIT_TIMEOUT: u32 = 258;
const WAIT_FAILED: u32 = u32::MAX;
const MAX_FINITE_MILLIS: u32 = u32::MAX - 1;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn WaitForSingleObject(handle: *mut c_void, milliseconds: u32) -> u32;
}

fn finite_millis(remaining: Duration) -> u32 {
    remaining.as_nanos().div_ceil(1_000_000).min(u128::from(MAX_FINITE_MILLIS)) as u32
}

fn wait_for_handle(handle: *mut c_void, milliseconds: u32) -> io::Result<bool> {
    // SAFETY: the caller borrows the sole live Child throughout this synchronous
    // wait. No handle is duplicated, opened, closed or assigned another owner.
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

fn wait_then_query(
    wait: impl FnOnce() -> io::Result<bool>,
    query: impl FnOnce() -> io::Result<Option<ExitStatus>>,
) -> io::Result<Option<ExitStatus>> {
    if !wait()? {
        return Ok(None);
    }
    let status = query()?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "signaled Child did not provide an exit status",
        )
    })?;
    Ok(Some(status))
}

pub(super) fn wait_for_exit(
    child: &mut Child,
    remaining: Duration,
    queries: Option<&mut ExitQueryProbe>,
) -> io::Result<Option<ExitStatus>> {
    let handle = child.as_raw_handle();
    wait_then_query(
        || wait_for_handle(handle, finite_millis(remaining)),
        || match queries {
            Some(queries) => queries.observe(|| child.try_wait()),
            None => child.try_wait(),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::process::ExitStatusExt;
    use std::time::Instant;

    #[test]
    fn finite_wait_milliseconds_ceil_without_ever_selecting_infinite() {
        for (remaining, expected) in [
            (Duration::ZERO, 0),
            (Duration::from_nanos(1), 1),
            (Duration::from_micros(999), 1),
            (Duration::from_millis(1), 1),
            (Duration::from_micros(1_001), 2),
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
    fn only_a_signaled_wait_queries_the_original_child_status() {
        assert!(
            wait_then_query(|| Ok(false), || panic!("timeout must not query status"))
                .unwrap()
                .is_none()
        );
        for code in [0, 7] {
            let actual = wait_then_query(|| Ok(true), || Ok(Some(ExitStatus::from_raw(code))))
                .unwrap()
                .unwrap();
            assert_eq!(actual.code(), Some(code as i32));
        }
        assert_eq!(
            wait_then_query(|| Ok(true), || Ok(None)).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            wait_then_query(|| Ok(true), || Err(io::Error::from_raw_os_error(6)))
                .unwrap_err()
                .raw_os_error(),
            Some(6)
        );
        assert_eq!(
            wait_then_query(
                || Err(io::Error::from_raw_os_error(87)),
                || panic!("failed wait must not query status")
            )
            .unwrap_err()
            .raw_os_error(),
            Some(87)
        );
    }

    #[test]
    fn event_timeouts_do_not_renew_deadline_or_hide_a_late_exit() {
        let start = Instant::now();
        let deadline = start + Duration::from_millis(10);
        let mut clocks = [
            start,
            start + Duration::from_millis(1),
            deadline - Duration::from_nanos(1),
            deadline,
        ]
        .into_iter();
        let pending = super::super::observe_retirement_exit(
            deadline,
            || clocks.next().unwrap(),
            || wait_then_query(|| Ok(false), || panic!("early timeout does not query")),
        )
        .unwrap();
        assert!(pending.is_none());
        let late = super::super::observe_retirement_exit(
            deadline,
            || clocks.next().unwrap(),
            || wait_then_query(|| Ok(true), || Ok(Some(ExitStatus::from_raw(0)))),
        )
        .unwrap()
        .expect("late actual status remains available for settlement");
        assert!(late.status.success());
        assert!(matches!(
            late.graceful,
            Err(super::super::OpenFxHostError::DeadlineExceeded)
        ));
        assert!(clocks.next().is_none());
    }

    #[test]
    fn native_event_wait_rejects_unknown_results_and_preserves_wait_failed_os_error() {
        assert!(classify_wait_result(WAIT_OBJECT_0).unwrap());
        assert!(!classify_wait_result(WAIT_TIMEOUT).unwrap());
        for unexpected in [1, 128, u32::MAX - 1] {
            assert_eq!(
                classify_wait_result(unexpected).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
        // The OS validates this scalar; no invalid BorrowedHandle is created.
        assert_eq!(
            wait_for_handle(std::ptr::null_mut(), 1).unwrap_err().raw_os_error(),
            Some(6)
        );
    }
}
