//! Consuming worker joins shared by Preview's media and auxiliary owners.
//!
//! A joined thread and a released unwind payload are independent facts. Never
//! run an opaque payload destructor on the caller's shutdown stack: it can
//! block, own foreign resources, or unwind a second time.

use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Outcome of synchronously reclaiming one Preview-owned worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewOwnedWorkerShutdown {
    /// No worker handle was created or retained for this owner.
    NotStarted,
    /// The real worker returned and was joined.
    Terminated,
    /// The worker was joined after unwinding with a safely released string payload.
    Panicked,
    /// The worker was joined, but its opaque unwind payload was deliberately retained.
    PanickedPayloadAbandoned,
    /// Joining on the same thread would deadlock, so closure is unproved.
    CurrentThreadSkipped,
    /// The shared deadline expired; the worker remains detached.
    TimedOutDetached,
}

impl PreviewOwnedWorkerShutdown {
    pub(crate) fn join(worker: JoinHandle<()>) -> Self {
        if worker.thread().id() == thread::current().id() {
            drop(worker);
            return Self::CurrentThreadSkipped;
        }
        Self::joined(worker.join())
    }

    pub(crate) fn join_until(worker: JoinHandle<()>, deadline: Instant) -> Self {
        if worker.thread().id() == thread::current().id() {
            drop(worker);
            return Self::CurrentThreadSkipped;
        }
        while !worker.is_finished() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        if !worker.is_finished() {
            drop(worker);
            return Self::TimedOutDetached;
        }
        Self::joined(worker.join())
    }

    fn joined(result: thread::Result<()>) -> Self {
        match result {
            Ok(()) => Self::Terminated,
            Err(payload) if payload.is::<&'static str>() || payload.is::<String>() => {
                drop(payload);
                Self::Panicked
            }
            Err(payload) => {
                std::mem::forget(payload);
                Self::PanickedPayloadAbandoned
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc};

    struct HostilePayload(Arc<AtomicUsize>);

    impl Drop for HostilePayload {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
            panic!("opaque payload destructor must never run during worker shutdown");
        }
    }

    #[test]
    fn both_join_interfaces_retain_opaque_payload_without_secondary_unwind() {
        for bounded in [false, true] {
            let drops = Arc::new(AtomicUsize::new(0));
            let payload = HostilePayload(Arc::clone(&drops));
            let worker = thread::spawn(move || std::panic::panic_any(payload));
            let outcome = if bounded {
                PreviewOwnedWorkerShutdown::join_until(
                    worker,
                    Instant::now() + Duration::from_secs(2),
                )
            } else {
                PreviewOwnedWorkerShutdown::join(worker)
            };
            assert_eq!(
                outcome,
                PreviewOwnedWorkerShutdown::PanickedPayloadAbandoned
            );
            assert_eq!(drops.load(Ordering::SeqCst), 0);
        }
    }

    #[test]
    fn known_string_panics_are_joined_and_not_abandoned() {
        for owned in [false, true] {
            let worker = thread::spawn(move || {
                if owned {
                    std::panic::panic_any(String::from("owned string"));
                }
                panic!("static string");
            });
            assert_eq!(
                PreviewOwnedWorkerShutdown::join(worker),
                PreviewOwnedWorkerShutdown::Panicked
            );
        }
    }

    #[test]
    fn expired_deadline_does_not_wait_for_a_running_worker() {
        let (release, released) = mpsc::channel();
        let (returned, done) = mpsc::channel();
        let worker = thread::spawn(move || {
            released.recv().expect("release worker after join returned");
            returned.send(()).expect("observe eventual return");
        });
        assert_eq!(
            PreviewOwnedWorkerShutdown::join_until(worker, Instant::now()),
            PreviewOwnedWorkerShutdown::TimedOutDetached
        );
        assert!(done.try_recv().is_err());
        release.send(()).expect("release detached worker");
        done.recv_timeout(Duration::from_secs(2)).expect("detached worker returned");
    }

    #[test]
    fn completed_worker_is_joined_even_after_deadline() {
        let worker = thread::spawn(|| {});
        while !worker.is_finished() {
            thread::yield_now();
        }
        assert_eq!(
            PreviewOwnedWorkerShutdown::join_until(worker, Instant::now()),
            PreviewOwnedWorkerShutdown::Terminated
        );
    }

    #[test]
    fn current_worker_handle_is_detached_without_self_join() {
        let (handle_tx, handle_rx) = mpsc::channel::<JoinHandle<()>>();
        let (result_tx, result_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let own_handle = handle_rx.recv().expect("receive own handle");
            result_tx
                .send(PreviewOwnedWorkerShutdown::join(own_handle))
                .expect("publish receipt");
        });
        handle_tx.send(worker).expect("transfer own handle");
        assert_eq!(
            result_rx.recv_timeout(Duration::from_secs(2)).expect("self join did not block"),
            PreviewOwnedWorkerShutdown::CurrentThreadSkipped
        );
    }
}
