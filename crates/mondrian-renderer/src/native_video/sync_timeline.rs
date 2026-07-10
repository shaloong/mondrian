//! Cross-API timeline-fence ownership protocol for native video textures.

/// Fence values reserved for one D3D11-copy -> DX12-render ownership cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct NativeVideoFrameSyncPlan {
    /// Previous renderer completion that D3D11 must wait for before overwriting.
    pub wait_before_copy: Option<u64>,
    /// Value D3D11 signals after copying the decoder surface.
    pub copy_ready: u64,
    /// Value DX12 signals after rendering and returning the texture to COMMON.
    pub renderer_complete: u64,
}

/// Current cross-API ownership phase of one shared texture entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NativeVideoSyncPhase {
    /// No frame is in flight; D3D11 may reserve the entry for a copy.
    Idle,
    /// Fence values are reserved, but no D3D11 signal has been published.
    CopyReserved,
    /// D3D11 has published the copied texture; DX12 has not acquired it yet.
    CopyPublished,
    /// DX12 has acquired the resource and may submit renderer work.
    RendererAcquired,
    /// A cross-API operation failed after reservation; the entry cannot be reused.
    Poisoned,
}

impl NativeVideoSyncPhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::CopyReserved => "copy_reserved",
            Self::CopyPublished => "copy_published",
            Self::RendererAcquired => "renderer_acquired",
            Self::Poisoned => "poisoned",
        }
    }
}

/// State machine error for the shared-texture ownership protocol.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub(super) enum NativeVideoSyncTimelineError {
    /// A transition was attempted from the wrong ownership phase.
    #[error("native video sync transition requires {expected}, got {actual}")]
    UnexpectedPhase {
        /// Required phase.
        expected: &'static str,
        /// Actual phase.
        actual: &'static str,
    },
    /// A caller supplied a plan other than the entry's active reservation.
    #[error("native video sync plan does not match the active reservation")]
    PlanMismatch,
    /// The monotonic 64-bit fence value space is exhausted.
    #[error("native video sync fence values are exhausted")]
    FenceValueExhausted,
    /// The entry was poisoned by a failed cross-API operation.
    #[error("native video sync entry is poisoned")]
    Poisoned,
}

/// Strict timeline for a reusable D3D11/DX12 shared texture entry.
///
/// Every frame consumes two monotonically increasing values on one shared
/// fence. No successful transition can skip a phase, reuse another frame's
/// plan, or overwrite the entry before the previous renderer completion.
#[derive(Debug, Clone)]
pub(super) struct NativeVideoSyncTimeline {
    next_value: u64,
    last_renderer_complete: Option<u64>,
    active_plan: Option<NativeVideoFrameSyncPlan>,
    phase: NativeVideoSyncPhase,
}

impl Default for NativeVideoSyncTimeline {
    fn default() -> Self {
        Self {
            next_value: 1,
            last_renderer_complete: None,
            active_plan: None,
            phase: NativeVideoSyncPhase::Idle,
        }
    }
}

impl NativeVideoSyncTimeline {
    /// Reserve the next copy/render fence pair.
    pub fn begin_copy(&mut self) -> Result<NativeVideoFrameSyncPlan, NativeVideoSyncTimelineError> {
        self.require_phase(NativeVideoSyncPhase::Idle)?;
        let renderer_complete = self
            .next_value
            .checked_add(1)
            .ok_or(NativeVideoSyncTimelineError::FenceValueExhausted)?;
        let following_value = renderer_complete
            .checked_add(1)
            .ok_or(NativeVideoSyncTimelineError::FenceValueExhausted)?;
        let plan = NativeVideoFrameSyncPlan {
            wait_before_copy: self.last_renderer_complete,
            copy_ready: self.next_value,
            renderer_complete,
        };
        self.next_value = following_value;
        self.active_plan = Some(plan);
        self.phase = NativeVideoSyncPhase::CopyReserved;
        Ok(plan)
    }

    /// Commit the D3D11 copy and copy-ready signal.
    pub fn publish_copy(
        &mut self,
        plan: NativeVideoFrameSyncPlan,
    ) -> Result<(), NativeVideoSyncTimelineError> {
        self.require_phase(NativeVideoSyncPhase::CopyReserved)?;
        self.require_active_plan(plan)?;
        self.phase = NativeVideoSyncPhase::CopyPublished;
        Ok(())
    }

    /// Commit the DX12 wait and COMMON -> RESOURCE ownership transition.
    pub fn acquire_renderer(
        &mut self,
        plan: NativeVideoFrameSyncPlan,
    ) -> Result<(), NativeVideoSyncTimelineError> {
        self.require_phase(NativeVideoSyncPhase::CopyPublished)?;
        self.require_active_plan(plan)?;
        self.phase = NativeVideoSyncPhase::RendererAcquired;
        Ok(())
    }

    /// Commit renderer submission, RESOURCE -> COMMON, and completion signal.
    pub fn release_renderer(
        &mut self,
        plan: NativeVideoFrameSyncPlan,
    ) -> Result<(), NativeVideoSyncTimelineError> {
        self.require_phase(NativeVideoSyncPhase::RendererAcquired)?;
        self.require_active_plan(plan)?;
        self.last_renderer_complete = Some(plan.renderer_complete);
        self.active_plan = None;
        self.phase = NativeVideoSyncPhase::Idle;
        Ok(())
    }

    /// Validate that a prepared token still owns the renderer phase.
    pub fn validate_renderer_submission(
        &self,
        plan: NativeVideoFrameSyncPlan,
    ) -> Result<(), NativeVideoSyncTimelineError> {
        self.require_phase(NativeVideoSyncPhase::RendererAcquired)?;
        self.require_active_plan(plan)
    }

    /// Permanently reject reuse after a partially submitted cross-API operation.
    pub fn poison(&mut self) {
        self.phase = NativeVideoSyncPhase::Poisoned;
        self.active_plan = None;
    }

    /// Current protocol phase for diagnostics and pool admission.
    pub fn phase(&self) -> NativeVideoSyncPhase {
        self.phase
    }

    /// Fence completion that must be observed before command allocators reuse.
    pub fn reusable_after(&self) -> Option<u64> {
        self.last_renderer_complete
    }

    fn require_phase(
        &self,
        expected: NativeVideoSyncPhase,
    ) -> Result<(), NativeVideoSyncTimelineError> {
        if self.phase == NativeVideoSyncPhase::Poisoned {
            return Err(NativeVideoSyncTimelineError::Poisoned);
        }
        if self.phase != expected {
            return Err(NativeVideoSyncTimelineError::UnexpectedPhase {
                expected: expected.as_str(),
                actual: self.phase.as_str(),
            });
        }
        Ok(())
    }

    fn require_active_plan(
        &self,
        plan: NativeVideoFrameSyncPlan,
    ) -> Result<(), NativeVideoSyncTimelineError> {
        if self.active_plan != Some(plan) {
            return Err(NativeVideoSyncTimelineError::PlanMismatch);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeline_allocates_monotonic_copy_and_renderer_values() {
        let mut timeline = NativeVideoSyncTimeline::default();
        let first = timeline.begin_copy().expect("first frame can reserve");
        assert_eq!(
            first,
            NativeVideoFrameSyncPlan {
                wait_before_copy: None,
                copy_ready: 1,
                renderer_complete: 2,
            }
        );
        timeline.publish_copy(first).expect("copy can publish");
        timeline.acquire_renderer(first).expect("renderer can acquire");
        timeline.release_renderer(first).expect("renderer can release");

        let second = timeline.begin_copy().expect("second frame can reserve");
        assert_eq!(
            second,
            NativeVideoFrameSyncPlan {
                wait_before_copy: Some(2),
                copy_ready: 3,
                renderer_complete: 4,
            }
        );
    }

    #[test]
    fn timeline_rejects_skipped_phases_and_foreign_plans() {
        let mut timeline = NativeVideoSyncTimeline::default();
        let plan = timeline.begin_copy().expect("frame can reserve");
        assert_eq!(
            timeline.acquire_renderer(plan),
            Err(NativeVideoSyncTimelineError::UnexpectedPhase {
                expected: "copy_published",
                actual: "copy_reserved",
            })
        );
        let foreign = NativeVideoFrameSyncPlan { copy_ready: 99, ..plan };
        assert_eq!(
            timeline.publish_copy(foreign),
            Err(NativeVideoSyncTimelineError::PlanMismatch)
        );
    }

    #[test]
    fn poisoned_timeline_never_reenters_idle() {
        let mut timeline = NativeVideoSyncTimeline::default();
        let _ = timeline.begin_copy().expect("frame can reserve");
        timeline.poison();
        assert_eq!(timeline.phase(), NativeVideoSyncPhase::Poisoned);
        assert_eq!(
            timeline.begin_copy(),
            Err(NativeVideoSyncTimelineError::Poisoned)
        );
    }

    #[test]
    fn fence_value_exhaustion_does_not_mutate_idle_state() {
        let mut timeline = NativeVideoSyncTimeline {
            next_value: u64::MAX - 1,
            ..NativeVideoSyncTimeline::default()
        };
        assert_eq!(
            timeline.begin_copy(),
            Err(NativeVideoSyncTimelineError::FenceValueExhausted)
        );
        assert_eq!(timeline.phase(), NativeVideoSyncPhase::Idle);
        assert_eq!(timeline.active_plan, None);
    }
}
