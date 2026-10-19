//! Extra immutable Plan roles owned by actual prepared/retired handoff storage.
//! One private Root scope, not a grant, Plan registry, allocator union or source.

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

fn sum(a: usize, b: usize) -> Result<usize, AudioRuntimeBuildError> {
    a.checked_add(b).ok_or(AudioRuntimeBuildError::ResourceFootprint(
        crate::AudioResourceFootprintError::ExtentOverflow,
    ))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FeeRole {
    Inactive,
    Prepared,
    Retired,
}

/// Non-Clone fee witness; it MUST be the last field of its actual owner struct.
/// Field drop glue destroys all Plan/scratch owners before this counter credit.
pub(super) struct LogicalPlanFee {
    scope: Arc<AtomicUsize>,
    old: usize,
    new: usize,
    role: FeeRole,
}

/// Prechecked signed delta, consumed only after a successful Source CAS.
pub(super) enum LogicalFeeRetirement {
    Add(usize),
    Subtract(usize),
    Unchanged,
}

impl LogicalPlanFee {
    fn inactive(scope: Arc<AtomicUsize>, old: usize, new: usize) -> Self {
        Self { scope, old, new, role: FeeRole::Inactive }
    }

    /// The owner has already been assembled and exclusively owns this receipt.
    /// Its Root prechecked C+H+N; concurrent external Drop can only lower H.
    pub(super) fn enroll(&mut self) -> Result<(), AudioRuntimeBuildError> {
        if self.role != FeeRole::Inactive {
            return Err(AudioRuntimeBuildError::StaleResourceAdmission);
        }
        self.scope.fetch_add(self.new, Ordering::SeqCst);
        self.role = FeeRole::Prepared;
        Ok(())
    }

    /// Refine an enrolled prospective upper bound after the actual immutable
    /// owner has been retained and its policy footprint checked. This changes
    /// no owner, scope, old-role amount or grant and grants no installation.
    /// The exclusive Prepared receipt still contributes all of `new` to H;
    /// peer Drop can only remove its own separate contribution.
    pub(super) fn shrink_prepared_to(
        &mut self,
        actual: usize,
    ) -> Result<(), AudioRuntimeBuildError> {
        if self.role != FeeRole::Prepared {
            return Err(AudioRuntimeBuildError::StaleResourceAdmission);
        }
        if actual > self.new {
            return Err(AudioRuntimeBuildError::InvalidResourceReplacement);
        }
        let released = self.new - actual;
        if released != 0 {
            self.scope.fetch_sub(released, Ordering::SeqCst);
        }
        self.new = actual;
        Ok(())
    }

    fn belongs_to(&self, scope: &Arc<AtomicUsize>) -> bool {
        Arc::ptr_eq(&self.scope, scope)
    }

    /// All checks precede Source guards; this receipt cannot concurrently Drop.
    fn checked_retirement(
        &self,
        scope: &Arc<AtomicUsize>,
        current: usize,
        next: usize,
        grant: usize,
    ) -> Result<LogicalFeeRetirement, AudioRuntimeBuildError> {
        if self.role != FeeRole::Prepared || !self.belongs_to(scope) {
            return Err(AudioRuntimeBuildError::StaleResourceAdmission);
        }
        let expected_next = current
            .checked_sub(self.old)
            .and_then(|value| value.checked_add(self.new))
            .ok_or(AudioRuntimeBuildError::InvalidResourceReplacement)?;
        if expected_next != next {
            return Err(AudioRuntimeBuildError::InvalidResourceReplacement);
        }
        let held = self.scope.load(Ordering::SeqCst);
        let next_held = held
            .checked_sub(self.new)
            .and_then(|value| value.checked_add(self.old))
            .ok_or(AudioRuntimeBuildError::InvalidResourceReplacement)?;
        let combined = sum(current, held)?;
        if sum(next, next_held)? != combined {
            return Err(AudioRuntimeBuildError::InvalidResourceReplacement);
        }
        if combined > grant {
            return Err(AudioRuntimeBuildError::ResourceOverlapGrantExceeded {
                category: AudioRuntimeResourceCategory::PreparedLogicalBytes,
                required: combined,
                granted: grant,
            });
        }
        Ok(if self.old > self.new {
            LogicalFeeRetirement::Add(self.old - self.new)
        } else if self.new > self.old {
            LogicalFeeRetirement::Subtract(self.new - self.old)
        } else {
            LogicalFeeRetirement::Unchanged
        })
    }

    /// Infallible prevalidated role move: one RMW, no retry/SDK/allocation/drop.
    /// For Add, only other credits can lower the checked H upper bound. For
    /// Subtract, this exclusively borrowed fee still contributes its entire N.
    pub(super) fn retire(&mut self, transition: LogicalFeeRetirement) {
        match transition {
            LogicalFeeRetirement::Add(bytes) => {
                self.scope.fetch_add(bytes, Ordering::SeqCst);
            }
            LogicalFeeRetirement::Subtract(bytes) => {
                self.scope.fetch_sub(bytes, Ordering::SeqCst);
            }
            LogicalFeeRetirement::Unchanged => {}
        }
        self.role = FeeRole::Retired;
    }
}

impl Drop for LogicalPlanFee {
    fn drop(&mut self) {
        let bytes = match self.role {
            FeeRole::Inactive => return,
            FeeRole::Prepared => self.new,
            FeeRole::Retired => self.old,
        };
        // This unique active receipt still contributes bytes to its own scope.
        // No early take/custom outer Drop is allowed to precede owner retirement.
        self.scope.fetch_sub(bytes, Ordering::SeqCst);
    }
}

impl AudioProgramRuntime {
    pub(super) fn held_native_plan_logical_bytes(&self) -> usize {
        self.native_held_plan_fees
            .as_ref()
            .map_or(0, |fees| fees.load(Ordering::SeqCst))
    }

    pub(super) fn include_held_native_plan_logical_bytes(
        &self,
        required: usize,
    ) -> Result<usize, AudioRuntimeBuildError> {
        sum(required, self.held_native_plan_logical_bytes())
    }

    pub(super) fn prepare_native_plan_logical_fee(
        &mut self,
        old: usize,
        new: usize,
        next: AudioRuntimeResourceFootprint,
    ) -> Result<LogicalPlanFee, AudioRuntimeBuildError> {
        let current = self.resource_footprint.prepared_logical_bytes;
        if current.checked_sub(old).and_then(|value| value.checked_add(new))
            != Some(next.prepared_logical_bytes)
        {
            return Err(AudioRuntimeBuildError::InvalidResourceReplacement);
        }
        self.check_resource_ceiling(
            AudioRuntimeResourceCategory::PreparedLogicalBytes,
            sum(current, new)?,
        )?;
        // A failed preflight never allocates this lazy scope. A receipt is built
        // inactive, then attached to an assembled real owner before enrollment.
        let scope = Arc::clone(
            self.native_held_plan_fees.get_or_insert_with(|| Arc::new(AtomicUsize::new(0))),
        );
        Ok(LogicalPlanFee::inactive(scope, old, new))
    }

    pub(super) fn validate_native_plan_logical_fee(
        &self,
        fee: &LogicalPlanFee,
    ) -> Result<(), AudioRuntimeBuildError> {
        let scope = self
            .native_held_plan_fees
            .as_ref()
            .ok_or(AudioRuntimeBuildError::StaleResourceAdmission)?;
        if fee.role != FeeRole::Prepared || !fee.belongs_to(scope) {
            return Err(AudioRuntimeBuildError::StaleResourceAdmission);
        }
        self.check_resource_ceiling(
            AudioRuntimeResourceCategory::PreparedLogicalBytes,
            self.resource_footprint.prepared_logical_bytes,
        )?;
        Ok(())
    }

    pub(super) fn prepare_native_plan_fee_retirement(
        &self,
        fee: &LogicalPlanFee,
        next: AudioRuntimeResourceFootprint,
    ) -> Result<LogicalFeeRetirement, AudioRuntimeBuildError> {
        let scope = self
            .native_held_plan_fees
            .as_ref()
            .ok_or(AudioRuntimeBuildError::StaleResourceAdmission)?;
        fee.checked_retirement(
            scope,
            self.resource_footprint.prepared_logical_bytes,
            next.prepared_logical_bytes,
            self.resource_grant.max_prepared_logical_bytes,
        )
    }
}

#[cfg(test)]
#[path = "native_author_logical_fees_tests.rs"]
mod tests;
