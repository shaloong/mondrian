//! Exact retained CLAP capture admission. No seed, Plan or binding publication.
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
    pub(crate) constants: Vec<f64>,
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
        if cohort.exhausted()
            || cohort.request != mondrian_core::NativeAudioReprepareRequest::ClapRestart
        {
            return Err(Error::Admission(
                "only nonexhausted CLAP restart is supported",
            ));
        }
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
                    crate::schedule::processor(program, instance.occurrence)?.definition,
                    mondrian_timeline::AudioProcessorDefinitionRef::Clap { .. }
                )
            {
                return Err(Error::Admission(
                    "selected restart occurrence or cohort changed",
                ));
            }
            selected = Some((index, lease, object));
        }
        let (index, lease, object) =
            selected.ok_or(Error::Admission("no exact CLAP restart occurrence"))?;
        let prepared = &schedule.processors[index];
        let mut constants = Vec::new();
        constants
            .try_reserve_exact(prepared.parameter_ids.len())
            .map_err(|_| Error::Admission("constant-profile allocation failed"))?;
        for curve in &prepared.parameter_curves {
            let curve = curve.as_ref().filter(|curve| curve.is_constant()).ok_or(
                Error::Admission("a native parameter has no canonical constant value"),
            )?;
            constants.push(curve.constant_value());
        }
        if constants.len() != prepared.parameter_ids.len() {
            return Err(Error::Admission(
                "constant profile differs from the admitted lanes",
            ));
        }
        if cancellation.is_canceled() {
            return Err(Error::Cancelled);
        }
        Ok(ReprepareSelection { index, lease, object, cohort, constants })
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
}
