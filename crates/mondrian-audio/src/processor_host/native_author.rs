//! Parameter-plan bindings on actual retained Hosted records, never public minting.

use super::note_ledger::LedgerOrigin;
use super::*;
use crate::native_runtime_owner::NativeHostedBinding;
use crate::{AudioNativeOwnerError, AudioSessionCursorIdentity, NativeAudioAuthorAdmissionError};
use mondrian_core::{ExecutionCancellationToken, NativeAudioAuthorProposal};
use std::num::NonZeroU64;

/// Retained by the actual Hosted owner, independent of public completion facts.
/// A successfully installed Plan can await retry of its SAME native Ack without
/// replaying the author transaction, Plan allocation or Source publication.
pub(super) struct NativeHostedAuthorDelivery {
    pub(super) proposal: NativeAudioAuthorProposal,
    pub(super) request_cohort: Option<crate::NativeRequestCohort>,
    installed: Option<mondrian_core::NativeAudioEditorTarget>,
    reentered: Option<crate::NativeAudioAuthorStateEntryReceipt>,
}

impl NativeHostedAuthorDelivery {
    pub(super) fn new(
        proposal: NativeAudioAuthorProposal,
        request_cohort: Option<crate::NativeRequestCohort>,
    ) -> Self {
        Self {
            proposal,
            request_cohort,
            installed: None,
            reentered: None,
        }
    }
    pub(super) fn install_state_entry(
        &mut self,
        target: mondrian_core::NativeAudioEditorTarget,
        receipt: Option<crate::NativeAudioAuthorStateEntryReceipt>,
    ) {
        self.installed = Some(target);
        self.reentered = receipt;
    }
    pub(super) fn same_retained_proposal(&self, proposal: &NativeAudioAuthorProposal) -> bool {
        self.proposal.same_retained_proposal(proposal)
    }
    pub(super) fn matches_source_and_current(
        &self,
        binding: crate::NativeAudioObjectBindRequest,
        revision: u64,
        request_cohort: Option<crate::NativeRequestCohort>,
        current: mondrian_core::NativeAudioEditorTarget,
        object: crate::NativeAudioObjectObservation,
    ) -> bool {
        self.request_cohort == request_cohort
            && self.proposal.revision() == revision
            && binding.binding == self.proposal.target().binding
            && binding.expected_object == object
            && self.installed.map_or(self.proposal.target() == current, |installed| {
                installed == current
            })
    }
}

#[derive(Clone, Copy)]
pub(crate) struct PreparedParameterNativeBinding {
    index: usize,
    prior_lease: crate::AudioNativeProcessorOwnerLease,
    object: crate::NativeAudioObjectObservation,
    address: crate::AudioNativeProcessorAddress,
    receipt: crate::NativeAudioObjectBindReceipt,
}

impl PreparedParameterNativeBinding {
    pub(crate) fn editor_target(
        self,
        serial: NonZeroU64,
    ) -> mondrian_core::NativeAudioEditorTarget {
        crate::AudioNativeProcessorOwnerLease::issue(self.address, self.receipt.binding(), serial)
            .editor_target()
    }
}

/// Same-object acknowledgements prepared before the final author/Source guards.
/// Record capacity is charged independently from the new Plan/event storage.
pub(crate) struct PreparedParameterNativeBindings {
    current: AudioSessionCursorIdentity,
    records: Vec<PreparedParameterNativeBinding>,
}

impl PreparedParameterNativeBindings {
    pub(crate) fn resource_owner(&self) -> &Vec<PreparedParameterNativeBinding> {
        &self.records
    }
    pub(crate) fn selected_record(
        &self,
        proposal: &NativeAudioAuthorProposal,
    ) -> Option<PreparedParameterNativeBinding> {
        self.records
            .iter()
            .copied()
            .find(|record| record.prior_lease.editor_target() == proposal.target())
    }
}

fn pending(
    lease: crate::AudioNativeProcessorOwnerLease,
    reason: crate::NativeAudioAuthorHoldReason,
) -> NativeAudioAuthorAdmissionError {
    NativeAudioAuthorAdmissionError::Pending { target: Box::new(lease.editor_target()), reason }
}

/// Read-only exact handoff facts, not an installed lease or issuer.
pub(crate) struct ParameterNativeBindingContext<'a> {
    pub(crate) current_root: AudioSessionCursorIdentity,
    pub(crate) current_local: AudioSessionCursorIdentity,
    pub(crate) next_root: AudioSessionCursorIdentity,
    pub(crate) next_local: AudioSessionCursorIdentity,
    pub(crate) installed_serial: NonZeroU64,
    pub(crate) proposal: &'a NativeAudioAuthorProposal,
    pub(crate) cancellation: &'a ExecutionCancellationToken,
}

impl PreparedProcessorHost {
    pub(crate) fn retains_native_author_proposal(
        &self,
        proposal: &NativeAudioAuthorProposal,
        root: AudioSessionCursorIdentity,
        local: AudioSessionCursorIdentity,
        serial: NonZeroU64,
    ) -> Result<(), AudioNativeOwnerError> {
        let mut count = 0;
        for instance in &self.instances {
            if let NativeHostedBinding::Installed { lease, .. } = instance.native_binding
                && lease.address().root == root
                && lease.address().local == local
                && lease.address().occurrence == instance.occurrence
                && lease.address().epoch == instance.continuity.native_epoch()?
                && lease.installed_root_serial() == serial
                && lease.editor_target() == proposal.target()
                && instance.native_author_outstanding.as_ref().is_some_and(|retained| {
                    retained.installed.is_none() && retained.same_retained_proposal(proposal)
                })
            {
                count += 1;
            }
        }
        if count == 1 {
            Ok(())
        } else {
            Err(AudioNativeOwnerError::OwnerUnavailable)
        }
    }

    pub(crate) fn allocate_parameter_native_bindings(
        &self,
        local: AudioSessionCursorIdentity,
    ) -> PreparedParameterNativeBindings {
        PreparedParameterNativeBindings {
            current: local,
            records: Vec::with_capacity(self.instances.len()),
        }
    }

    pub(crate) fn prepare_parameter_native_bindings(
        &mut self,
        prepared: &mut PreparedParameterNativeBindings,
        context: ParameterNativeBindingContext<'_>,
    ) -> Result<(), NativeAudioAuthorAdmissionError> {
        let ParameterNativeBindingContext {
            current_root,
            current_local,
            next_root,
            next_local,
            installed_serial,
            proposal,
            cancellation,
        } = context;
        use crate::NativeAudioAuthorHoldReason as Reason;
        if prepared.current != current_local
            || !prepared.records.is_empty()
            || prepared.records.capacity() < self.instances.len()
        {
            return Err(AudioNativeOwnerError::OwnerUnavailable.into());
        }
        let records = &mut prepared.records;
        for (index, instance) in self.instances.iter_mut().enumerate() {
            if cancellation.is_canceled() {
                return Err(NativeAudioAuthorAdmissionError::Cancelled);
            }
            let (lease, object) = match instance.native_binding {
                NativeHostedBinding::Installed { lease, object }
                    if lease.address().root == current_root
                        && lease.address().local == current_local
                        && lease.address().occurrence == instance.occurrence
                        && lease.address().epoch == instance.continuity.native_epoch()?
                        && lease.installed_root_serial() == installed_serial =>
                {
                    (lease, object)
                }
                NativeHostedBinding::Unbound => {
                    // A log-capable adapter without an actual native object is
                    // not an editor peer. Never invent a binding for it.
                    let observed =
                        isolate_adapter_call("unbound parameter peer observation", || {
                            instance.processor.poll_native_observations(
                                crate::NativeAudioObservationConsumption::Preserve,
                            )
                        })?;
                    if observed.object.is_none() && observed.author.is_none() {
                        continue;
                    }
                    return Err(AudioNativeOwnerError::OwnerUnavailable.into());
                }
                _ => return Err(AudioNativeOwnerError::OwnerUnavailable.into()),
            };
            let selected = lease.editor_target() == proposal.target();
            let retained = instance.native_author_outstanding.as_ref();
            if selected
                && !retained.is_some_and(|retained| retained.same_retained_proposal(proposal))
            {
                return Err(AudioNativeOwnerError::OwnerUnavailable.into());
            }
            if !selected && retained.is_some() {
                return Err(pending(lease, Reason::OutstandingDelivery));
            }
            let observation =
                isolate_adapter_call("parameter handoff same-object source admission", || {
                    instance.processor.preflight_native_observations(object)
                })?;
            if cancellation.is_canceled() {
                return Err(NativeAudioAuthorAdmissionError::Cancelled);
            }
            if observation.object != Some(object) {
                return Err(AudioNativeOwnerError::OwnerUnavailable.into());
            }
            if observation.editor_cleanup.is_some() || observation.pending_reprepare.is_some() {
                return Err(pending(lease, Reason::PendingCallbacks));
            }
            let author = observation
                .author
                .as_ref()
                .ok_or_else(|| pending(lease, Reason::UnsupportedSource))?;
            author
                .source
                .validate()
                .map_err(|reason| pending(lease, Reason::DeliveryRefused(reason)))?;
            if let Some(failure) = author.source.failure() {
                return Err(pending(lease, Reason::SourceFailure(failure)));
            }
            if author.source.publishing() {
                return Err(pending(lease, Reason::PendingCallbacks));
            }
            if author.source.parameter_flush_pending() {
                return Err(pending(lease, Reason::DspFeedbackPending));
            }
            // Begin/Group scopes may remain OPEN after a nonterminal accepted
            // batch. They are not a whole-gesture transport stop condition.
            // The selected exact retained prefix may be installed while newer
            // callbacks remain owned by the same source. Ack consumes only that
            // prefix; PCM still holds for the suffix until its separate handoff.
            // An unrelated peer has no such accepted prefix permission.
            if !selected && author.source.pending_events() != 0 {
                return Err(pending(lease, Reason::PendingCallbacks));
            }
            if !selected && author.source.pending_dsp_feedback_values() != 0 {
                return Err(pending(lease, Reason::DspFeedbackPending));
            }
            if proposal.terminal() && !selected && !author.source.boundaries_closed() {
                return Err(pending(lease, Reason::OpenBoundary));
            }
            match &author.delivery {
                crate::NativeAudioAuthorDeliveryState::AwaitingAcknowledgement {
                    binding,
                    revision,
                    request_cohort,
                } if selected
                    && *binding
                        == (crate::NativeAudioObjectBindRequest {
                            binding: lease.binding(),
                            expected_object: object,
                        })
                    && *revision == proposal.revision()
                    && retained
                        .is_some_and(|retained| retained.request_cohort == *request_cohort) => {}
                crate::NativeAudioAuthorDeliveryState::Preserved if !selected => {}
                _ => return Err(pending(lease, Reason::OutstandingDelivery)),
            }
            let request = crate::NativeAudioObjectBindRequest {
                binding: mondrian_core::AudioProcessorBindingId::new(),
                expected_object: object,
            };
            let next_address = crate::AudioNativeProcessorAddress {
                root: next_root,
                local: next_local,
                occurrence: instance.occurrence,
                epoch: lease.address().epoch,
            };
            if let Some(notes) = &instance.notes {
                notes
                    .ledger
                    .preflight_continuing_rebind(
                        LedgerOrigin {
                            address: lease.address(),
                            binding: lease.binding(),
                            object,
                        },
                        LedgerOrigin {
                            address: next_address,
                            binding: request.binding,
                            object,
                        },
                    )
                    .map_err(|_| AudioNativeOwnerError::OwnerUnavailable)?;
            }
            let receipt = match isolate_adapter_call("parameter handoff native Bind", || {
                instance.processor.bind_native_audio_object(request)
            })? {
                crate::NativeAudioObjectBindOutcome::Bound(receipt)
                    if receipt.binding() == request.binding && receipt.object() == object =>
                {
                    receipt
                }
                crate::NativeAudioObjectBindOutcome::Bound(_) => {
                    return Err(AudioNativeOwnerError::OwnerUnavailable.into())
                }
                crate::NativeAudioObjectBindOutcome::Refused(reason) => {
                    return Err(AudioNativeOwnerError::BindingRefused(reason).into())
                }
            };
            records.push(PreparedParameterNativeBinding {
                index,
                prior_lease: lease,
                object,
                address: next_address,
                receipt,
            });
        }
        Ok(())
    }

    pub(crate) fn validate_parameter_native_bindings(
        &self,
        prepared: &PreparedParameterNativeBindings,
        current: AudioSessionCursorIdentity,
    ) -> Result<(), AudioNativeOwnerError> {
        if prepared.current != current {
            return Err(AudioNativeOwnerError::OwnerUnavailable);
        }
        for record in &prepared.records {
            let instance = self
                .instances
                .get(record.index)
                .ok_or(AudioNativeOwnerError::OwnerUnavailable)?;
            if !matches!(instance.native_binding, NativeHostedBinding::Installed { lease, object }
                if lease == record.prior_lease && object == record.object)
                || instance.occurrence != record.address.occurrence
                || instance.continuity.native_epoch()? != record.address.epoch
            {
                return Err(AudioNativeOwnerError::OwnerUnavailable);
            }
            if let Some(notes) = &instance.notes {
                notes
                    .ledger
                    .preflight_continuing_rebind(
                        LedgerOrigin {
                            address: record.prior_lease.address(),
                            binding: record.prior_lease.binding(),
                            object: record.object,
                        },
                        LedgerOrigin {
                            address: record.address,
                            binding: record.receipt.binding(),
                            object: record.object,
                        },
                    )
                    .map_err(|_| AudioNativeOwnerError::OwnerUnavailable)?;
            }
        }
        Ok(())
    }

    pub(crate) fn publish_parameter_native_bindings(
        &mut self,
        prepared: &PreparedParameterNativeBindings,
        serial: NonZeroU64,
    ) {
        // The exclusive same Runtime has prevalidated every index/record before
        // the final Source guard. No foreign call, allocation or owner drop.
        for record in &prepared.records {
            let lease = crate::AudioNativeProcessorOwnerLease::issue(
                record.address,
                record.receipt.binding(),
                serial,
            );
            let instance = &mut self.instances[record.index];
            // Every same-object peer is prevalidated before the final Source
            // guard. Move only its origin with this installed lease; retain all
            // IDs, tuples, phases and capacity counters. Selected Ack remains
            // a separate retryable native operation and must not migrate twice.
            if let Some(notes) = &mut instance.notes {
                notes.ledger.commit_prevalidated_continuing_rebind(LedgerOrigin {
                    address: lease.address(),
                    binding: lease.binding(),
                    object: record.object,
                });
            }
            instance.native_binding =
                NativeHostedBinding::Installed { lease, object: record.object };
            if let Some(retained) = &mut instance.native_author_outstanding {
                // Only the actual selected record has this retained delivery.
                // Earlier tree validation refused all other outstanding work.
                retained.installed = Some(lease.editor_target());
            }
        }
    }

    pub(crate) fn acknowledge_parameter_native_author(
        &mut self,
        proposal: &NativeAudioAuthorProposal,
        serial: NonZeroU64,
    ) -> Result<mondrian_core::NativeAudioEditorTarget, NativeAudioAuthorAdmissionError> {
        for instance in &mut self.instances {
            if !instance
                .native_author_outstanding
                .as_ref()
                .is_some_and(|retained| retained.same_retained_proposal(proposal))
            {
                continue;
            }
            let (lease, object) = match instance.native_binding {
                NativeHostedBinding::Installed { lease, object }
                    if lease.installed_root_serial() == serial =>
                {
                    (lease, object)
                }
                _ => return Err(AudioNativeOwnerError::OwnerUnavailable.into()),
            };
            if instance
                .native_author_outstanding
                .as_ref()
                .and_then(|retained| retained.installed)
                != Some(lease.editor_target())
            {
                return Err(pending(
                    lease,
                    crate::NativeAudioAuthorHoldReason::OutstandingDelivery,
                ));
            }
            let request_cohort = instance
                .native_author_outstanding
                .as_ref()
                .and_then(|retained| retained.request_cohort);
            if let Some(preparation) = instance
                .native_author_outstanding
                .as_ref()
                .and_then(|retained| retained.reentered)
            {
                if request_cohort.is_some() {
                    return Err(pending(
                        lease,
                        crate::NativeAudioAuthorHoldReason::OutstandingDelivery,
                    ));
                }
                let ack = crate::NativeAudioReenteredAuthorAcknowledgement {
                    preparation,
                    installed_binding: crate::NativeAudioObjectBindRequest {
                        binding: lease.binding(),
                        expected_object: object,
                    },
                };
                let observed = isolate_adapter_call("state-entry retained prefix Ack", || {
                    instance.processor.poll_native_observations(
                        crate::NativeAudioObservationConsumption::AcknowledgeReenteredAuthor(ack),
                    )
                })?;
                if observed.object != Some(object)
                    || !matches!(observed.author.as_ref().map(|author|&author.delivery),
                    Some(crate::NativeAudioAuthorDeliveryState::ReenteredAcknowledged(actual)) if *actual==ack)
                {
                    return Err(pending(
                        lease,
                        crate::NativeAudioAuthorHoldReason::OutstandingDelivery,
                    ));
                }
                instance.native_author_outstanding = None;
                return Ok(lease.editor_target());
            }
            let ack = crate::NativeAudioAuthorAcknowledgement {
                source_binding: crate::NativeAudioObjectBindRequest {
                    binding: proposal.target().binding,
                    expected_object: object,
                },
                installed_binding: crate::NativeAudioObjectBindRequest {
                    binding: lease.binding(),
                    expected_object: object,
                },
                revision: proposal.revision(),
            };
            let observation = isolate_adapter_call("parameter handoff exact source Ack", || {
                instance.processor.poll_native_observations(
                    crate::NativeAudioObservationConsumption::AcknowledgeAuthor(ack),
                )
            })?;
            if observation.object != Some(object)
                || !matches!(observation.author.as_ref().map(|author| &author.delivery),
                    Some(crate::NativeAudioAuthorDeliveryState::Acknowledged { acknowledgement: actual, request_cohort: actual_cohort }) if *actual == ack && *actual_cohort == request_cohort)
            {
                return Err(pending(
                    lease,
                    crate::NativeAudioAuthorHoldReason::OutstandingDelivery,
                ));
            }
            // Receipt is discarded ONLY after the same native object confirms
            // old-source/new-installed bindings and exact retained revision.
            instance.native_author_outstanding = None;
            return Ok(lease.editor_target());
        }
        Err(AudioNativeOwnerError::OwnerUnavailable.into())
    }
}
