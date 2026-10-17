//! One initial-only handoff; never a runtime GUI-author consumer.
use super::*;
use vst3_host::controller_parameters::ControllerParameterQueryFrame;
use vst3_host::initial_parameter_input::InitialParameterStorage;
#[cfg(target_os = "windows")]
use vst3_host::initial_parameter_input::InitialParameterValue;

/// Fixed bounded metadata observation scratch; charged as main inline storage.
#[cfg(target_os = "windows")]
pub(super) struct InitialParameterFrame {
    values: [InitialParameterValue; MAX_PARAMETERS],
}
impl Vst3Worker {
    pub(super) fn service_first_parameter_values(&mut self) -> bool {
        if self.initial_signal_phase != InitialSignalPhase::Unserviced {
            return true;
        }
        if self.last_end.is_some()
            || self.prepared_author_entry.is_some()
            || self.last_parameter_values.iter().any(Option::is_some)
        {
            return false;
        }
        if !self.service_fresh_parameter_values(None) {
            return false;
        }
        // Also closes a zero-work initial window before native GUI is authorized.
        self.initial_signal_phase = InitialSignalPhase::AwaitingFirstSignal;
        true
    }

    /// Fresh loader calls only after proving a strictly new native incarnation.
    #[cfg(target_os = "windows")]
    pub(super) fn service_fresh_parameter_values(
        &mut self,
        author_entry: Option<&crate::NativeAudioAuthorStateEntryRequest<'_>>,
    ) -> bool {
        // The only retained delivery exception belongs to the OLD incarnation.
        // Its original request is not permission by itself: match the full
        // None-prefix receipt and the installed actual fresh object again.
        let retained_old_entry = author_entry.is_some_and(|request| {
            self.selected_terminal_delivery_matches(request)
                && self.payload.state.as_deref() == Some(request.configuration)
                && request.binding.expected_object.process_id().get() == std::process::id()
                && self.plugin.as_ref().is_some_and(|plugin| {
                    plugin.native_editor_attachment_owner().is_some_and(|owner| {
                        owner.serial() > request.binding.expected_object.object_serial()
                    })
                })
        });
        if !matches!(
            self.configuration_phase,
            super::super::native_configuration_control::NativeConfigurationPhase::Admitted
        ) || (self.native_author_delivery.is_some() && !retained_old_entry)
            || self.last_parameter_values.iter().any(Option::is_some)
        {
            return false;
        }
        let Some(plugin) = self.plugin.as_mut() else {
            return false;
        };
        let Some(cohort) = plugin.pending_restart_cohort() else {
            return false;
        };
        if cohort.exhausted() || !matches!(cohort.flags().bits(), 0 | 4 | 20) {
            return false;
        }
        let Ok(source) = plugin.editor_event_source_status() else {
            return false;
        };
        if source.failure().is_some()
            || source.open_parameter_edits() != 0
            || source.group_depth() != 0
        {
            return false;
        }
        let admitted = &self.payload.parameters;
        if admitted.len() > MAX_PARAMETERS {
            return false;
        }
        let mut observations = InitialParameterFrame {
            values: [InitialParameterValue::default(); MAX_PARAMETERS],
        };
        let mut writable = 0;
        let mut frame = ControllerParameterQueryFrame::default();
        let count = visit_admitted_controller_values(
            plugin,
            admitted,
            &mut frame,
            &mut |index, normalized| {
                if !admitted[index].read_only {
                    observations.values[writable] =
                        InitialParameterValue { id: admitted[index].id, normalized };
                    writable += 1;
                }
                Ok(())
            },
        );
        if !matches!(count, Ok(count) if count == admitted.len()) {
            return false;
        }
        let (vector_bytes, com_payload_bytes) =
            self.native_lane.map_or((0, 0), |profile| profile.processor_input_storage());
        plugin
            .handoff_initial_parameter_input(
                cohort,
                source,
                &observations.values[..writable],
                InitialParameterStorage { vector_bytes, com_payload_bytes },
            )
            .is_ok()
    }
    // Inline platforms retain their existing flags4-only consumer. No scoped
    // storage grant or newly supported baseline transfer is invented here.
    #[cfg(not(target_os = "windows"))]
    pub(super) fn service_fresh_parameter_values(
        &mut self,
        _author_entry: Option<&crate::NativeAudioAuthorStateEntryRequest<'_>>,
    ) -> bool {
        if !matches!(
            self.configuration_phase,
            super::super::native_configuration_control::NativeConfigurationPhase::Admitted
        ) || self.native_author_delivery.is_some()
            || self.last_parameter_values.iter().any(Option::is_some)
        {
            return true;
        }
        let Some(plugin) = self.plugin.as_mut() else {
            return true;
        };
        let Some(cohort) = plugin.pending_restart_cohort() else {
            return true;
        };
        if cohort.flags().bits() != 4 || cohort.exhausted() {
            return true;
        }
        let Ok(source) = plugin.editor_event_source_status() else {
            return true;
        };
        if source.failure().is_some()
            || source.open_parameter_edits() != 0
            || source.group_depth() != 0
            || source.pending_dsp_feedback_values() != 0
        {
            return true;
        }
        let admitted = &self.payload.parameters;
        if admitted.len() > MAX_PARAMETERS {
            return true;
        }
        let mut frame = ControllerParameterQueryFrame::default();
        let count = plugin.visit_controller_parameters_bounded(
            admitted.len(),
            &mut frame,
            &mut |index, observed| {
                let expected = admitted.get(index).ok_or_else(|| {
                    vst3_host::Error::InvalidParameter("unadmitted controller parameter".to_owned())
                })?;
                if observed.id() != expected.id
                    || !observed.title_matches(&expected.name)
                    || observed.default_normalized() != expected.default_normalized
                    || observed.step_count() != expected.step_count
                    || observed.flags() != expected.flags
                    || !observed.normalized().is_finite()
                    || !(0.0..=1.0).contains(&observed.normalized())
                {
                    return Err(vst3_host::Error::InvalidParameter(
                        "controller identity or normalized value changed".to_owned(),
                    ));
                }
                Ok(())
            },
        );
        if !matches!(count, Ok(count) if count == admitted.len()) {
            return true;
        }
        // Reentrant edits and repeated same-bit callbacks are later obligations.
        // The SDK consumes only a proved intact marker-only initial prefix,
        // with zero input storage under the same source/feedback/input locks.
        if !matches!(plugin.editor_event_source_status(), Ok(after) if after == source)
            || plugin.pending_restart_cohort() != Some(cohort)
        {
            return true;
        }
        let _ = plugin.handoff_initial_parameter_input(
            cohort,
            source,
            &[],
            InitialParameterStorage::default(),
        );
        true
    }
}
fn invalid_native_parameter() -> vst3_host::Error {
    vst3_host::Error::InvalidParameter("controller identity or normalized value changed".to_owned())
}

/// Two same-owner consumers share the admitted metadata comparison, not a
/// second parameter inventory. The callback receives each actual current value.
pub(super) fn visit_admitted_controller_values(
    plugin: &Plugin,
    admitted: &[Vst3ParameterDescriptor],
    frame: &mut ControllerParameterQueryFrame,
    visit: &mut impl FnMut(usize, f64) -> vst3_host::Result<()>,
) -> vst3_host::Result<usize> {
    if admitted.len() > MAX_PARAMETERS {
        return Err(invalid_native_parameter());
    }
    let count = plugin.visit_controller_parameters_bounded(
        admitted.len(),
        frame,
        &mut |index, observed| {
            let expected = admitted.get(index).ok_or_else(invalid_native_parameter)?;
            if observed.id() != expected.id
                || !observed.title_matches(&expected.name)
                || observed.default_normalized().to_bits() != expected.default_normalized.to_bits()
                || observed.step_count() != expected.step_count
                || observed.flags() != expected.flags
                || !observed.normalized().is_finite()
                || !(0.0..=1.0).contains(&observed.normalized())
            {
                return Err(invalid_native_parameter());
            }
            visit(index, observed.normalized())
        },
    )?;
    if count != admitted.len() {
        return Err(invalid_native_parameter());
    }
    Ok(count)
}
