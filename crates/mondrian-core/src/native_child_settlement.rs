//! Observation-only native child lifetime evidence in an existing owner domain.
//!
//! These counted capabilities never authorize spawning, DSP, installation or
//! retirement. The actual child owner alone classifies its final settlement.
use std::fmt;
use std::sync::Arc;

/// Final fact reported by the original native child owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeChildSettlement {
    /// The original owner confirmed exit/reap; graceful acknowledgement is separate.
    Settled,
    /// The original owner could not confirm settlement.
    Unconfirmed,
}

/// Missing or rejected evidence registration, never a DSP execution refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum NativeChildSettlementError {
    /// The domain or a required forwarding observer is unavailable.
    #[error("native child settlement evidence is unavailable")]
    Unavailable,
    /// The original domain has sealed new evidence registration.
    #[error("native child settlement registration is closed")]
    Closed,
    /// A checked evidence counter could not represent another registration.
    #[error("native child settlement evidence counter overflowed")]
    Overflow,
    /// An existing owner cannot move its evidence to another domain.
    #[error("native child settlement owner belongs to another domain")]
    ForeignDomain,
}

/// Prepared factory's explicit native child evidence requirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeChildSettlementRequirement {
    /// This trusted factory creates no native child owner.
    NotApplicable,
    /// Every actual instance must forward to its original native child owner.
    Required,
    /// No trustworthy statement about native child ownership is available.
    Unavailable,
}

/// Actual instance's evidence forwarding result, checked against its factory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeChildSettlementBinding {
    /// This actual trusted instance owns no native child.
    NotApplicable,
    /// The actual original child owner holds a result producer in this domain.
    Registered,
}

/// Trusted fixed-field evidence sink implemented by an existing owner tracker.
///
/// Implementations perform checked counters under their original synchronization,
/// with no foreign calls and no panics. Closed/overflow failures must first make
/// the domain permanently unavailable. End operations remain valid after sealing;
/// underflow and `Unconfirmed` are sticky failures. A reporter contract violation
/// must be caught and marked unavailable by the original outer owner boundary,
/// never swallowed as healthy evidence. No mutex poisoning behavior is assumed.
pub trait NativeChildSettlementReporter: Send + Sync {
    /// Register one live capability that can prospectively register children.
    fn begin_scope(&self) -> Result<(), NativeChildSettlementError>;
    /// Finish one previously successful scope registration.
    fn end_scope(&self);
    /// Register one result obligation for an actual original child owner.
    fn begin_child(&self) -> Result<(), NativeChildSettlementError>;
    /// Finish one previously successful child registration exactly once.
    fn end_child(&self, settlement: NativeChildSettlement);
    /// Permanently invalidate this domain's completeness evidence.
    fn unavailable(&self);
}

/// Counted registration capability; every live fork keeps its domain unresolved.
///
/// This is deliberately not `Clone`: a new capability must successfully register
/// its own scope. The observer's raw tracker Arc is not itself such a capability.
#[must_use]
pub struct NativeChildSettlementRegistrar {
    reporter: Option<Arc<dyn NativeChildSettlementReporter>>,
}

impl NativeChildSettlementRegistrar {
    /// Register a root capability against the same existing tracker allocation.
    pub fn try_new(
        reporter: Arc<dyn NativeChildSettlementReporter>,
    ) -> Result<Self, NativeChildSettlementError> {
        if let Err(error) = reporter.begin_scope() {
            reporter.unavailable();
            return Err(error);
        }
        Ok(Self { reporter: Some(reporter) })
    }

    /// Register another live capability before prospective owner construction.
    pub fn try_fork(&self) -> Result<Self, NativeChildSettlementError> {
        Self::try_new(Arc::clone(self.reporter()))
    }

    /// Register one actual child result obligation; no new execution is granted.
    pub fn try_child(&self) -> Result<NativeChildResultProducer, NativeChildSettlementError> {
        let reporter = self.reporter();
        if let Err(error) = reporter.begin_child() {
            reporter.unavailable();
            return Err(error);
        }
        Ok(NativeChildResultProducer { reporter: Some(Arc::clone(reporter)) })
    }

    /// Compare the original tracker allocation without issuing another domain ID.
    pub fn same_domain(&self, other: &Self) -> bool {
        Arc::ptr_eq(self.reporter(), other.reporter())
    }

    /// Preserve missing forwarding or rejected rebinding as a permanent failure.
    pub fn mark_unavailable(&self) {
        self.reporter().unavailable();
    }

    fn reporter(&self) -> &Arc<dyn NativeChildSettlementReporter> {
        self.reporter.as_ref().expect("a live registration capability is armed")
    }
}

impl Drop for NativeChildSettlementRegistrar {
    fn drop(&mut self) {
        // Disarm before the trusted callback: unwind cannot report twice.
        if let Some(reporter) = self.reporter.take() {
            reporter.end_scope();
        }
    }
}
impl fmt::Debug for NativeChildSettlementRegistrar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativeChildSettlementRegistrar").finish_non_exhaustive()
    }
}

/// Move-only result obligation held by the original actual native child owner.
///
/// It retains only the existing tracker Arc, never a Runtime, Session or Child.
/// An unreported Drop is `Unconfirmed`, even if an external handle saw an exit.
#[must_use]
pub struct NativeChildResultProducer {
    reporter: Option<Arc<dyn NativeChildSettlementReporter>>,
}
impl NativeChildResultProducer {
    /// Consume this obligation exactly once after the original owner's final cleanup.
    pub fn finish(mut self, settlement: NativeChildSettlement) {
        if let Some(reporter) = self.reporter.take() {
            reporter.end_child(settlement);
        }
    }

    /// Test idempotent forwarding to the same existing tracker allocation.
    pub fn same_domain(&self, registrar: &NativeChildSettlementRegistrar) -> bool {
        self.reporter
            .as_ref()
            .is_some_and(|reporter| Arc::ptr_eq(reporter, registrar.reporter()))
    }

    /// Preserve a forbidden foreign rebind in the original domain as well.
    pub fn mark_unavailable(&self) {
        if let Some(reporter) = &self.reporter {
            reporter.unavailable();
        }
    }
}
impl Drop for NativeChildResultProducer {
    fn drop(&mut self) {
        if let Some(reporter) = self.reporter.take() {
            reporter.end_child(NativeChildSettlement::Unconfirmed);
        }
    }
}
impl fmt::Debug for NativeChildResultProducer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativeChildResultProducer").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[derive(Default)]
    struct Reporter {
        scopes: AtomicUsize,
        children: AtomicUsize,
        finishes: AtomicUsize,
        unavailable: AtomicBool,
        sealed: AtomicBool,
        panic_end: AtomicBool,
    }
    impl Reporter {
        fn begin(&self, count: &AtomicUsize) -> Result<(), NativeChildSettlementError> {
            if self.sealed.load(Ordering::SeqCst) {
                self.unavailable();
                return Err(NativeChildSettlementError::Closed);
            }
            count
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_add(1))
                .map(|_| ())
                .map_err(|_| {
                    self.unavailable();
                    NativeChildSettlementError::Overflow
                })
        }
        fn end(&self, count: &AtomicUsize) {
            if count
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_err()
            {
                self.unavailable();
            }
        }
    }
    impl NativeChildSettlementReporter for Reporter {
        fn begin_scope(&self) -> Result<(), NativeChildSettlementError> {
            self.begin(&self.scopes)
        }
        fn end_scope(&self) {
            self.end(&self.scopes);
        }
        fn begin_child(&self) -> Result<(), NativeChildSettlementError> {
            self.begin(&self.children)
        }
        fn end_child(&self, settlement: NativeChildSettlement) {
            self.finishes.fetch_add(1, Ordering::SeqCst);
            self.end(&self.children);
            if settlement == NativeChildSettlement::Unconfirmed {
                self.unavailable();
            }
            if self.panic_end.swap(false, Ordering::SeqCst) {
                panic!("reporter contract violation");
            }
        }
        fn unavailable(&self) {
            self.unavailable.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn counted_forks_and_child_finish_use_one_domain_without_reverse_owner() {
        let tracker = Arc::new(Reporter::default());
        let root = NativeChildSettlementRegistrar::try_new(tracker.clone()).unwrap();
        let candidate = root.try_fork().unwrap();
        assert!(root.same_domain(&candidate));
        let child = candidate.try_child().unwrap();
        assert!(child.same_domain(&root));
        assert_eq!(tracker.scopes.load(Ordering::SeqCst), 2);
        drop(root);
        assert_eq!(tracker.scopes.load(Ordering::SeqCst), 1);
        child.finish(NativeChildSettlement::Settled);
        assert_eq!(tracker.finishes.load(Ordering::SeqCst), 1);
        assert_eq!(tracker.children.load(Ordering::SeqCst), 0);
        drop(candidate);
        assert_eq!(tracker.scopes.load(Ordering::SeqCst), 0);
        assert!(!tracker.unavailable.load(Ordering::SeqCst));
        assert_eq!(Arc::strong_count(&tracker), 1);
    }

    #[test]
    fn unreported_child_and_rejected_sealed_registration_stay_unavailable() {
        let tracker = Arc::new(Reporter::default());
        let root = NativeChildSettlementRegistrar::try_new(tracker.clone()).unwrap();
        drop(root.try_child().unwrap());
        assert!(tracker.unavailable.load(Ordering::SeqCst));
        tracker.sealed.store(true, Ordering::SeqCst);
        assert!(matches!(
            root.try_fork(),
            Err(NativeChildSettlementError::Closed)
        ));
        assert!(matches!(
            root.try_child(),
            Err(NativeChildSettlementError::Closed)
        ));
        assert_eq!(tracker.scopes.load(Ordering::SeqCst), 1);
        assert_eq!(tracker.children.load(Ordering::SeqCst), 0);
        drop(root);
        assert_eq!(tracker.scopes.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn registration_overflow_cannot_create_an_uncounted_capability() {
        let tracker = Arc::new(Reporter::default());
        tracker.scopes.store(usize::MAX, Ordering::SeqCst);
        assert!(matches!(
            NativeChildSettlementRegistrar::try_new(tracker.clone()),
            Err(NativeChildSettlementError::Overflow)
        ));
        assert_eq!(tracker.scopes.load(Ordering::SeqCst), usize::MAX);
        assert!(tracker.unavailable.load(Ordering::SeqCst));
        assert_eq!(Arc::strong_count(&tracker), 1);
    }

    #[test]
    fn reporter_contract_panic_after_finish_does_not_finish_again() {
        let tracker = Arc::new(Reporter::default());
        let root = NativeChildSettlementRegistrar::try_new(tracker.clone()).unwrap();
        let child = root.try_child().unwrap();
        tracker.panic_end.store(true, Ordering::SeqCst);
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            child.finish(NativeChildSettlement::Settled);
        }));
        assert!(caught.is_err());
        // Model the ORIGINAL outer boundary's required contract-violation report.
        root.mark_unavailable();
        assert_eq!(tracker.finishes.load(Ordering::SeqCst), 1);
        assert_eq!(tracker.children.load(Ordering::SeqCst), 0);
        assert!(tracker.unavailable.load(Ordering::SeqCst));
    }
}
