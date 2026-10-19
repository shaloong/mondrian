//! Preallocated storage transfer for explicit whole-root native state entry.

use super::*;
use crate::{AudioProcessorFactoryStorageBorrow as Storage, AudioProcessorFactoryStorageError};

impl PreparedAudioSessionStorage {
    pub(crate) fn identity(&self) -> crate::AudioSessionCursorIdentity {
        self.session.native_owner_identity()
    }
    pub(crate) fn plan(&self) -> &PreparedAudioPlan {
        self.session.resource_plan()
    }
    pub(crate) fn visit_storage<'a>(
        &'a self,
        visitor: &mut dyn FnMut(Storage<'a>) -> Result<(), AudioProcessorFactoryStorageError>,
    ) -> Result<(), AudioProcessorFactoryStorageError> {
        self.session.visit_state_entry_storage(visitor)
    }
    pub(crate) fn is_empty_unentered(&self) -> bool {
        self.session.processor_host.state_entry_destination_empty()
            && self.session.pending_reprepare_ticket.is_none()
            && matches!(self.session.continuity, SessionContinuity::Unentered)
    }
    pub(crate) fn validate_transfer(
        &self,
        current: &AudioRenderSession,
    ) -> Result<(), AudioExecutionError> {
        if !self.session.processor_host.state_entry_destination_empty()
            || self.session.pending_reprepare_ticket.is_some()
            || !matches!(self.session.continuity, SessionContinuity::Unentered)
            || self.identity().session_id == current.native_owner_identity().session_id
            || self.plan().contract() != current.plan.contract()
        {
            return Err(AudioExecutionError::InvalidPreparedSchedule);
        }
        current
            .processor_host
            .validate_state_entry_transfer(&current.plan.schedule, &self.plan().schedule)
    }
    pub(crate) fn swap_retained(&mut self, current: &mut AudioRenderSession) {
        // All fallible checks and native acknowledgements preceded the final
        // guards. Whole Hosted records move; no plugin create/drop occurs here.
        current
            .processor_host
            .swap_state_entry_instances(&mut self.session.processor_host);
        std::mem::swap(current, &mut self.session);
    }
}

impl AudioRenderSession {
    pub(crate) fn validate_reprepare_capture(
        &mut self,
        selection: &crate::processor_host::native_reprepare::ReprepareSelection,
        cancellation: &mondrian_core::ExecutionCancellationToken,
    ) -> Result<(), crate::NativeAudioReprepareError> {
        self.processor_host
            .validate_reprepare_capture(&self.plan.schedule, selection, cancellation)
    }
    pub(crate) fn visit_state_entry_storage<'a>(
        &'a self,
        visitor: &mut dyn FnMut(Storage<'a>) -> Result<(), AudioProcessorFactoryStorageError>,
    ) -> Result<(), AudioProcessorFactoryStorageError> {
        visitor(Storage::complete(&self.node_buffers)?)?;
        for node in &self.node_buffers {
            for buffer in [
                &node.input,
                &node.pre_fader,
                &node.post_fader_pre_mute,
                &node.post_mute,
            ] {
                visitor(Storage::complete(buffer)?)?;
            }
        }
        visitor(Storage::complete(&self.scratch.source_frames)?)?;
        visitor(Storage::complete(&self.scratch.source_pcm)?)?;
        visitor(Storage::complete(&self.scratch.contribution_pcm)?)?;
        visitor(Storage::complete(&self.scratch.contribution_processed)?)?;
        visitor(Storage::complete(&self.scratch.contribution_sample_gains)?)?;
        visitor(Storage::complete(&self.scratch.strip_post_gains)?)?;
        visitor(Storage::complete(&self.scratch.route_gains)?)?;
        visitor(Storage::complete(&self.scratch.frame_gain_db)?)?;
        visitor(Storage::complete(&self.scratch.frame_pan)?)?;
        for lines in [
            &self.contribution_delay_lines,
            &self.route_delay_lines,
            &self.sidechain_delay_lines,
            &self.processor_delay_lines,
        ] {
            visitor(Storage::complete(lines)?)?;
            for line in lines {
                line.visit_host_storage(visitor)?;
            }
        }
        visitor(Storage::complete(&self.sidechain_source_buffers)?)?;
        for buffer in &self.sidechain_source_buffers {
            visitor(Storage::complete(buffer)?)?;
        }
        visitor(Storage::complete(&self.processor_output_buffers)?)?;
        for buffer in &self.processor_output_buffers {
            visitor(Storage::complete(buffer)?)?;
        }
        self.processor_host.visit_state_entry_storage(visitor)?;
        self.meter_bank.visit_host_storage(visitor)
    }
    pub(crate) fn allocate_state_entry_records(
        &self,
    ) -> crate::processor_host::state_entry::PreparedStateEntryRecords {
        self.processor_host.allocate_state_entry_records(self.native_owner_identity())
    }
    pub(crate) fn inspect_state_entry_records(
        &mut self,
        records: &mut crate::processor_host::state_entry::PreparedStateEntryRecords,
        context: &crate::processor_host::state_entry::StateEntryContext<'_>,
    ) -> Result<(), crate::NativeAudioAuthorAdmissionError> {
        self.processor_host.inspect_state_entry_records(records, context)
    }
    pub(crate) fn prepare_state_entry_records(
        &mut self,
        records: &mut crate::processor_host::state_entry::PreparedStateEntryRecords,
        context: &crate::processor_host::state_entry::StateEntryContext<'_>,
    ) -> Result<(), crate::NativeAudioAuthorAdmissionError> {
        self.processor_host.prepare_state_entry_records(records, context)
    }
    pub(crate) fn validate_state_entry_records(
        &self,
        records: &crate::processor_host::state_entry::PreparedStateEntryRecords,
    ) -> Result<(), crate::AudioNativeOwnerError> {
        self.processor_host
            .validate_state_entry_records(records, self.native_owner_identity())
    }
    pub(crate) fn revalidate_state_entry_sources(
        &mut self,
        records: &crate::processor_host::state_entry::PreparedStateEntryRecords,
        cancellation: &mondrian_core::ExecutionCancellationToken,
    ) -> Result<(), crate::NativeAudioAuthorAdmissionError> {
        self.processor_host.revalidate_state_entry_sources(records, cancellation)
    }
    pub(crate) fn publish_state_entry_records(
        &mut self,
        records: &crate::processor_host::state_entry::PreparedStateEntryRecords,
        serial: std::num::NonZeroU64,
    ) {
        self.processor_host.publish_state_entry_records(records, serial)
    }
    pub(crate) fn retains_parameter_author(
        &self,
        proposal: &mondrian_core::NativeAudioAuthorProposal,
    ) -> bool {
        self.processor_host.retains_parameter_author(proposal)
    }
}

#[cfg(test)]
mod output_storage_tests {
    use super::*;
    use crate::tests::processor_output_tests::{fixture, plan, OutputResolver};
    use crate::NativeHostAllocationOwner;

    #[test]
    fn processor_output_state_entry_visitor_retains_each_actual_new_and_old_pcm_owner() {
        let sequence = fixture(AudioChannelLayout::Mono);
        let plan = Arc::new(plan(&sequence, &OutputResolver::default()).unwrap());
        let current = AudioRenderSession::new(Arc::clone(&plan)).unwrap();
        let next = AudioRenderSession::allocate_storage(plan).unwrap();
        let mut expected = Vec::new();
        for session in [&current, &next.session] {
            expected.push(Storage::complete(&session.processor_output_buffers).unwrap());
            for pcm in &session.processor_output_buffers {
                expected.push(Storage::complete(pcm).unwrap());
            }
        }
        assert_eq!(
            expected.len(),
            8,
            "two independent outer owners and six actual output PCM owners"
        );
        let mut observed = Vec::new();
        current
            .visit_state_entry_storage(&mut |owner| {
                observed.push(owner);
                Ok(())
            })
            .unwrap();
        next.visit_storage(&mut |owner| {
            observed.push(owner);
            Ok(())
        })
        .unwrap();
        for wanted in &expected {
            assert_eq!(
                observed
                    .iter()
                    .filter(|owner| owner.address() == wanted.address()
                        && owner.layout() == wanted.layout()
                        && owner.complete_allocation().is_some())
                    .count(),
                1
            );
        }
        let mut ledger =
            crate::NativeHostAllocationLedger::new(expected.len(), usize::MAX).unwrap();
        for owner in observed
            .iter()
            .filter(|owner| expected.iter().any(|wanted| wanted.address() == owner.address()))
        {
            ledger.record(owner.complete_allocation().unwrap()).unwrap();
            ledger.record(owner.complete_allocation().unwrap()).unwrap();
        }
        let expected_bytes = expected.iter().map(|owner| owner.layout().size()).sum::<usize>();
        assert_eq!(
            ledger.finish().unwrap().allocation_bytes(),
            expected_bytes
                + crate::NativeHostAllocationLedger::storage_layout(expected.len())
                    .unwrap()
                    .bytes()
                    .unwrap()
        );
        // The expected owners above are the actual Vecs, not synthesized policy sizes.
        assert!(current.processor_output_buffers.host_allocation().unwrap().layout().size() > 0);
    }
}
