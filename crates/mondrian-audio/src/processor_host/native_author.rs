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
    pub(crate) fn prior_root(self) -> AudioSessionCursorIdentity {
        self.prior_lease.address().root
    }
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

/// Only the authentic, coherent VST3 value-rescan prefix may continue while a
/// later request remains held. This reads the original Hosted receipt, never a
/// later live request as a substitute for that prefix's identity.
fn continuing_rescan_cohort(
    instance: &HostedProcessorInstance,
    lease: crate::AudioNativeProcessorOwnerLease,
    object: crate::NativeAudioObjectObservation,
    proposal: &NativeAudioAuthorProposal,
    program: &crate::CompiledAudioProgram,
) -> Result<Option<crate::NativeRequestCohort>, NativeAudioAuthorAdmissionError> {
    use crate::NativeAudioAuthorHoldReason as Reason;
    let retained = instance
        .native_author_outstanding
        .as_ref()
        .filter(|retained| retained.same_retained_proposal(proposal))
        .ok_or(AudioNativeOwnerError::OwnerUnavailable)?;
    let Some(cohort) = retained.request_cohort else {
        return Ok(None);
    };
    let address = lease.address();
    if cohort.serial == 0
        || cohort.exhausted()
        || cohort.request != (mondrian_core::NativeAudioReprepareRequest::Vst3Restart { flags: 4 })
        || retained.installed.is_some()
        || retained.reentered.is_some()
        || lease.editor_target() != proposal.target()
        || address.root != address.local
        || address.occurrence != instance.occurrence
        || !matches!(instance.continuity, HostedProcessorContinuity::Active { epoch, .. }
            if Some(epoch) == address.epoch)
        || proposal.completion()
            != mondrian_core::NativeAudioAuthorCompletion::CoherentConfiguration
        || proposal.state().is_none()
        || proposal.open_parameter_edits() != 0
        || proposal.group_depth() != 0
        || address.occurrence.insertion == crate::AudioProcessorInsertionPoint::Scope
        || !matches!(instance.native_binding, NativeHostedBinding::Installed { lease: actual, object: actual_object }
            if actual == lease && actual_object == object)
        || !crate::schedule::processor(program, address.occurrence).is_ok_and(|processor| {
            matches!(
                processor.definition,
                mondrian_timeline::AudioProcessorDefinitionRef::Vst3 { .. }
            )
        })
    {
        return Err(pending(lease, Reason::OutstandingDelivery));
    }
    Ok(Some(cohort))
}

fn admit_rescan_live_request(
    lease: crate::AudioNativeProcessorOwnerLease,
    original: crate::NativeRequestCohort,
    observed: Option<crate::NativeObservedRequest>,
) -> Result<(), NativeAudioAuthorAdmissionError> {
    // A missing live request may mean an external raw retirement. Only the
    // SDK's eventual exact Ack can settle A, never a missing observation here.
    if !matches!(observed, Some(crate::NativeObservedRequest::Cohort(live))
        if live.serial >= original.serial && !live.exhausted()
            && live.request == original.request)
    {
        return Err(pending(
            lease,
            crate::NativeAudioAuthorHoldReason::PendingCallbacks,
        ));
    }
    Ok(())
}

/// Read-only exact handoff facts, not an installed lease or issuer.
#[derive(Clone, Copy)]
pub(crate) struct ParameterNativeBindingContext<'a> {
    pub(crate) current_root: AudioSessionCursorIdentity,
    pub(crate) current_local: AudioSessionCursorIdentity,
    pub(crate) next_root: AudioSessionCursorIdentity,
    pub(crate) next_local: AudioSessionCursorIdentity,
    pub(crate) installed_serial: NonZeroU64,
    pub(crate) proposal: &'a NativeAudioAuthorProposal,
    pub(crate) cancellation: &'a ExecutionCancellationToken,
}

// Same source checks for the original singleton and the terminal member set.
// This borrows real Hosted owners and performs no Bind or publication.
fn admit_parameter_native_source(
    instance: &mut HostedProcessorInstance,
    context: &ParameterNativeBindingContext<'_>,
    proposals: &[NativeAudioAuthorProposal],
    program: &crate::CompiledAudioProgram,
    terminal_revision: bool,
) -> Result<
    Option<(
        crate::AudioNativeProcessorOwnerLease,
        crate::NativeAudioObjectObservation,
    )>,
    NativeAudioAuthorAdmissionError,
> {
    use crate::NativeAudioAuthorHoldReason as Reason;
    let current_root = context.current_root;
    let current_local = context.current_local;
    let installed_serial = context.installed_serial;
    let cancellation = context.cancellation;
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
            let observed = isolate_adapter_call("unbound parameter peer observation", || {
                instance
                    .processor
                    .poll_native_observations(crate::NativeAudioObservationConsumption::Preserve)
            })?;
            if observed.object.is_none() && observed.author.is_none() {
                return Ok(None);
            }
            return Err(AudioNativeOwnerError::OwnerUnavailable.into());
        }
        _ => return Err(AudioNativeOwnerError::OwnerUnavailable.into()),
    };
    let selected_proposal =
        proposals.iter().find(|proposal| proposal.target() == lease.editor_target());
    let selected = selected_proposal.is_some();
    let proposal = selected_proposal.unwrap_or(context.proposal);
    let retained = instance.native_author_outstanding.as_ref();
    if selected && !retained.is_some_and(|retained| retained.same_retained_proposal(proposal)) {
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
    if observation.editor_cleanup.is_some() {
        return Err(pending(lease, Reason::PendingCallbacks));
    }
    if observation.pending_reprepare.is_some() {
        let original = if selected {
            continuing_rescan_cohort(instance, lease, object, proposal, program)?
        } else {
            None
        }
        .ok_or_else(|| pending(lease, Reason::PendingCallbacks))?;
        admit_rescan_live_request(lease, original, observation.pending_reprepare)?;
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
    if terminal_revision && selected && author.source.revision() < proposal.revision() {
        return Err(pending(lease, Reason::OutstandingDelivery));
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
            && retained.is_some_and(|retained| retained.request_cohort == *request_cohort) => {}
        crate::NativeAudioAuthorDeliveryState::Preserved if !selected => {}
        _ => return Err(pending(lease, Reason::OutstandingDelivery)),
    }
    Ok(Some((lease, object)))
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

    pub(crate) fn continuing_rescan_cohort(
        &self,
        proposal: &NativeAudioAuthorProposal,
        root: AudioSessionCursorIdentity,
        local: AudioSessionCursorIdentity,
        serial: NonZeroU64,
        program: &crate::CompiledAudioProgram,
    ) -> Result<Option<crate::NativeRequestCohort>, NativeAudioAuthorAdmissionError> {
        self.retains_native_author_proposal(proposal, root, local, serial)?;
        let instance = self
            .instances
            .iter()
            .find(|instance| {
                instance
                    .native_author_outstanding
                    .as_ref()
                    .is_some_and(|retained| retained.same_retained_proposal(proposal))
            })
            .ok_or(AudioNativeOwnerError::OwnerUnavailable)?;
        let NativeHostedBinding::Installed { lease, object } = instance.native_binding else {
            return Err(AudioNativeOwnerError::OwnerUnavailable.into());
        };
        continuing_rescan_cohort(instance, lease, object, proposal, program)
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
        program: &crate::CompiledAudioProgram,
    ) -> Result<(), NativeAudioAuthorAdmissionError> {
        self.prepare_parameter_native_bindings_for_selection(
            prepared,
            context,
            std::slice::from_ref(context.proposal),
            program,
        )
    }

    pub(crate) fn preflight_terminal_author_sources(
        &mut self,
        context: ParameterNativeBindingContext<'_>,
        proposals: &[NativeAudioAuthorProposal],
        program: &crate::CompiledAudioProgram,
    ) -> Result<(), NativeAudioAuthorAdmissionError> {
        for proposal in proposals {
            self.retains_native_author_proposal(
                proposal,
                context.current_root,
                context.current_local,
                context.installed_serial,
            )?;
        }
        for instance in &mut self.instances {
            if context.cancellation.is_canceled() {
                return Err(NativeAudioAuthorAdmissionError::Cancelled);
            }
            admit_parameter_native_source(instance, &context, proposals, program, true)?;
        }
        Ok(())
    }

    pub(crate) fn prepare_terminal_author_bindings(
        &mut self,
        prepared: &mut PreparedParameterNativeBindings,
        context: ParameterNativeBindingContext<'_>,
        proposals: &[NativeAudioAuthorProposal],
        program: &crate::CompiledAudioProgram,
    ) -> Result<(), NativeAudioAuthorAdmissionError> {
        self.prepare_parameter_native_bindings_for_selection(prepared, context, proposals, program)
    }

    fn prepare_parameter_native_bindings_for_selection(
        &mut self,
        prepared: &mut PreparedParameterNativeBindings,
        context: ParameterNativeBindingContext<'_>,
        proposals: &[NativeAudioAuthorProposal],
        program: &crate::CompiledAudioProgram,
    ) -> Result<(), NativeAudioAuthorAdmissionError> {
        let ParameterNativeBindingContext {
            current_root: _,
            current_local,
            next_root,
            next_local,
            installed_serial: _,
            proposal: _,
            cancellation,
        } = context;
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
            let Some((lease, object)) =
                admit_parameter_native_source(instance, &context, proposals, program, false)?
            else {
                continue;
            };
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

    /// Final whole-source check after every native Bind and before Source CAS.
    pub(crate) fn validate_terminal_author_bindings(
        &mut self,
        prepared: &PreparedParameterNativeBindings,
        context: ParameterNativeBindingContext<'_>,
        proposals: &[NativeAudioAuthorProposal],
        program: &crate::CompiledAudioProgram,
    ) -> Result<(), NativeAudioAuthorAdmissionError> {
        if prepared.current != context.current_local {
            return Err(AudioNativeOwnerError::OwnerUnavailable.into());
        }
        let mut records = 0usize;
        for (index, instance) in self.instances.iter_mut().enumerate() {
            if context.cancellation.is_canceled() {
                return Err(NativeAudioAuthorAdmissionError::Cancelled);
            }
            let admitted =
                admit_parameter_native_source(instance, &context, proposals, program, true)?;
            let record = prepared.records.iter().find(|record| record.index == index);
            let Some((lease, object)) = admitted else {
                if record.is_some() {
                    return Err(AudioNativeOwnerError::OwnerUnavailable.into());
                }
                continue;
            };
            let record = record.ok_or(AudioNativeOwnerError::OwnerUnavailable)?;
            if record.prior_lease != lease
                || record.object != object
                || record.receipt.object() != object
                || record.address.root != context.next_root
                || record.address.local != context.next_local
                || record.address.occurrence != instance.occurrence
                || record.address.epoch != lease.address().epoch
            {
                return Err(AudioNativeOwnerError::OwnerUnavailable.into());
            }
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
                            address: record.address,
                            binding: record.receipt.binding(),
                            object,
                        },
                    )
                    .map_err(|_| AudioNativeOwnerError::OwnerUnavailable)?;
            }
            records = records.checked_add(1).ok_or(AudioNativeOwnerError::OwnerUnavailable)?;
        }
        if records != prepared.records.len() {
            return Err(AudioNativeOwnerError::OwnerUnavailable.into());
        }
        Ok(())
    }

    pub(crate) fn validate_parameter_native_bindings(
        &mut self,
        prepared: &PreparedParameterNativeBindings,
        current: AudioSessionCursorIdentity,
        proposal: &NativeAudioAuthorProposal,
        program: &crate::CompiledAudioProgram,
        ticket: Option<crate::AudioProcessorReprepareTicket>,
    ) -> Result<(), NativeAudioAuthorAdmissionError> {
        if prepared.current != current {
            return Err(AudioNativeOwnerError::OwnerUnavailable.into());
        }
        for record in &prepared.records {
            let instance = self
                .instances
                .get_mut(record.index)
                .ok_or(AudioNativeOwnerError::OwnerUnavailable)?;
            if !matches!(instance.native_binding, NativeHostedBinding::Installed { lease, object }
                if lease == record.prior_lease && object == record.object)
                || instance.occurrence != record.address.occurrence
                || instance.continuity.native_epoch()? != record.address.epoch
            {
                return Err(AudioNativeOwnerError::OwnerUnavailable.into());
            }
            if record.prior_lease.editor_target() == proposal.target()
                && (ticket.is_some()
                    || instance.native_author_outstanding.as_ref().is_some_and(|retained| {
                        retained.request_cohort.is_some_and(|cohort| {
                            cohort.request
                                == (mondrian_core::NativeAudioReprepareRequest::Vst3Restart {
                                    flags: 4,
                                })
                        })
                    }))
            {
                // The new permission is rechecked after Bind against the old
                // Hosted lease plus the prepared receipt. Old CLAP/no-pending
                // paths do not acquire this additional source query or gate.
                let observation =
                    isolate_adapter_call("parameter handoff final rescan admission", || {
                        instance.processor.preflight_native_observations(record.object)
                    })?;
                if record.receipt.object() != record.object
                    || record.address.root.session_id
                        != record.prior_lease.address().root.session_id
                    || record.address.local.session_id
                        != record.prior_lease.address().local.session_id
                    || record.address.epoch != record.prior_lease.address().epoch
                    || observation.object != Some(record.object)
                    || observation.editor_cleanup.is_some()
                {
                    return Err(AudioNativeOwnerError::OwnerUnavailable.into());
                }
                let author = observation.author.as_ref().ok_or_else(|| {
                    pending(
                        record.prior_lease,
                        crate::NativeAudioAuthorHoldReason::UnsupportedSource,
                    )
                })?;
                author.source.validate().map_err(|reason| {
                    pending(
                        record.prior_lease,
                        crate::NativeAudioAuthorHoldReason::DeliveryRefused(reason),
                    )
                })?;
                if let Some(failure) = author.source.failure() {
                    return Err(pending(
                        record.prior_lease,
                        crate::NativeAudioAuthorHoldReason::SourceFailure(failure),
                    ));
                }
                if author.source.publishing()
                    || author.source.parameter_flush_pending()
                    || author.source.revision() < proposal.revision()
                    || !matches!(author.delivery,
                        crate::NativeAudioAuthorDeliveryState::AwaitingAcknowledgement{binding,revision,request_cohort}
                        if binding==(crate::NativeAudioObjectBindRequest{
                            binding:record.prior_lease.binding(),expected_object:record.object,
                        }) && revision==proposal.revision() && instance.native_author_outstanding.as_ref()
                            .is_some_and(|retained|retained.request_cohort==request_cohort))
                {
                    return Err(pending(
                        record.prior_lease,
                        crate::NativeAudioAuthorHoldReason::OutstandingDelivery,
                    ));
                }
                if observation.pending_reprepare.is_some() || ticket.is_some() {
                    let original = continuing_rescan_cohort(
                        instance,
                        record.prior_lease,
                        record.object,
                        proposal,
                        program,
                    )?
                    .ok_or_else(|| {
                        pending(
                            record.prior_lease,
                            crate::NativeAudioAuthorHoldReason::PendingCallbacks,
                        )
                    })?;
                    admit_rescan_live_request(
                        record.prior_lease,
                        original,
                        observation.pending_reprepare,
                    )?;
                }
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
                // The singleton or terminal batch admitted this exact retained
                // member. Earlier validation refused all unselected outstanding work.
                retained.installed = Some(lease.editor_target());
            }
        }
    }

    pub(crate) fn parameter_author_request_cohort(
        &self,
        proposal: &NativeAudioAuthorProposal,
        serial: NonZeroU64,
    ) -> Result<Option<crate::NativeRequestCohort>, NativeAudioAuthorAdmissionError> {
        let instance = self
            .instances
            .iter()
            .find(|instance| {
                instance
                    .native_author_outstanding
                    .as_ref()
                    .is_some_and(|retained| retained.same_retained_proposal(proposal))
            })
            .ok_or(AudioNativeOwnerError::OwnerUnavailable)?;
        let NativeHostedBinding::Installed { lease, .. } = instance.native_binding else {
            return Err(AudioNativeOwnerError::OwnerUnavailable.into());
        };
        let retained = instance
            .native_author_outstanding
            .as_ref()
            .ok_or(AudioNativeOwnerError::OwnerUnavailable)?;
        if lease.installed_root_serial() != serial {
            return Err(AudioNativeOwnerError::OwnerUnavailable.into());
        }
        if retained.installed != Some(lease.editor_target()) {
            return Err(pending(
                lease,
                crate::NativeAudioAuthorHoldReason::OutstandingDelivery,
            ));
        }
        Ok(retained.request_cohort)
    }

    pub(crate) fn acknowledge_parameter_native_author(
        &mut self,
        proposal: &NativeAudioAuthorProposal,
        serial: NonZeroU64,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<
        (
            mondrian_core::NativeAudioEditorTarget,
            crate::NativeAudioObservations,
        ),
        NativeAudioAuthorAdmissionError,
    > {
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
                if cancellation.is_canceled() {
                    return Err(NativeAudioAuthorAdmissionError::Cancelled);
                }
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
                return Ok((lease.editor_target(), observed));
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
            if cancellation.is_canceled() {
                return Err(NativeAudioAuthorAdmissionError::Cancelled);
            }
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
            return Ok((lease.editor_target(), observation));
        }
        Err(AudioNativeOwnerError::OwnerUnavailable.into())
    }
}

#[cfg(test)]
mod terminal_source_revision_tests {
    use super::*;
    use std::num::NonZeroU32;
    use std::sync::Mutex;

    struct Snapshot(Arc<Mutex<crate::NativeAudioObservations>>);
    impl AudioProcessor for Snapshot {
        fn poll_native_observations(
            &mut self,
            _: crate::NativeAudioObservationConsumption,
        ) -> Result<crate::NativeAudioObservations, AudioProcessorHostError> {
            Ok(self.0.lock().unwrap().clone())
        }
        fn enter_state(&mut self, _: i64) -> Result<(), AudioProcessorHostError> {
            unreachable!()
        }
        fn process(
            &mut self,
            _: crate::AudioProcessorProcessContext,
            _: &mut dyn AudioProcessorAudioIo,
            _: crate::AudioParameterEventBatch<'_>,
        ) -> Result<(), AudioProcessorHostError> {
            unreachable!()
        }
    }

    #[test]
    fn terminal_final_source_rejects_revision_below_its_exact_retained_delivery() {
        // Portable same-object ABI model, not native DLL or author permission.
        let root = crate::AudioSessionCursorIdentity {
            prepared_plan_id: mondrian_core::AudioPreparedPlanId::new(),
            session_id: mondrian_core::AudioRenderSessionId::new(),
        };
        let next = crate::AudioSessionCursorIdentity {
            prepared_plan_id: mondrian_core::AudioPreparedPlanId::new(),
            ..root
        };
        let occurrence = crate::AudioProcessorOccurrence {
            instance_id: mondrian_core::AudioProcessorInstanceId::new(),
            owner: crate::AudioProcessorOccurrenceOwner::Output(
                mondrian_core::ProgramOutputId::new(),
            ),
            insertion: crate::AudioProcessorInsertionPoint::PreFader,
        };
        let epoch = crate::AudioContinuityEpoch::new(7);
        let address = crate::AudioNativeProcessorAddress {
            root,
            local: root,
            occurrence,
            epoch: Some(epoch),
        };
        let serial = NonZeroU64::new(9).unwrap();
        let binding = mondrian_core::AudioProcessorBindingId::new();
        let lease = crate::AudioNativeProcessorOwnerLease::issue(address, binding, serial);
        let object = crate::NativeAudioObjectObservation::new(
            NonZeroU32::new(123).unwrap(),
            NonZeroU64::new(1).unwrap(),
        );
        let request = crate::NativeAudioObjectBindRequest { binding, expected_object: object };
        let sequence = mondrian_core::SequenceId::new();
        let proposal = NativeAudioAuthorProposal::project(
            lease.editor_target(),
            sequence,
            5,
            mondrian_core::NativeAudioAuthorBoundaryFacts {
                completion: mondrian_core::NativeAudioAuthorCompletion::CoherentConfiguration,
                open_parameter_edits: 0,
                group_depth: 0,
            },
            vec![mondrian_core::NativeAudioAuthorCallback::DirtyChanged(
                false,
            )],
            Some(vec![1]),
        )
        .unwrap();
        let source = crate::NativeAudioAuthorSourceStatus {
            revision: 4,
            pending_events: 0,
            pending_parameter_observations: 0,
            pending_dsp_feedback_values: 0,
            open_parameter_edits: 0,
            group_depth: 0,
            failure: None,
            parameter_flush_pending: false,
            publishing: false,
        };
        source.validate().unwrap(); // Legal source shape alone misses the relation.
        let observation = Arc::new(Mutex::new(crate::NativeAudioObservations {
            object: Some(object),
            author: Some(crate::NativeAudioAuthorObservation {
                source,
                delivery: crate::NativeAudioAuthorDeliveryState::AwaitingAcknowledgement {
                    binding: request,
                    revision: 5,
                    request_cohort: None,
                },
            }),
            ..Default::default()
        }));
        let instance = HostedProcessorInstance {
            occurrence,
            processor: Box::new(Snapshot(Arc::clone(&observation))),
            continuity: HostedProcessorContinuity::Active { epoch, next_sample: 4 },
            notes: None,
            native_binding: NativeHostedBinding::Installed { lease, object },
            native_author_outstanding: Some(NativeHostedAuthorDelivery::new(
                proposal.clone(),
                None,
            )),
            native_reprepare_seed_bytes: 0,
            native_editor_detached: None,
            native_editor_attachment: None,
        };
        let mut host = PreparedProcessorHost {
            musical_time_reference: None,
            instances: vec![instance],
            entry_binding: None,
            parameter_events: ProcessorParameterEventScratch::new(0, 0),
            render_contract: AudioRenderContract {
                sample_rate: 48_000,
                channel_layout: AudioChannelLayout::Stereo,
                max_block_frames: 4,
                processing_mode: crate::AudioProcessingMode::Offline,
                processor_session_scratch_budget_bytes: 1024,
                public_output_lookahead_budget_frames: 0,
                compensation_delay_scratch_budget_bytes: 0,
            },
            maximum_parameter_lanes: 0,
            parameter_event_capacity: 0,
            session_scratch_bytes: 0,
            auxiliary_pcm: Vec::new(),
            sidechain_frame_db: Vec::new(),
            sidechain_gains: Vec::new(),
            prepared_notes: Vec::new(),
        };
        let fresh_request = crate::NativeAudioObjectBindRequest {
            binding: mondrian_core::AudioProcessorBindingId::new(),
            expected_object: object,
        };
        let prepared = PreparedParameterNativeBindings {
            current: root,
            records: vec![PreparedParameterNativeBinding {
                index: 0,
                prior_lease: lease,
                object,
                address: crate::AudioNativeProcessorAddress { root: next, local: next, ..address },
                receipt: crate::NativeAudioObjectBindReceipt::acknowledge(fresh_request, object)
                    .unwrap(),
            }],
        };
        let cancel = ExecutionCancellationToken::new();
        let context = ParameterNativeBindingContext {
            current_root: root,
            current_local: root,
            next_root: next,
            next_local: next,
            installed_serial: serial,
            proposal: &proposal,
            cancellation: &cancel,
        };
        let strip = crate::plan::CompiledChannelStrip {
            input_trim_db: 0.0,
            pre_fader: crate::plan::CompiledRack::default(),
            fader_db: 0.0,
            fader_automation: None,
            post_fader: crate::plan::CompiledRack::default(),
        };
        let program = crate::CompiledAudioProgram {
            musical_time_reference: None,
            output_id: match occurrence.owner {
                crate::AudioProcessorOccurrenceOwner::Output(id) => id,
                _ => unreachable!(),
            },
            contributions: Vec::new(),
            processing_scopes: Default::default(),
            transitions: Vec::new(),
            track_channels: Default::default(),
            buses: Default::default(),
            bus_order: Vec::new(),
            output: strip,
            routes: Vec::new(),
            sidechain_routes: Vec::new(),
            note_lanes: Vec::new(),
        };
        // Prove all old common shape/object/binding/delivery gates pass, so the
        // new final rejection is specifically the source/delivery revision edge.
        assert!(admit_parameter_native_source(
            &mut host.instances[0],
            &context,
            std::slice::from_ref(&proposal),
            &program,
            false
        )
        .unwrap()
        .is_some());
        let error = host
            .validate_terminal_author_bindings(
                &prepared,
                context,
                std::slice::from_ref(&proposal),
                &program,
            )
            .unwrap_err();
        assert!(matches!(
            error,
            NativeAudioAuthorAdmissionError::Pending {
                reason: crate::NativeAudioAuthorHoldReason::OutstandingDelivery,
                ..
            }
        ));
        for revision in [5, 6] {
            observation.lock().unwrap().author.as_mut().unwrap().source.revision = revision;
            host.validate_terminal_author_bindings(
                &prepared,
                context,
                std::slice::from_ref(&proposal),
                &program,
            )
            .unwrap();
        }
        assert!(host.instances[0]
            .native_author_outstanding
            .as_ref()
            .unwrap()
            .same_retained_proposal(&proposal));
        assert!(matches!(host.instances[0].native_binding,
            NativeHostedBinding::Installed { lease: current, object: actual } if current == lease && actual == object));
    }
}
