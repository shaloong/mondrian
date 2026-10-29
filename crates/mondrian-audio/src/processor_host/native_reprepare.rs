//! Exact retained restart admission and stopped same-child seed commands.
use super::*;
use crate::native_runtime_owner::NativeHostedBinding;
use crate::NativeAudioReprepareError as Error;
use mondrian_core::ExecutionCancellationToken;
use std::num::NonZeroU64;

pub(crate) struct ReprepareSelection {
    pub(crate) index: usize,
    pub(crate) lease: crate::AudioNativeProcessorOwnerLease,
    pub(crate) object: crate::NativeAudioObjectObservation,
    pub(crate) cohort: crate::NativeRequestCohort,
    pub(crate) constants: Vec<Option<f64>>,
    pub(crate) auxiliary_output_count: usize,
}

// One real inline capacity slot per Hosted occurrence. Existing metadata
// policy remains intact; this new known layout is charged separately once.
pub(crate) const fn native_reprepare_seed_slot_bytes() -> usize {
    std::mem::size_of::<usize>()
}

impl PreparedProcessorHost {
    pub(crate) fn inspect_native_reprepare(
        &mut self,
        schedule: &PreparedAudioSchedule,
        program: &crate::CompiledAudioProgram,
        ticket: crate::AudioProcessorReprepareTicket,
        serial: NonZeroU64,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<ReprepareSelection, Error> {
        let crate::NativeObservedRequest::Cohort(cohort) = ticket.request else {
            return Err(Error::Admission("restart has no exact callback cohort"));
        };
        let format = crate::NativeConfigurationFormat::for_retained_restart(cohort.request)
            .filter(|_| cohort.serial != 0 && !cohort.exhausted())
            .ok_or(Error::Admission(
                "only exact nonexhausted CLAP or VST3 latency restart is supported",
            ))?;
        if self.instances.len() != schedule.processors.len() {
            return Err(Error::Admission(
                "Hosted instances do not match the installed schedule",
            ));
        }
        let root = crate::AudioSessionCursorIdentity {
            prepared_plan_id: ticket.prepared_plan_id,
            session_id: ticket.session_id,
        };
        let mut selected = None;
        for (index, (instance, prepared)) in
            self.instances.iter_mut().zip(&schedule.processors).enumerate()
        {
            if cancellation.is_canceled() {
                return Err(Error::Cancelled);
            }
            if instance.occurrence != prepared.occurrence
                || instance.notes.is_some()
                || instance.native_editor_attachment.is_some()
                || instance.native_editor_detached.is_some()
                || instance.native_author_outstanding.is_some()
                || matches!(
                    instance.occurrence.owner,
                    crate::AudioProcessorOccurrenceOwner::Contribution { .. }
                )
                || instance.occurrence.insertion == crate::AudioProcessorInsertionPoint::Scope
                || prepared.parameter_curves.iter().flatten().any(|curve| !curve.is_constant())
                || matches!(
                    instance.continuity,
                    HostedProcessorContinuity::Unentered
                        | HostedProcessorContinuity::Pending { .. }
                        | HostedProcessorContinuity::Failed
                )
            {
                return Err(Error::Admission(
                    "notes, Scope, editor, author work or nonconstant processing is pending",
                ));
            }
            let target = instance.occurrence == ticket.occurrence;
            let (lease, object) = match instance.native_binding {
                NativeHostedBinding::Installed { lease, object }
                    if lease.address().root == root
                        && lease.address().local == root
                        && lease.address().occurrence == instance.occurrence
                        && lease.address().epoch == instance.continuity.native_epoch()?
                        && lease.installed_root_serial() == serial =>
                {
                    (lease, object)
                }
                NativeHostedBinding::Unbound
                    if !target
                        && !instance.processor.native_observations_supported()
                        && prepared.factory.native_child_settlement_requirement()
                            == mondrian_core::NativeChildSettlementRequirement::NotApplicable =>
                {
                    if isolate_adapter_call("restart peer pending preflight", || {
                        instance.processor.preflight_reprepare()
                    })?
                    .is_some()
                    {
                        return Err(Error::Admission("another processor requires reprepare"));
                    }
                    continue;
                }
                _ => {
                    return Err(Error::Admission(
                        "actual installed native binding is unavailable",
                    ))
                }
            };
            let observed = isolate_adapter_call("restart exact source admission", || {
                instance.processor.preflight_native_observations(object)
            })?;
            if observed.object != Some(object) || observed.editor_cleanup.is_some() {
                return Err(Error::Admission(
                    "native object changed or editor cleanup remains",
                ));
            }
            crate::native_audio_author_runtime::closed_source(observed.author.as_ref(), false)
                .map_err(|_| Error::Admission("native author source is not closed and empty"))?;
            if !target {
                if observed.pending_reprepare.is_some() {
                    return Err(Error::Admission(
                        "multiple native requests require a broader rebuild",
                    ));
                }
                continue;
            }
            if selected.is_some()
                || observed.pending_reprepare != Some(ticket.request)
                || lease.address().epoch != ticket.continuity_epoch
                || !matches!(
                    (
                        format,
                        &crate::schedule::processor(program, instance.occurrence)?.definition
                    ),
                    (
                        crate::NativeConfigurationFormat::ClapState,
                        mondrian_timeline::AudioProcessorDefinitionRef::Clap { .. }
                    ) | (
                        crate::NativeConfigurationFormat::Vst3ComponentAndController,
                        mondrian_timeline::AudioProcessorDefinitionRef::Vst3 { .. }
                    )
                )
            {
                return Err(Error::Admission(
                    "selected restart occurrence or cohort changed",
                ));
            }
            selected = Some((index, lease, object));
        }
        let (index, lease, object) =
            selected.ok_or(Error::Admission("no exact retained restart occurrence"))?;
        let prepared = &schedule.processors[index];
        let mut constants = Vec::new();
        constants
            .try_reserve_exact(prepared.parameter_ids.len())
            .map_err(|_| Error::Admission("constant-profile allocation failed"))?;
        for curve in &prepared.parameter_curves {
            // Preserve the full admitted lane order: None belongs to the native
            // configuration, not a controller/default scalar invented by restart.
            constants.push(match curve {
                None => None,
                Some(curve) if curve.is_constant() => Some(curve.constant_value()),
                Some(_) => return Err(Error::Admission("a native parameter is not constant")),
            });
        }
        if constants.len() != prepared.parameter_ids.len() {
            return Err(Error::Admission(
                "constant profile differs from the admitted lanes",
            ));
        }
        if cancellation.is_canceled() {
            return Err(Error::Cancelled);
        }
        Ok(ReprepareSelection {
            index,
            lease,
            object,
            cohort,
            constants,
            auxiliary_output_count: prepared.factory.auxiliary_outputs.buses.len(),
        })
    }

    pub(crate) fn service_native_reprepare(
        &mut self,
        selection: &ReprepareSelection,
        token: NonZeroU64,
        state_limit: usize,
        control_grant: usize,
    ) -> Result<crate::NativeConfigurationControlOutcome, Error> {
        let instance = self
            .instances
            .get_mut(selection.index)
            .ok_or(Error::Admission("restart Hosted index changed"))?;
        if !matches!(instance.native_binding, NativeHostedBinding::Installed { lease, object }
            if lease == selection.lease && object == selection.object)
        {
            return Err(Error::Admission("restart owner changed before service"));
        }
        Ok(isolate_adapter_call(
            "retained CLAP restart service",
            || {
                instance.processor.service_native_configuration(
                    crate::NativeConfigurationControlRequest {
                        expected_cohort: selection.cohort,
                        control_token: token,
                        canonical_constant_values: &selection.constants,
                        configuration_limit_bytes: state_limit,
                        control_buffer_grant_bytes: control_grant,
                    },
                )
            },
        )?)
    }
    pub(crate) fn stage_native_reprepare_audio_inputs(
        &mut self,
        selected: &ReprepareSelection,
        request: crate::ClapAudioInputBufferRequest,
    ) -> Result<crate::ClapAudioInputBufferOutcome, Error> {
        let instance = self
            .instances
            .get_mut(selected.index)
            .ok_or(Error::Admission("input stage Hosted index changed"))?;
        if request.proposal.observed_cohort != selected.cohort
            || !matches!(instance.native_binding, NativeHostedBinding::Installed { lease, object }
                if lease == selected.lease && object == selected.object)
        {
            return Err(Error::Admission("input stage original owner changed"));
        }
        Ok(isolate_adapter_call("native input buffer stage", || {
            instance.processor.stage_clap_audio_input_buffers(request)
        })?)
    }
    pub(crate) fn stage_native_reprepare_audio_input_mapping(
        &mut self,
        selected: &ReprepareSelection,
        request: crate::ClapAudioInputMappingRequest,
    ) -> Result<crate::ClapAudioInputMappingOutcome, Error> {
        let instance = self
            .instances
            .get_mut(selected.index)
            .ok_or(Error::Admission("input stage Hosted index changed"))?;
        if request.buffers.request.proposal.observed_cohort != selected.cohort
            || !matches!(instance.native_binding, NativeHostedBinding::Installed { lease, object }
                if lease == selected.lease && object == selected.object)
        {
            return Err(Error::Admission("input stage original owner changed"));
        }
        Ok(isolate_adapter_call("native input mapping stage", || {
            instance.processor.stage_clap_audio_input_mapping(request)
        })?)
    }
}

impl PreparedProcessorHost {
    // Restart alone permits the selected latency/tail to differ. The ordinary
    // author transfer validator remains exact and is not reused or relaxed.
    pub(crate) fn validate_reprepare_transfer(
        &mut self,
        old: &PreparedAudioSchedule,
        next: &PreparedAudioSchedule,
        selection: &ReprepareSelection,
        expected: crate::AudioProcessorExecutionContract,
        inputs: Option<&crate::ClapAudioInputProposal>,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<(), Error> {
        if self.instances.len() != old.processors.len()
            || old.processors.len() != next.processors.len()
            || selection.index >= self.instances.len()
        {
            return Err(Error::Admission("restart transfer shape changed"));
        }
        for (index, (instance, (before, after))) in self
            .instances
            .iter_mut()
            .zip(old.processors.iter().zip(&next.processors))
            .enumerate()
        {
            let a = before.factory.contract();
            let b = after.factory.contract();
            if instance.occurrence != before.occurrence
                || before.occurrence != after.occurrence
                || before.parameter_ids != after.parameter_ids
                || before.parameter_curves.len() != after.parameter_curves.len()
                || before.parameter_curves.iter().zip(&after.parameter_curves).any(|(a, b)| match (
                    a, b,
                ) {
                    (None, None) => false,
                    (Some(a), Some(b)) => {
                        !a.is_constant()
                            || !b.is_constant()
                            || a.constant_value().to_bits() != b.constant_value().to_bits()
                    }
                    _ => true,
                })
                || if index == selection.index
                    && let Some(inputs) = inputs
                {
                    !inputs.matches_reconfigured_inputs(
                        before.factory.auxiliary_inputs(),
                        after.factory.auxiliary_inputs(),
                    )
                } else {
                    before.factory.auxiliary_inputs() != after.factory.auxiliary_inputs()
                }
                || before.factory.auxiliary_outputs() != after.factory.auxiliary_outputs()
                || before.factory.note_inputs() != after.factory.note_inputs()
                || instance.notes.is_some()
                || instance.native_author_outstanding.is_some()
                || instance.native_editor_attachment.is_some()
                || instance.native_editor_detached.is_some()
                || (index != selection.index && a != b)
                || (index == selection.index
                    && (b.algorithmic_latency_frames() != expected.algorithmic_latency_frames()
                        || b.tail() != expected.tail()
                        || a.requires_state_entry() != b.requires_state_entry()
                        || a.realtime_capable() != b.realtime_capable()
                        || a.offline_capable() != b.offline_capable()
                        || (inputs.is_none()
                            && a.session_scratch_bytes() != b.session_scratch_bytes())))
            {
                return Err(Error::Admission(
                    "restart changed non-scheduling contract or owners",
                ));
            }
            if index == selection.index
                && inputs.is_some()
                && inputs.unwrap().isolated_scratch_bytes(
                    a.session_scratch_bytes(),
                    self.render_contract.max_block_frames,
                    before.factory.auxiliary_inputs(),
                ) != Some(b.session_scratch_bytes())
            {
                return Err(Error::Admission(
                    "migrated resource model does not match the prepared Factory",
                ));
            }
            if cancellation.is_canceled() {
                return Err(Error::Cancelled);
            }
            let NativeHostedBinding::Installed { lease, object } = instance.native_binding else {
                if index != selection.index
                    && !instance.processor.native_observations_supported()
                    && before.factory.native_child_settlement_requirement()
                        == mondrian_core::NativeChildSettlementRequirement::NotApplicable
                    && isolate_adapter_call("restart transfer peer preflight", || {
                        instance.processor.preflight_reprepare()
                    })?
                    .is_none()
                {
                    continue;
                }
                return Err(Error::Admission("restart transfer binding unavailable"));
            };
            if lease.address().root != selection.lease.address().root
                || lease.address().local != selection.lease.address().local
                || lease.installed_root_serial() != selection.lease.installed_root_serial()
                || lease.address().occurrence != instance.occurrence
                || (index == selection.index
                    && (lease != selection.lease || object != selection.object))
            {
                return Err(Error::Admission("restart transfer owner changed"));
            }
            let observed = isolate_adapter_call("committed restart source recheck", || {
                instance.processor.preflight_native_observations(object)
            })?;
            if observed.object != Some(object)
                || observed.editor_cleanup.is_some()
                || observed.pending_reprepare.is_some()
            {
                return Err(Error::Admission("committed restart has new pending work"));
            }
            crate::native_audio_author_runtime::closed_source(observed.author.as_ref(), false)
                .map_err(|_| Error::Admission("restart author source is not closed and empty"))?;
            if index == selection.index {
                let cohort = isolate_adapter_call("committed restart cohort recheck", || {
                    instance.processor.native_configuration_cohort()
                })?;
                if cohort.is_some() {
                    return Err(Error::Cohort(cohort));
                }
            }
        }
        Ok(())
    }

    pub(crate) fn stage_reprepare_seed(
        &mut self,
        selection: &ReprepareSelection,
        request: crate::NativeConfigurationSeedRequest<'_>,
    ) -> Result<crate::NativeConfigurationSeedOutcome, Error> {
        let instance = self.restart_seed_instance(selection)?;
        Ok(isolate_adapter_call("retained restart seed stage", || {
            instance.processor.stage_native_configuration_seed(request)
        })?)
    }

    pub(crate) fn commit_reprepare_seed(
        &mut self,
        selection: &ReprepareSelection,
        ticket: crate::NativeConfigurationSeedTicket,
    ) -> Result<crate::NativeConfigurationSeedOutcome, Error> {
        let instance = self.restart_seed_instance(selection)?;
        let outcome = isolate_adapter_call("retained restart seed commit", || {
            instance.processor.commit_native_configuration_seed(ticket)
        })?;
        if let crate::NativeConfigurationSeedOutcome::Accepted(receipt) = outcome {
            // Authenticated same-child outcome is retained before any caller
            // postcheck/CAS can fail. This moves with the actual Hosted record.
            instance.native_reprepare_seed_bytes = receipt.child_seed_capacity_bytes;
        }
        Ok(outcome)
    }

    pub(crate) fn reprepare_seed_weights(&self, index: usize) -> Result<(usize, usize), Error> {
        let selected = self
            .instances
            .get(index)
            .ok_or(Error::Admission("restart seed occurrence changed"))?;
        let total = self.instances.iter().try_fold(0_usize, |sum, instance| {
            sum.checked_add(instance.native_reprepare_seed_bytes)
                .ok_or(Error::Admission("committed seed owner extent overflow"))
        })?;
        Ok((total, selected.native_reprepare_seed_bytes))
    }

    fn restart_seed_instance(
        &mut self,
        selection: &ReprepareSelection,
    ) -> Result<&mut HostedProcessorInstance, Error> {
        let instance = self
            .instances
            .get_mut(selection.index)
            .ok_or(Error::Admission("restart seed occurrence changed"))?;
        if !matches!(instance.native_binding, NativeHostedBinding::Installed {lease,object}
            if lease == selection.lease && object == selection.object)
        {
            return Err(Error::Admission("restart seed object changed"));
        }
        Ok(instance)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::processor_output_tests::{fixture, plan, OutputResolver};

    fn hosts() -> (PreparedProcessorHost, PreparedProcessorHost) {
        let sequence = fixture(mondrian_core::AudioChannelLayout::Mono);
        let plan = plan(&sequence, &OutputResolver::default()).unwrap();
        let mut current =
            PreparedProcessorHost::allocate_storage(&plan.schedule, plan.contract()).unwrap();
        current.populate_instances(&plan.schedule, None).unwrap();
        let next =
            PreparedProcessorHost::allocate_storage(&plan.schedule, plan.contract()).unwrap();
        assert_eq!(
            current.instances.len(),
            2,
            "actual portable Factory-created Hosted instances"
        );
        assert!(next.instances.is_empty());
        (current, next)
    }

    // These controls model already recorded capacities in actual Hosted records.
    // They do not run/acknowledge native commands, invent leases, or qualify two
    // child processes. The real mode7 gate separately exercises Accepted writes.
    #[test]
    fn committed_seed_aggregation_preserves_peer_charges_across_hosted_transfer() {
        let (mut current, mut next) = hosts();
        assert_eq!(current.reprepare_seed_weights(0).unwrap(), (0, 0));
        assert_eq!(current.reprepare_seed_weights(1).unwrap(), (0, 0));
        let processors = [
            std::ptr::from_ref(current.instances[0].processor.as_ref()).cast::<()>(),
            std::ptr::from_ref(current.instances[1].processor.as_ref()).cast::<()>(),
        ];
        assert_ne!(processors[0], processors[1]);
        current.instances[0].native_reprepare_seed_bytes = 5;
        current.instances[1].native_reprepare_seed_bytes = 11;
        assert_eq!(current.reprepare_seed_weights(0).unwrap(), (16, 5));
        assert_eq!(current.reprepare_seed_weights(1).unwrap(), (16, 11));
        // Replacing only A must preserve B's distinct committed owner charge.
        current.instances[0].native_reprepare_seed_bytes = 7;
        assert_eq!(current.reprepare_seed_weights(0).unwrap(), (18, 7));
        assert_eq!(current.reprepare_seed_weights(1).unwrap(), (18, 11));
        current.swap_state_entry_instances(&mut next);
        assert!(current.instances.is_empty());
        assert_eq!(next.instances.len(), 2);
        for (instance, original) in next.instances.iter().zip(processors) {
            assert_eq!(
                std::ptr::from_ref(instance.processor.as_ref()).cast::<()>(),
                original,
                "the same processor Box and its capacity attribution move together"
            );
        }
        assert_eq!(next.reprepare_seed_weights(0).unwrap(), (18, 7));
        assert_eq!(next.reprepare_seed_weights(1).unwrap(), (18, 11));
        assert!(matches!(
            current.reprepare_seed_weights(0),
            Err(Error::Admission("restart seed occurrence changed"))
        ));
    }

    #[test]
    fn committed_seed_aggregation_rejects_overflow_and_invalid_index_without_mutation() {
        let (mut current, _) = hosts();
        current.instances[0].native_reprepare_seed_bytes = usize::MAX;
        current.instances[1].native_reprepare_seed_bytes = 1;
        let before = [
            current.instances[0].native_reprepare_seed_bytes,
            current.instances[1].native_reprepare_seed_bytes,
        ];
        assert!(matches!(
            current.reprepare_seed_weights(0),
            Err(Error::Admission("committed seed owner extent overflow"))
        ));
        // The existing selected-index error still precedes aggregation overflow.
        assert!(matches!(
            current.reprepare_seed_weights(2),
            Err(Error::Admission("restart seed occurrence changed"))
        ));
        assert_eq!(
            [
                current.instances[0].native_reprepare_seed_bytes,
                current.instances[1].native_reprepare_seed_bytes
            ],
            before
        );
        assert_eq!(current.instances.len(), 2);
    }
}
