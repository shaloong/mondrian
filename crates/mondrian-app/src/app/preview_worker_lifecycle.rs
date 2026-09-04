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
#[path = "../../tests/protocol/preview_worker_lifecycle.rs"]
mod tests;
