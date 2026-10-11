//! Exact old/fresh native records prepared on the actual retained Hosted owners.

use super::*;
use crate::native_runtime_owner::NativeHostedBinding;
use crate::{AudioNativeOwnerError, NativeAudioAuthorAdmissionError as Error};
use crate::{AudioProcessorFactoryStorageBorrow as Storage, AudioProcessorFactoryStorageError};
use mondrian_core::NativeAudioAuthorProposal;
use std::num::NonZeroU64;

pub(crate) struct StateEntryContext<'a> {
    pub(crate) old_root: crate::AudioSessionCursorIdentity,
    pub(crate) next_root: crate::AudioSessionCursorIdentity,
    pub(crate) next_local: crate::AudioSessionCursorIdentity,
    pub(crate) serial: NonZeroU64,
    pub(crate) proposal: &'a NativeAudioAuthorProposal,
    pub(crate) next_plan: &'a crate::PreparedAudioPlan,
    pub(crate) affected: bool,
    pub(crate) cancellation: &'a mondrian_core::ExecutionCancellationToken,
}

#[derive(Clone, Copy)]
struct StateEntryRecord {
    index: usize,
    lease: crate::AudioNativeProcessorOwnerLease,
    old_object: crate::NativeAudioObjectObservation,
    source: Option<crate::NativeAudioAuthorStateEntrySource>,
    preparation: Option<crate::NativeAudioAuthorStateEntryReceipt>,
    binding: Option<crate::NativeAudioObjectBindReceipt>,
    address: crate::AudioNativeProcessorAddress,
}

pub(crate) struct PreparedStateEntryRecords {
    current: crate::AudioSessionCursorIdentity,
    records: Vec<StateEntryRecord>,
}

impl PreparedStateEntryRecords {
    pub(crate) fn allocation(
        &self,
    ) -> Result<crate::NativeHostAllocationBorrow<'_>, crate::NativeHostLedgerError> {
        use crate::NativeHostAllocationOwner;
        self.records.host_allocation()
    }
    pub(crate) fn selected_target(
        &self,
        proposal: &NativeAudioAuthorProposal,
        serial: NonZeroU64,
    ) -> Option<mondrian_core::NativeAudioEditorTarget> {
        self.records
            .iter()
            .find(|record| record.lease.editor_target() == proposal.target())
            .and_then(|record| {
                record.binding.map(|binding| {
                    crate::AudioNativeProcessorOwnerLease::issue(
                        record.address,
                        binding.binding(),
                        serial,
                    )
                    .editor_target()
                })
            })
    }
}

fn pending(
    lease: crate::AudioNativeProcessorOwnerLease,
    reason: crate::NativeAudioAuthorHoldReason,
) -> Error {
    Error::Pending { target: Box::new(lease.editor_target()), reason }
}

fn canonical_peer(
    occurrence: crate::AudioProcessorOccurrence,
    proposal: &NativeAudioAuthorProposal,
) -> bool {
    use crate::{AudioProcessorInsertionPoint as I, AudioProcessorOccurrenceOwner as O};
    use mondrian_core::{NativeAudioEditorInsertion as J, NativeAudioEditorOccurrenceOwner as N};
    let target = proposal.target();
    occurrence.instance_id == target.instance
        && matches!(
            (occurrence.insertion, target.insertion),
            (I::Scope, J::Scope) | (I::PreFader, J::PreFader) | (I::PostFader, J::PostFader)
        )
        && match (occurrence.owner, target.owner) {
            // Actual edit IDs distinguish materializations; their common Scope
            // is the single canonical state owner selected by the author Session.
            (O::Contribution { scope_id: a, .. }, N::Contribution { scope_id: b, .. }) => a == b,
            (O::Track(a), N::Track(b)) => a == b,
            (O::Bus(a), N::Bus(b)) => a == b,
            (O::Output(a), N::Output(b)) => a == b,
            _ => false,
        }
}

impl PreparedProcessorHost {
    pub(crate) fn allocate_state_entry_records(
        &self,
        current: crate::AudioSessionCursorIdentity,
    ) -> PreparedStateEntryRecords {
        PreparedStateEntryRecords {
            current,
            records: Vec::with_capacity(self.instances.len()),
        }
    }
    pub(crate) fn state_entry_destination_empty(&self) -> bool {
        self.instances.is_empty()
    }
    pub(crate) fn validate_state_entry_transfer(
        &self,
        old: &PreparedAudioSchedule,
        next: &PreparedAudioSchedule,
    ) -> Result<(), AudioExecutionError> {
        if self.instances.len() != old.processors.len()
            || old.processors.len() != next.processors.len()
            || self.instances.iter().zip(old.processors.iter().zip(&next.processors)).any(
                |(instance, (old, next))| {
                    instance.occurrence != old.occurrence
                        || old.occurrence != next.occurrence
                        || old.factory.contract() != next.factory.contract()
                        || old.factory.auxiliary_inputs() != next.factory.auxiliary_inputs()
                        || old.parameter_ids != next.parameter_ids
                },
            )
        {
            return Err(AudioExecutionError::InvalidPreparedSchedule);
        }
        Ok(())
    }
    pub(crate) fn swap_state_entry_instances(&mut self, next: &mut Self) {
        std::mem::swap(&mut self.instances, &mut next.instances);
        for instance in &mut next.instances {
            instance.continuity = match instance.continuity {
                HostedProcessorContinuity::Stateless => HostedProcessorContinuity::Stateless,
                _ => HostedProcessorContinuity::Unentered,
            };
            instance.native_editor_attachment = None;
        }
    }
    pub(crate) fn visit_state_entry_storage<'a>(
        &'a self,
        visitor: &mut dyn FnMut(Storage<'a>) -> Result<(), AudioProcessorFactoryStorageError>,
    ) -> Result<(), AudioProcessorFactoryStorageError> {
        visitor(Storage::complete(&self.instances)?)?;
        let (events, lanes) = self.parameter_events.resource_buffers();
        visitor(Storage::complete(events)?)?;
        visitor(Storage::complete(lanes)?)?;
        visitor(Storage::complete(&self.auxiliary_pcm)?)?;
        visitor(Storage::complete(&self.sidechain_frame_db)?)?;
        visitor(Storage::complete(&self.sidechain_gains)?)
    }
    pub(crate) fn retains_parameter_author(&self, proposal: &NativeAudioAuthorProposal) -> bool {
        self.instances.iter().any(|instance| {
            instance
                .native_author_outstanding
                .as_ref()
                .is_some_and(|retained| retained.same_retained_proposal(proposal))
        })
    }

    // This whole-tree pass runs before ANY destructive preparation. Every
    // installed source, unrelated peer and pending lifecycle is admitted once.
    pub(crate) fn inspect_state_entry_records(
        &mut self,
        prepared: &mut PreparedStateEntryRecords,
        context: &StateEntryContext<'_>,
    ) -> Result<(), Error> {
        use crate::NativeAudioAuthorHoldReason as Reason;
        if !prepared.records.is_empty() || prepared.records.capacity() < self.instances.len() {
            return Err(AudioNativeOwnerError::OwnerUnavailable.into());
        }
        for (index, instance) in self.instances.iter_mut().enumerate() {
            if context.cancellation.is_canceled() {
                return Err(Error::Cancelled);
            }
            let (lease, object) = match instance.native_binding {
                NativeHostedBinding::Installed { lease, object }
                    if lease.address().root == context.old_root
                        && lease.address().local == prepared.current
                        && lease.address().occurrence == instance.occurrence
                        && lease.address().epoch == instance.continuity.native_epoch()?
                        && lease.installed_root_serial() == context.serial =>
                {
                    (lease, object)
                }
                NativeHostedBinding::Unbound
                    if !instance.processor.native_observations_supported() =>
                {
                    continue
                }
                _ => return Err(AudioNativeOwnerError::OwnerUnavailable.into()),
            };
            let selected = lease.editor_target() == context.proposal.target();
            if selected
                != instance
                    .native_author_outstanding
                    .as_ref()
                    .is_some_and(|retained| retained.same_retained_proposal(context.proposal))
                || (!selected && instance.native_author_outstanding.is_some())
            {
                return Err(pending(lease, Reason::OutstandingDelivery));
            }
            let observed = isolate_adapter_call("whole-root author state source", || {
                instance.processor.preflight_native_observations(object)
            })?;
            if observed.object != Some(object) {
                return Err(AudioNativeOwnerError::OwnerUnavailable.into());
            }
            if observed.pending_reprepare.is_some() || observed.editor_cleanup.is_some() {
                return Err(pending(lease, Reason::PendingCallbacks));
            }
            let author = observed
                .author
                .as_ref()
                .ok_or_else(|| pending(lease, Reason::UnsupportedSource))?;
            author
                .source
                .validate()
                .map_err(|reason| pending(lease, Reason::DeliveryRefused(reason)))?;
            if !author.source.closed_empty() {
                return Err(pending(lease, Reason::PendingCallbacks));
            }
            match &author.delivery {
                crate::NativeAudioAuthorDeliveryState::AwaitingAcknowledgement {
                    binding,
                    revision,
                } if selected
                    && binding.binding == lease.binding()
                    && binding.expected_object == object
                    && *revision == context.proposal.revision() => {}
                crate::NativeAudioAuthorDeliveryState::Preserved if !selected => {}
                _ => return Err(pending(lease, Reason::OutstandingDelivery)),
            }
            let affected =
                context.affected && canonical_peer(instance.occurrence, context.proposal);
            let source = if affected {
                let canonical =
                    crate::schedule::processor(context.next_plan.program(), instance.occurrence)
                        .map_err(|_| AudioNativeOwnerError::OwnerUnavailable)?;
                if !matches!(
                    &canonical.definition,
                    mondrian_timeline::audio::AudioProcessorDefinitionRef::Vst3 { .. }
                ) {
                    return Err(pending(lease, Reason::UnsupportedSource));
                }
                if canonical.opaque_state.as_deref()
                    != context.proposal.state().map(|bytes| bytes.as_ref())
                {
                    return Err(AudioNativeOwnerError::OwnerUnavailable.into());
                }
                Some(if selected {
                    crate::NativeAudioAuthorStateEntrySource::SelectedTerminal {
                        revision: NonZeroU64::new(context.proposal.revision())
                            .ok_or(AudioNativeOwnerError::OwnerUnavailable)?,
                    }
                } else {
                    crate::NativeAudioAuthorStateEntrySource::CanonicalPeer {
                        revision: author.source.revision(),
                    }
                })
            } else {
                None
            };
            prepared.records.push(StateEntryRecord {
                index,
                lease,
                old_object: object,
                source,
                preparation: None,
                binding: None,
                address: crate::AudioNativeProcessorAddress {
                    root: context.next_root,
                    local: context.next_local,
                    occurrence: instance.occurrence,
                    epoch: None,
                },
            });
        }
        Ok(())
    }

    pub(crate) fn prepare_state_entry_records(
        &mut self,
        prepared: &mut PreparedStateEntryRecords,
        context: &StateEntryContext<'_>,
    ) -> Result<(), Error> {
        use crate::NativeAudioAuthorHoldReason as Reason;
        for record in &mut prepared.records {
            if context.cancellation.is_canceled() {
                return Err(Error::Cancelled);
            }
            let instance = self
                .instances
                .get_mut(record.index)
                .ok_or(AudioNativeOwnerError::OwnerUnavailable)?;
            if !matches!(instance.native_binding,NativeHostedBinding::Installed{lease,object}
                if lease==record.lease && object==record.old_object)
            {
                return Err(AudioNativeOwnerError::OwnerUnavailable.into());
            }
            let prior = crate::NativeAudioObjectBindRequest {
                binding: record.lease.binding(),
                expected_object: record.old_object,
            };
            let object = if let Some(source) = record.source {
                let bytes =
                    context.proposal.state().ok_or(AudioNativeOwnerError::OwnerUnavailable)?;
                let outcome =
                    isolate_adapter_call("actual peer native state-entry preparation", || {
                        instance.processor.prepare_native_author_state_entry(
                            crate::NativeAudioAuthorStateEntryRequest {
                                binding: prior,
                                source,
                                format:
                                    crate::NativeConfigurationFormat::Vst3ComponentAndController,
                                configuration: bytes.as_ref(),
                            },
                        )
                    })?;
                let receipt = match outcome {
                    crate::NativeAudioAuthorStateEntryOutcome::Prepared(receipt)
                        if receipt.source_binding() == prior
                            && receipt.source() == source
                            && receipt.configuration_bytes() == bytes.len() =>
                    {
                        receipt
                    }
                    crate::NativeAudioAuthorStateEntryOutcome::Refused(_) => {
                        return Err(pending(record.lease, Reason::CoherentPeerStateRequired))
                    }
                    _ => return Err(AudioNativeOwnerError::OwnerUnavailable.into()),
                };
                if record.preparation.is_some_and(|previous| previous != receipt) {
                    return Err(AudioNativeOwnerError::OwnerUnavailable.into());
                }
                record.preparation = Some(receipt);
                receipt.object()
            } else if let Some(binding) = record.binding {
                binding.object()
            } else {
                match isolate_adapter_call("whole-root native view cleanup", || {
                    instance.processor.close_native_audio_editor(prior)
                })? {
                    crate::NativeAudioEditorOutcome::Closed { .. } => {}
                    crate::NativeAudioEditorOutcome::ClosePending { .. } => {
                        return Err(pending(record.lease, Reason::PendingCallbacks))
                    }
                    _ => return Err(AudioNativeOwnerError::OwnerUnavailable.into()),
                }
                let observation =
                    isolate_adapter_call("unaffected source after view cleanup", || {
                        instance.processor.preflight_native_observations(record.old_object)
                    })?;
                if observation.object != Some(record.old_object)
                    || observation.pending_reprepare.is_some()
                    || observation.editor_cleanup.is_some()
                    || !observation.author.as_ref().is_some_and(|author| {
                        author.source.closed_empty()
                            && matches!(
                                author.delivery,
                                crate::NativeAudioAuthorDeliveryState::Preserved
                            )
                    })
                {
                    return Err(pending(record.lease, Reason::PendingCallbacks));
                }
                record.old_object
            };
            if let Some(binding) = record.binding {
                let current = isolate_adapter_call("prepared peer source revalidation", || {
                    instance.processor.preflight_native_observations(object)
                })?;
                // Selected prepared-prefix status is intentionally AwaitingAcceptance;
                // it cannot be delivered under the fresh object until explicit Ack.
                if current.object != Some(object)
                    || binding.object() != object
                    || current.pending_reprepare.is_some()
                    || current.editor_cleanup.is_some()
                    || !current.author.as_ref().is_some_and(|author| author.source.closed_empty())
                {
                    return Err(pending(record.lease, Reason::PendingCallbacks));
                }
                continue;
            }
            let request = crate::NativeAudioObjectBindRequest {
                binding: mondrian_core::AudioProcessorBindingId::new(),
                expected_object: object,
            };
            let bound = isolate_adapter_call("state-entry destination native Bind", || {
                instance.processor.bind_native_audio_object(request)
            })?;
            match bound {
                crate::NativeAudioObjectBindOutcome::Bound(receipt)
                    if receipt.binding() == request.binding && receipt.object() == object =>
                {
                    record.binding = Some(receipt)
                }
                crate::NativeAudioObjectBindOutcome::Refused(reason) => {
                    return Err(AudioNativeOwnerError::BindingRefused(reason).into())
                }
                _ => return Err(AudioNativeOwnerError::OwnerUnavailable.into()),
            }
        }
        Ok(())
    }
    pub(crate) fn validate_state_entry_records(
        &self,
        prepared: &PreparedStateEntryRecords,
        current: crate::AudioSessionCursorIdentity,
    ) -> Result<(), AudioNativeOwnerError> {
        if prepared.current != current {
            return Err(AudioNativeOwnerError::OwnerUnavailable);
        }
        for record in &prepared.records {
            let instance = self
                .instances
                .get(record.index)
                .ok_or(AudioNativeOwnerError::OwnerUnavailable)?;
            if record.binding.is_none()
                || (record.source.is_some() && record.preparation.is_none())
                || instance.occurrence != record.address.occurrence
                || !matches!(instance.native_binding,NativeHostedBinding::Installed{lease,object} if lease==record.lease && object==record.old_object)
            {
                return Err(AudioNativeOwnerError::OwnerUnavailable);
            }
        }
        Ok(())
    }
    // This pass follows every peer's foreign prepare/Bind. It is deliberately
    // outside final guards; a callback from a later peer may invalidate an
    // earlier source. An unchanged prefix is retained for a precise retry.
    pub(crate) fn revalidate_state_entry_sources(
        &mut self,
        prepared: &PreparedStateEntryRecords,
        cancellation: &mondrian_core::ExecutionCancellationToken,
    ) -> Result<(), Error> {
        use crate::NativeAudioAuthorHoldReason as Reason;
        for record in &prepared.records {
            if cancellation.is_canceled() {
                return Err(Error::Cancelled);
            }
            let instance = self
                .instances
                .get_mut(record.index)
                .ok_or(AudioNativeOwnerError::OwnerUnavailable)?;
            let binding = record.binding.ok_or(AudioNativeOwnerError::OwnerUnavailable)?;
            let observed = isolate_adapter_call("all-peer prepared source revalidation", || {
                instance.processor.preflight_native_observations(binding.object())
            })?;
            if observed.object != Some(binding.object())
                || observed.pending_reprepare.is_some()
                || observed.editor_cleanup.is_some()
                || !observed.author.as_ref().is_some_and(|author| author.source.closed_empty())
            {
                return Err(pending(record.lease, Reason::PendingCallbacks));
            }
        }
        Ok(())
    }
    pub(crate) fn publish_state_entry_records(
        &mut self,
        prepared: &PreparedStateEntryRecords,
        serial: NonZeroU64,
    ) {
        for record in &prepared.records {
            // Presence and complete indices were admitted before final CAS.
            if let Some(binding) = record.binding {
                let lease = crate::AudioNativeProcessorOwnerLease::issue(
                    record.address,
                    binding.binding(),
                    serial,
                );
                let instance = &mut self.instances[record.index];
                instance.native_binding =
                    NativeHostedBinding::Installed { lease, object: binding.object() };
                if let Some(retained) = &mut instance.native_author_outstanding {
                    retained.install_state_entry(lease.editor_target(), record.preparation);
                }
            }
        }
    }
}
