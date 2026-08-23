//! Monotonic runtime-clock Interface shared by production and Headless Adapters.

use crate::MonotonicTimestamp;
use std::time::Instant;

/// Process-local elapsed-time source for scheduling and evidence.
///
/// This is not a playback Clock Master and never represents authored media
/// time. Implementations must return a nondecreasing timestamp. Modules using
/// this Interface still fail closed and report regressions from faulty Adapters.
/// `now` may be sampled while lifecycle state is locked, so it must not block,
/// perform I/O, or re-enter the calling Module.
pub trait MonotonicRuntimeClock: Send + Sync + 'static {
    /// Sample the current process-local monotonic timestamp.
    fn now(&self) -> MonotonicTimestamp;
}

/// Production Adapter backed by Rust's monotonic [`Instant`].
#[derive(Debug)]
pub struct SystemMonotonicRuntimeClock {
    origin: Instant,
}

impl Default for SystemMonotonicRuntimeClock {
    fn default() -> Self {
        Self { origin: Instant::now() }
    }
}

impl MonotonicRuntimeClock for SystemMonotonicRuntimeClock {
    fn now(&self) -> MonotonicTimestamp {
        MonotonicTimestamp::from_duration(self.origin.elapsed())
    }
}
