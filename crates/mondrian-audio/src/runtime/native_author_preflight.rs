//! Borrowed author logical-policy preflight before any Preserved local clone.
//!
//! This is a read-only check against the existing frozen Root policy. It is not
//! a reservation, a physical allocator bound, or an external-handle inventory.

use super::native_author::{NativeAudioAuthorProgram, NativeAudioParameterHandoffError};
use super::*;

#[derive(Default)]
struct RequestedAuthorPlans {
    inserted_logical_bytes: usize,
    affected_plans: usize,
}

impl RequestedAuthorPlans {
    fn checked_add(&mut self, other: Self) -> Result<(), AudioRuntimeBuildError> {
        let overflow = || {
            AudioRuntimeBuildError::ResourceFootprint(
                crate::AudioResourceFootprintError::ExtentOverflow,
            )
        };
        self.inserted_logical_bytes = self
            .inserted_logical_bytes
            .checked_add(other.inserted_logical_bytes)
            .ok_or_else(overflow)?;
        self.affected_plans =
            self.affected_plans.checked_add(other.affected_plans).ok_or_else(overflow)?;
        Ok(())
    }
}

impl AudioProgramRuntime {
    /// Check the complete retained tree before its first local deep clone.
    /// External prepared/retired handles remain an explicit later frontier.
    pub(super) fn preflight_native_author_logical_overlap(
        &self,
        programs: &[NativeAudioAuthorProgram],
        proposal: &mondrian_core::NativeAudioAuthorProposal,
    ) -> Result<(), NativeAudioParameterHandoffError> {
        let requested = self.requested_native_author_plans(programs, proposal)?;
        if requested.affected_plans == 0 {
            return Err(NativeAudioParameterHandoffError::ProgramMismatch);
        }
        // Current Root footprint already includes every nested occurrence.
        // Add it exactly once; recursion below sums only new local weights.
        let required = self
            .resource_footprint
            .prepared_logical_bytes
            .checked_add(requested.inserted_logical_bytes)
            .ok_or(AudioRuntimeBuildError::ResourceFootprint(
                crate::AudioResourceFootprintError::ExtentOverflow,
            ))?;
        self.check_resource_ceiling(AudioRuntimeResourceCategory::PreparedLogicalBytes, required)?;
        Ok(())
    }

    fn requested_native_author_plans(
        &self,
        programs: &[NativeAudioAuthorProgram],
        proposal: &mondrian_core::NativeAudioAuthorProposal,
    ) -> Result<RequestedAuthorPlans, NativeAudioParameterHandoffError> {
        let current = self.session.resource_plan();
        let mut requested = RequestedAuthorPlans::default();
        if self.sequence_id == proposal.sequence() {
            let next = programs
                .iter()
                .find(|program| {
                    program.sequence == proposal.sequence()
                        && program.program.output_id() == current.program().output_id()
                })
                .ok_or(NativeAudioParameterHandoffError::ProgramMismatch)?;
            // Exactly preserve prepare_parameter_locals' selected-force rule.
            if current.program() != next.program.as_ref()
                || self.session.native_owner_identity().session_id
                    == proposal.target().local_session
            {
                requested.inserted_logical_bytes = current.requested_native_author_logical_bytes(
                    next.program.as_ref(),
                    super::native_author::locator(proposal),
                )?;
                requested.affected_plans = 1;
            }
        }
        for source in self.sources.entries.values() {
            if let RuntimeSource::Nested(nested) = source {
                requested.checked_add(
                    nested.runtime.requested_native_author_plans(programs, proposal)?,
                )?;
            }
        }
        Ok(requested)
    }
}
