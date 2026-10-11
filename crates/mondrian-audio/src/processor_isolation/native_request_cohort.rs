//! Bounded symbolic request cohorts inside one native child callback owner.

use std::sync::atomic::{AtomicU64, Ordering};

/// An exact recorded callback generation and its coalesced raw flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RecordedRequestCohort(u64);

impl RecordedRequestCohort {
    pub(super) const fn from_word(word: u64) -> Self {
        Self(word)
    }

    pub(super) const fn serial(self) -> u32 {
        (self.0 >> 32) as u32
    }

    pub(super) const fn flags(self) -> u32 {
        self.0 as u32
    }

    pub(super) const fn exhausted(self) -> bool {
        self.serial() == u32::MAX
    }

    pub(super) const fn pending(self) -> bool {
        self.flags() != 0 || self.exhausted()
    }
}

/// One atomic authority for callbacks and exact service retirement.
pub(super) struct NativeRequestMarker {
    word: AtomicU64,
}

impl NativeRequestMarker {
    pub(super) const fn new() -> Self {
        Self { word: AtomicU64::new(0) }
    }

    pub(super) fn snapshot(&self) -> RecordedRequestCohort {
        RecordedRequestCohort::from_word(self.word.load(Ordering::Acquire))
    }

    /// Record every nonempty callback, even if its bit was already pending.
    ///
    /// The maximum serial is a sticky sentinel. Its flags remain observable,
    /// and callers cannot acknowledge it or wrap to a previously used serial.
    pub(super) fn record(&self, flags: u32) {
        if flags == 0 {
            return;
        }
        let mut previous = self.word.load(Ordering::Acquire);
        loop {
            let serial = (previous >> 32) as u32;
            let next_serial = serial.saturating_add(1);
            let next = (u64::from(next_serial) << 32) | u64::from(previous as u32 | flags);
            match self.word.compare_exchange_weak(
                previous,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(observed) => previous = observed,
            }
        }
    }

    /// Linearize an unchanged already-retired serial without clearing newer flags.
    ///
    /// A callback after this claim is a later pending generation and must still
    /// be queried before the next root DSP window. This does not inhibit callbacks.
    pub(super) fn claim_retired_serial(&self, serial: u32) -> bool {
        if serial == 0 || serial == u32::MAX {
            return false;
        }
        let expected = u64::from(serial) << 32;
        self.word
            .compare_exchange(expected, expected, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// Retire only the exact nonempty, nonexhausted cohort actually serviced.
    pub(super) fn retire(&self, expected: RecordedRequestCohort) -> bool {
        if !expected.pending() || expected.exhausted() {
            return false;
        }
        self.word
            .compare_exchange(
                expected.0,
                u64::from(expected.serial()) << 32,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn native_request_cohort_retirement_retains_same_bit_and_unknown_bits() {
        let marker = NativeRequestMarker::new();
        marker.record(1);
        let serviced = marker.snapshot();
        marker.record(1);
        assert!(
            !marker.retire(serviced),
            "a same-bit callback is a new cohort"
        );
        marker.record(1 << 31);
        let current = marker.snapshot();
        assert_eq!(current.serial(), 3);
        assert_eq!(current.flags(), 1 | (1 << 31));
        assert!(marker.retire(current));
        let empty = marker.snapshot();
        assert_eq!(empty.serial(), 3);
        assert_eq!(empty.flags(), 0);
        assert!(!marker.retire(empty));
        marker.record(1);
        assert_eq!(marker.snapshot().serial(), 4);
        assert!(
            !marker.retire(current),
            "an old token cannot clear another generation"
        );
    }

    #[test]
    fn native_request_cohort_exhaustion_is_sticky_and_nonretirable() {
        let marker = NativeRequestMarker {
            word: AtomicU64::new(u64::from(u32::MAX - 1) << 32),
        };
        marker.record(1);
        let exhausted = marker.snapshot();
        assert!(exhausted.exhausted());
        assert!(!marker.retire(exhausted));
        marker.record(1 << 29);
        assert_eq!(marker.snapshot().serial(), u32::MAX);
        assert_eq!(marker.snapshot().flags(), 1 | (1 << 29));
        marker.record(0);
        assert_eq!(marker.snapshot().serial(), u32::MAX);
    }

    #[test]
    fn native_request_cohort_concurrent_callbacks_preserve_serial_and_union() {
        let marker = Arc::new(NativeRequestMarker::new());
        let threads = (0..4)
            .map(|bit| {
                let marker = marker.clone();
                std::thread::spawn(move || {
                    for _ in 0..1_000 {
                        marker.record(1 << bit);
                    }
                })
            })
            .collect::<Vec<_>>();
        for thread in threads {
            thread.join().expect("callback thread");
        }
        let current = marker.snapshot();
        assert_eq!(current.serial(), 4_000);
        assert_eq!(current.flags(), 15);
        assert!(marker.retire(current));
    }
}
