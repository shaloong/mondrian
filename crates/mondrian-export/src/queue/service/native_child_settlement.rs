//! Audio child observations in the Queue's existing state and synchronization.
//! This owns no process, Runtime, teardown work or execution permission.
use super::*;
use mondrian_core::{
    NativeChildSettlement, NativeChildSettlementError as Error, NativeChildSettlementRegistrar,
    NativeChildSettlementReporter,
};

/// Original-owner observations for Audio Runtime native children since Queue start.
/// Empty is valid for work without native audio; missing JSON fields are rejected.
/// Exit/reap settlement does not assert graceful acknowledgement or OFX closure.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportNativeChildSettlementEvidence {
    /// Live counted capabilities that may register prospective Audio children.
    pub registration_scopes: u64,
    /// Actual original native owners registered over Queue lifetime.
    pub children_registered: u64,
    /// Owners that explicitly confirmed exit/reap after their own cleanup.
    pub children_settled: u64,
    /// Actual result obligations that have not yet finished.
    pub pending_children: u64,
    /// Sticky missing forwarding, unconfirmed result or counter/contract failure.
    pub unavailable: bool,
}

impl ExportNativeChildSettlementEvidence {
    /// Whether every registered native audio owner and prospective scope ended.
    #[must_use]
    pub const fn all_resources_released(self) -> bool {
        !self.unavailable
            && self.registration_scopes == 0
            && self.pending_children == 0
            && self.children_registered == self.children_settled
    }
}

impl RenderQueueInner {
    fn update_native_settlement<T>(&self, update: impl FnOnce(&mut ExportQueueState) -> T) -> T {
        let mut state = self.state.lock();
        let result = update(&mut state);
        self.mark_diagnostics_changed_locked(&state);
        drop(state);
        self.wake.notify_all();
        result
    }
}

impl NativeChildSettlementReporter for RenderQueueInner {
    fn begin_scope(&self) -> Result<(), Error> {
        self.update_native_settlement(|state| {
            let evidence = &mut state.audio_native_children;
            if !state.worker_running {
                evidence.unavailable = true;
                return Err(Error::Closed);
            }
            if evidence.unavailable {
                return Err(Error::Unavailable);
            }
            let Some(next) = evidence.registration_scopes.checked_add(1) else {
                evidence.unavailable = true;
                return Err(Error::Overflow);
            };
            evidence.registration_scopes = next;
            Ok(())
        })
    }

    fn end_scope(&self) {
        self.update_native_settlement(|state| {
            let evidence = &mut state.audio_native_children;
            match evidence.registration_scopes.checked_sub(1) {
                Some(next) => evidence.registration_scopes = next,
                None => evidence.unavailable = true,
            }
        });
    }

    fn begin_child(&self) -> Result<(), Error> {
        self.update_native_settlement(|state| {
            let evidence = &mut state.audio_native_children;
            if !state.worker_running {
                evidence.unavailable = true;
                return Err(Error::Closed);
            }
            if evidence.unavailable || evidence.registration_scopes == 0 {
                evidence.unavailable = true;
                return Err(Error::Unavailable);
            }
            let next = evidence
                .pending_children
                .checked_add(1)
                .zip(evidence.children_registered.checked_add(1));
            let Some((pending, registered)) = next else {
                evidence.unavailable = true;
                return Err(Error::Overflow);
            };
            evidence.pending_children = pending;
            evidence.children_registered = registered;
            Ok(())
        })
    }

    fn end_child(&self, settlement: NativeChildSettlement) {
        self.update_native_settlement(|state| {
            let evidence = &mut state.audio_native_children;
            match evidence.pending_children.checked_sub(1) {
                Some(next) => evidence.pending_children = next,
                None => {
                    evidence.unavailable = true;
                    return;
                }
            }
            if settlement == NativeChildSettlement::Unconfirmed {
                evidence.unavailable = true;
                return;
            }
            match evidence.children_settled.checked_add(1) {
                Some(next) if next <= evidence.children_registered => {
                    evidence.children_settled = next
                }
                _ => evidence.unavailable = true,
            }
        });
    }

    fn unavailable(&self) {
        self.update_native_settlement(|state| state.audio_native_children.unavailable = true);
    }
}

impl ExportExecutionGate {
    /// Observe before any cold resolver/factory call. Failure is sticky in the
    /// original Queue and cannot become a new DSP refusal. Sessions retain forks.
    pub(in crate::queue) fn native_child_settlement_registrar(
        &self,
    ) -> Option<NativeChildSettlementRegistrar> {
        match &self.authority {
            ExportExecutionGateAuthority::Queue { inner, .. } => {
                let reporter: Arc<dyn NativeChildSettlementReporter> = inner.clone();
                NativeChildSettlementRegistrar::try_new(reporter).ok()
            }
            #[cfg(test)]
            ExportExecutionGateAuthority::AlwaysOpen => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracker() -> Arc<RenderQueueInner> {
        Arc::new(RenderQueueInner {
            state: Mutex::new(ExportQueueState { worker_running: true, ..Default::default() }),
            audio_processor_resolver: Arc::new(BuiltInAudioProcessorResolver),
            wake: Condvar::new(),
            shutdown: AtomicBool::new(false),
            revision: AtomicU64::new(0),
            jobs_revision: AtomicU64::new(0),
        })
    }

    #[test]
    fn original_native_obligations_can_end_after_worker_closure() {
        let tracker = tracker();
        let root = NativeChildSettlementRegistrar::try_new(tracker.clone()).unwrap();
        let fork = root.try_fork().unwrap();
        let child = fork.try_child().unwrap();
        assert!(!tracker.state.lock().audio_native_children.all_resources_released());
        tracker.state.lock().worker_running = false;
        child.finish(NativeChildSettlement::Settled);
        drop(fork);
        drop(root);
        let evidence = tracker.state.lock().audio_native_children;
        assert!(evidence.all_resources_released());
        assert_eq!(
            (evidence.children_registered, evidence.children_settled),
            (1, 1)
        );
        assert_eq!(Arc::strong_count(&tracker), 1);
        assert_eq!(tracker.begin_scope(), Err(Error::Closed));
        assert!(!tracker.state.lock().audio_native_children.all_resources_released());
    }

    #[test]
    fn unconfirmed_underflow_and_missing_scope_are_permanent_failures() {
        for failure in 0..4 {
            let tracker = tracker();
            match failure {
                0 => {
                    let root = NativeChildSettlementRegistrar::try_new(tracker.clone()).unwrap();
                    drop(root.try_child().unwrap());
                    drop(root);
                }
                1 => tracker.end_scope(),
                2 => tracker.end_child(NativeChildSettlement::Settled),
                3 => assert_eq!(tracker.begin_child(), Err(Error::Unavailable)),
                _ => unreachable!(),
            }
            let evidence = tracker.state.lock().audio_native_children;
            assert!(evidence.unavailable);
            assert_eq!(
                (evidence.registration_scopes, evidence.pending_children),
                (0, 0)
            );
            assert!(!evidence.all_resources_released());
            assert!(matches!(
                NativeChildSettlementRegistrar::try_new(tracker),
                Err(Error::Unavailable)
            ));
        }
    }

    #[test]
    fn native_counter_overflow_is_checked_without_partial_registration() {
        let scope_overflow = tracker();
        scope_overflow.state.lock().audio_native_children.registration_scopes = u64::MAX;
        assert_eq!(scope_overflow.begin_scope(), Err(Error::Overflow));
        assert!(scope_overflow.state.lock().audio_native_children.unavailable);
        for registered_overflow in [false, true] {
            let tracker = tracker();
            let root = NativeChildSettlementRegistrar::try_new(tracker.clone()).unwrap();
            {
                let mut state = tracker.state.lock();
                if registered_overflow {
                    state.audio_native_children.children_registered = u64::MAX;
                } else {
                    state.audio_native_children.pending_children = u64::MAX;
                }
            }
            let before = tracker.state.lock().audio_native_children;
            assert!(matches!(root.try_child(), Err(Error::Overflow)));
            let after = tracker.state.lock().audio_native_children;
            assert_eq!(before.pending_children, after.pending_children);
            assert_eq!(before.children_registered, after.children_registered);
            assert!(after.unavailable);
        }
    }
}
