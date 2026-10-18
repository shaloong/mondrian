//! Fixed native-child observation in Audio Playback's existing retirement tracker.
//!
//! No child handle, execution authority, registry or additional allocation is
//! retained. The counters describe this same owner domain; they do not cover
//! unbound construction or make ordinary nonwaiting Drop a bounded closure.
use super::*;

#[derive(Default)]
struct RetirementState {
    evidence: ForeignOwnerRetirementEvidence,
    scopes: u32,
    children: u32,
    sealed: bool,
    unavailable: bool,
}

#[derive(Default)]
pub(super) struct ForeignOwnerRetirementTracker {
    state: Mutex<RetirementState>,
}

impl ForeignOwnerRetirementTracker {
    pub(super) fn record(&self, evidence: ForeignOwnerRetirementEvidence) {
        let mut state = self.state.lock();
        state.evidence.panics = state.evidence.panics.saturating_add(evidence.panics);
        state.evidence.owner_abandonments =
            state.evidence.owner_abandonments.saturating_add(evidence.owner_abandonments);
    }

    pub(super) fn snapshot(&self) -> ForeignOwnerRetirementEvidence {
        self.state.lock().evidence
    }

    pub(super) fn seal(&self) -> bool {
        let mut state = self.state.lock();
        state.sealed = true;
        !state.unavailable && state.scopes == 0 && state.children == 0
    }
}

impl mondrian_core::NativeChildSettlementReporter for ForeignOwnerRetirementTracker {
    fn begin_scope(&self) -> Result<(), mondrian_core::NativeChildSettlementError> {
        use mondrian_core::NativeChildSettlementError as Error;
        let mut state = self.state.lock();
        if state.sealed {
            state.unavailable = true;
            return Err(Error::Closed);
        }
        if state.unavailable {
            return Err(Error::Unavailable);
        }
        let Some(next) = state.scopes.checked_add(1) else {
            state.unavailable = true;
            return Err(Error::Overflow);
        };
        state.scopes = next;
        Ok(())
    }

    fn end_scope(&self) {
        let mut state = self.state.lock();
        match state.scopes.checked_sub(1) {
            Some(next) => state.scopes = next,
            None => state.unavailable = true,
        }
    }

    fn begin_child(&self) -> Result<(), mondrian_core::NativeChildSettlementError> {
        use mondrian_core::NativeChildSettlementError as Error;
        let mut state = self.state.lock();
        if state.sealed {
            state.unavailable = true;
            return Err(Error::Closed);
        }
        if state.unavailable || state.scopes == 0 {
            state.unavailable = true;
            return Err(Error::Unavailable);
        }
        let Some(next) = state.children.checked_add(1) else {
            state.unavailable = true;
            return Err(Error::Overflow);
        };
        state.children = next;
        Ok(())
    }

    fn end_child(&self, settlement: mondrian_core::NativeChildSettlement) {
        let mut state = self.state.lock();
        if settlement == mondrian_core::NativeChildSettlement::Unconfirmed {
            state.unavailable = true;
        }
        match state.children.checked_sub(1) {
            Some(next) => state.children = next,
            None => state.unavailable = true,
        }
    }

    fn unavailable(&self) {
        self.state.lock().unavailable = true;
    }
}

impl AudioPlayback {
    /// Observe cold renderer construction in this Playback's existing owner domain.
    ///
    /// Acquire before invoking any resolver or factory. Failure is already sticky
    /// in the original tracker and must not become a new execution refusal. This
    /// counted capability retains no Playback, device, renderer or child handle.
    pub fn native_child_settlement_registrar(
        &self,
    ) -> Result<
        mondrian_core::NativeChildSettlementRegistrar,
        mondrian_core::NativeChildSettlementError,
    > {
        let reporter: Arc<dyn mondrian_core::NativeChildSettlementReporter> =
            self.foreign_owner_retirements.clone();
        mondrian_core::NativeChildSettlementRegistrar::try_new(reporter)
    }

    pub(super) fn observe_native_child_settlement(&self, renderer: &Arc<dyn AudioPcmRenderer>) {
        let registrar = match self.native_child_settlement_registrar() {
            Ok(registrar) => registrar,
            Err(_) => return, // The original reporter has already recorded failure.
        };
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            renderer.bind_native_child_settlement(&registrar)
        })) {
            Ok(Ok(())) => {}
            Ok(Err(_)) => registrar.mark_unavailable(),
            Err(payload) => {
                registrar.mark_unavailable();
                let abandoned = dispose_canonical_or_abandon_opaque_panic_payload(payload);
                self.foreign_owner_retirements.record(ForeignOwnerRetirementEvidence {
                    panics: 1,
                    owner_abandonments: u32::from(abandoned),
                });
            }
        }
        // This local scope ends; a real renderer/Session must hold its own fork.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::{
        NativeChildSettlement as Settlement, NativeChildSettlementError as Error,
        NativeChildSettlementRegistrar as Registrar, NativeChildSettlementReporter,
    };

    #[test]
    fn counted_native_scopes_and_children_can_finish_after_sealing() {
        let tracker = Arc::new(ForeignOwnerRetirementTracker::default());
        let root = Registrar::try_new(tracker.clone()).expect("counted root");
        let fork = root.try_fork().expect("counted candidate");
        let child = fork.try_child().expect("original owner result obligation");
        assert!(root.same_domain(&fork));
        assert!(child.same_domain(&root));
        {
            let state = tracker.state.lock();
            assert_eq!((state.scopes, state.children), (2, 1));
        }
        assert!(
            !tracker.seal(),
            "live capabilities cannot prove closed ownership"
        );
        child.finish(Settlement::Settled);
        drop(fork);
        drop(root);
        assert!(tracker.seal(), "actual ends remain legal after seal");
        assert_eq!(Arc::strong_count(&tracker), 1, "no reverse execution owner");
    }

    #[test]
    fn native_registration_refuses_missing_scope_overflow_and_closed_domain() {
        let missing = Arc::new(ForeignOwnerRetirementTracker::default());
        assert_eq!(missing.begin_child(), Err(Error::Unavailable));
        assert!(matches!(
            Registrar::try_new(missing.clone()),
            Err(Error::Unavailable)
        ));
        assert_eq!(missing.state.lock().scopes, 0);
        assert_eq!(missing.state.lock().children, 0);
        assert!(!missing.seal());

        let overflow = Arc::new(ForeignOwnerRetirementTracker::default());
        overflow.state.lock().scopes = u32::MAX;
        assert!(matches!(
            Registrar::try_new(overflow.clone()),
            Err(Error::Overflow)
        ));
        assert_eq!(overflow.state.lock().scopes, u32::MAX);
        assert_eq!(Arc::strong_count(&overflow), 1);
        assert!(!overflow.seal());

        let child_overflow = Arc::new(ForeignOwnerRetirementTracker::default());
        let root = Registrar::try_new(child_overflow.clone()).expect("counted root");
        child_overflow.state.lock().children = u32::MAX;
        assert!(matches!(root.try_child(), Err(Error::Overflow)));
        assert_eq!(child_overflow.state.lock().children, u32::MAX);
        drop(root);
        assert_eq!(child_overflow.state.lock().scopes, 0);
        assert!(!child_overflow.seal());

        let closed = Arc::new(ForeignOwnerRetirementTracker::default());
        let root = Registrar::try_new(closed.clone()).expect("counted root");
        assert!(!closed.seal());
        assert!(matches!(root.try_fork(), Err(Error::Closed)));
        assert!(matches!(root.try_child(), Err(Error::Closed)));
        assert_eq!(closed.state.lock().scopes, 1);
        assert_eq!(closed.state.lock().children, 0);
        drop(root);
        assert!(
            !closed.seal(),
            "failed mint is sticky after original scope ends"
        );
    }

    #[test]
    fn native_failure_never_clears_original_retirement_evidence() {
        for unconfirmed in [false, true] {
            let tracker = Arc::new(ForeignOwnerRetirementTracker::default());
            tracker.record(ForeignOwnerRetirementEvidence { panics: 2, owner_abandonments: 3 });
            if unconfirmed {
                let root = Registrar::try_new(tracker.clone()).expect("counted root");
                drop(root.try_child().expect("actual result obligation"));
                drop(root);
            } else {
                tracker.end_scope();
                tracker.end_child(Settlement::Settled);
            }
            assert!(!tracker.seal(), "unconfirmed/underflow stays unavailable");
            assert_eq!(
                tracker.snapshot(),
                ForeignOwnerRetirementEvidence { panics: 2, owner_abandonments: 3 }
            );
            assert_eq!(tracker.state.lock().scopes, 0);
            assert_eq!(tracker.state.lock().children, 0);
        }
        let healthy_native = ForeignOwnerRetirementTracker::default();
        healthy_native.record(ForeignOwnerRetirementEvidence { panics: 1, owner_abandonments: 1 });
        assert!(healthy_native.seal());
        assert_eq!(
            healthy_native.snapshot(),
            ForeignOwnerRetirementEvidence { panics: 1, owner_abandonments: 1 },
            "healthy native counters cannot erase previous foreign failures"
        );
    }

    #[test]
    fn native_counters_extend_the_same_actual_tracker_payload() {
        #[derive(Default)]
        struct PriorTracker {
            evidence: Mutex<ForeignOwnerRetirementEvidence>,
        }
        let tracker = Arc::new(ForeignOwnerRetirementTracker::default());
        let before = PriorTracker::default();
        assert_eq!(*before.evidence.lock(), tracker.snapshot());
        let prior_layout = std::alloc::Layout::for_value(&before);
        let actual_layout = std::alloc::Layout::for_value(tracker.as_ref());
        assert_eq!(
            actual_layout,
            std::alloc::Layout::new::<ForeignOwnerRetirementTracker>()
        );
        assert!(actual_layout.size() > prior_layout.size());
        // This measures only the real payload delta. Arc's private prefix and
        // allocator/platform storage are not a complete heap or Root fee claim.
        let registrar = Registrar::try_new(tracker.clone()).expect("same tracker Arc");
        assert_eq!(Arc::strong_count(&tracker), 2);
        drop(registrar);
        assert_eq!(Arc::strong_count(&tracker), 1);
    }
}
