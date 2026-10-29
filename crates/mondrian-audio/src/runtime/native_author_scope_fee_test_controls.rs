//! Borrowed test observations of the actual Root-owned StateEntry cohort.

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ScopeLocalObservation {
    pub(crate) original_identity: crate::AudioSessionCursorIdentity,
    pub(crate) currently_stored_identity: crate::AudioSessionCursorIdentity,
    pub(crate) affected: bool,
    pub(crate) currently_stored_weight: crate::AudioSessionResourceFootprint,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ScopeCohortObservation {
    pub(crate) stable_box_address: usize,
    pub(crate) installed: bool,
    pub(crate) old_root: crate::AudioSessionCursorIdentity,
    pub(crate) locals: Vec<ScopeLocalObservation>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ScopeFeeObservation {
    pub(crate) root: crate::AudioSessionCursorIdentity,
    pub(crate) published: AudioRuntimeResourceFootprint,
    pub(crate) grant: AudioRuntimeResourceGrant,
    pub(crate) held_logical_bytes: usize,
    pub(crate) cohort: Option<ScopeCohortObservation>,
}

impl AudioProgramRuntime {
    pub(crate) fn observe_state_entry_fees_for_test(
        &self,
    ) -> Result<ScopeFeeObservation, AudioRuntimeBuildError> {
        let runtime = self;
        let cohort = runtime
            .native_author_scope
            .as_deref()
            .map(
                |cohort| -> Result<ScopeCohortObservation, AudioRuntimeBuildError> {
                    let locals = cohort
                        .locals
                        .iter()
                        .map(|local| {
                            Ok(ScopeLocalObservation {
                                original_identity: local.current,
                                currently_stored_identity: local.next.identity(),
                                affected: local.affected,
                                currently_stored_weight: local.next.current_resource_footprint()?,
                            })
                        })
                        .collect::<Result<Vec<_>, AudioRuntimeBuildError>>()?;
                    Ok(ScopeCohortObservation {
                        stable_box_address: std::ptr::from_ref(cohort) as usize,
                        installed: cohort.installed,
                        old_root: cohort.old_root,
                        locals,
                    })
                },
            )
            .transpose()?;
        Ok(ScopeFeeObservation {
            root: runtime.resource_root_identity(),
            published: runtime.resource_footprint(),
            grant: runtime.resource_grant(),
            held_logical_bytes: runtime.held_native_plan_logical_bytes(),
            cohort,
        })
    }
}

/// Read-only scalar lifetime witness. No Atomic, owner or issuer escapes.
pub(crate) struct LogicalFeeScopeWitness {
    scope: Arc<AtomicUsize>,
}

impl LogicalFeeScopeWitness {
    pub(crate) fn held_bytes(&self) -> usize {
        self.scope.load(Ordering::SeqCst)
    }
}

impl AudioProgramRuntime {
    pub(crate) fn state_entry_fee_witness_for_test(&self) -> Option<LogicalFeeScopeWitness> {
        self.native_held_plan_fees
            .as_ref()
            .map(|scope| LogicalFeeScopeWitness { scope: Arc::clone(scope) })
    }
}
