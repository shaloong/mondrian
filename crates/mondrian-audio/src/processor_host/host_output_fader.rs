//! Parent-only addresses for unchanged native objects. Never sends Bind or Ack.

use super::*;
use crate::native_runtime_owner::NativeHostedBinding;
use crate::{AudioNativeOwnerError, AudioSessionCursorIdentity, NativeAudioAuthorAdmissionError};
use mondrian_core::ExecutionCancellationToken;
use std::num::NonZeroU64;

struct Record {
    index: usize,
    prior: crate::AudioNativeProcessorOwnerLease,
    object: crate::NativeAudioObjectObservation,
    next: crate::AudioNativeProcessorAddress,
}

pub(crate) struct PreparedHostFaderBindings {
    records: Vec<Record>,
}

impl PreparedHostFaderBindings {
    pub(crate) fn len(&self) -> usize {
        self.records.len()
    }

    pub(crate) fn resource_owner(&self) -> &impl crate::NativeHostAllocationOwner {
        &self.records
    }
}

impl PreparedProcessorHost {
    pub(crate) fn prepare_host_fader_bindings(
        &mut self,
        current: AudioSessionCursorIdentity,
        next: AudioSessionCursorIdentity,
        serial: NonZeroU64,
        program: &crate::CompiledAudioProgram,
        schedule: &PreparedAudioSchedule,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<PreparedHostFaderBindings, NativeAudioAuthorAdmissionError> {
        let mut records = Vec::with_capacity(self.instances.len());
        for (index, instance) in self.instances.iter_mut().enumerate() {
            let requirement = schedule
                .processors
                .get(index)
                .ok_or(AudioNativeOwnerError::OwnerUnavailable)?
                .factory
                .native_child_settlement_requirement();
            let Some((prior, object)) = admit(
                instance,
                current,
                serial,
                program,
                requirement,
                cancellation,
            )?
            else {
                continue;
            };
            let mut address = prior.address();
            address.root = next;
            address.local = next;
            if let Some(notes) = &instance.notes {
                notes
                    .ledger
                    .preflight_continuing_rebind(
                        note_ledger::LedgerOrigin {
                            address: prior.address(),
                            binding: prior.binding(),
                            object,
                        },
                        note_ledger::LedgerOrigin { address, binding: prior.binding(), object },
                    )
                    .map_err(|_| AudioNativeOwnerError::OwnerUnavailable)?;
            }
            records.push(Record { index, prior, object, next: address });
        }
        Ok(PreparedHostFaderBindings { records })
    }

    pub(crate) fn validate_host_fader_bindings(
        &mut self,
        prepared: &PreparedHostFaderBindings,
        current: AudioSessionCursorIdentity,
        serial: NonZeroU64,
        program: &crate::CompiledAudioProgram,
        schedule: &PreparedAudioSchedule,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<(), NativeAudioAuthorAdmissionError> {
        let mut records = prepared.records.iter();
        for (index, instance) in self.instances.iter_mut().enumerate() {
            let requirement = schedule
                .processors
                .get(index)
                .ok_or(AudioNativeOwnerError::OwnerUnavailable)?
                .factory
                .native_child_settlement_requirement();
            let Some((lease, object)) = admit(
                instance,
                current,
                serial,
                program,
                requirement,
                cancellation,
            )?
            else {
                continue;
            };
            let record = records.next().ok_or(AudioNativeOwnerError::OwnerUnavailable)?;
            if record.index != index || record.prior != lease || record.object != object {
                return Err(AudioNativeOwnerError::OwnerUnavailable.into());
            }
        }
        if records.next().is_some() {
            return Err(AudioNativeOwnerError::OwnerUnavailable.into());
        }
        Ok(())
    }

    pub(crate) fn publish_host_fader_bindings(
        &mut self,
        prepared: &PreparedHostFaderBindings,
        serial: NonZeroU64,
    ) {
        for record in &prepared.records {
            let instance = &mut self.instances[record.index];
            let lease = crate::AudioNativeProcessorOwnerLease::issue(
                record.next,
                record.prior.binding(),
                serial,
            );
            if let Some(notes) = &mut instance.notes {
                notes.ledger.commit_prevalidated_continuing_rebind(note_ledger::LedgerOrigin {
                    address: record.next,
                    binding: record.prior.binding(),
                    object: record.object,
                });
            }
            instance.native_binding =
                NativeHostedBinding::Installed { lease, object: record.object };
        }
    }
}

fn admit(
    instance: &mut HostedProcessorInstance,
    current: AudioSessionCursorIdentity,
    serial: NonZeroU64,
    program: &crate::CompiledAudioProgram,
    requirement: mondrian_core::NativeChildSettlementRequirement,
    cancellation: &ExecutionCancellationToken,
) -> Result<
    Option<(
        crate::AudioNativeProcessorOwnerLease,
        crate::NativeAudioObjectObservation,
    )>,
    NativeAudioAuthorAdmissionError,
> {
    if cancellation.is_canceled() {
        return Err(NativeAudioAuthorAdmissionError::Cancelled);
    }
    if instance.native_editor_attachment.is_some()
        || instance.native_editor_detached.is_some()
        || instance.native_author_outstanding.is_some()
        || matches!(
            instance.continuity,
            HostedProcessorContinuity::Failed
                | HostedProcessorContinuity::Unentered
                | HostedProcessorContinuity::Pending { .. }
        )
    {
        return Err(AudioNativeOwnerError::ContinuityUnavailable.into());
    }
    let (lease, object) = match instance.native_binding {
        NativeHostedBinding::Installed { lease, object }
            if lease.address().root == current
                && lease.address().local == current
                && lease.address().occurrence == instance.occurrence
                && lease.address().epoch == instance.continuity.native_epoch()?
                && lease.installed_root_serial() == serial =>
        {
            (lease, object)
        }
        NativeHostedBinding::Unbound => {
            let definition = &crate::schedule::processor(program, instance.occurrence)
                .map_err(|_| AudioNativeOwnerError::OwnerUnavailable)?
                .definition;
            if requirement != mondrian_core::NativeChildSettlementRequirement::NotApplicable
                || matches!(
                    definition,
                    mondrian_timeline::AudioProcessorDefinitionRef::Clap { .. }
                        | mondrian_timeline::AudioProcessorDefinitionRef::Vst3 { .. }
                )
                || instance.processor.native_observations_supported()
                || isolate_adapter_call("Host fader unbound control preflight", || {
                    instance.processor.preflight_reprepare()
                })?
                .is_some()
            {
                return Err(AudioNativeOwnerError::OwnerUnavailable.into());
            }
            return Ok(None);
        }
        _ => return Err(AudioNativeOwnerError::OwnerUnavailable.into()),
    };
    let observation = isolate_adapter_call("Host fader same-object source admission", || {
        instance.processor.preflight_native_observations(object)
    })?;
    if cancellation.is_canceled() {
        return Err(NativeAudioAuthorAdmissionError::Cancelled);
    }
    if observation.object != Some(object) {
        return Err(AudioNativeOwnerError::OwnerUnavailable.into());
    }
    let pending = |reason| NativeAudioAuthorAdmissionError::Pending {
        target: Box::new(lease.editor_target()),
        reason,
    };
    if observation.editor_cleanup.is_some() || observation.pending_reprepare.is_some() {
        return Err(pending(
            crate::NativeAudioAuthorHoldReason::PendingCallbacks,
        ));
    }
    crate::native_audio_author_runtime::closed_source(observation.author.as_ref(), false)
        .map_err(pending)?;
    Ok(Some((lease, object)))
}
