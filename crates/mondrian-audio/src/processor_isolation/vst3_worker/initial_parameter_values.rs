//! One initial-only handoff; never a runtime GUI-author consumer.
use super::*;
use vst3_host::controller_parameters::ControllerParameterQueryFrame;
#[cfg(target_os = "windows")]
use vst3_host::initial_parameter_input::{InitialParameterStorage, InitialParameterValue};

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
        if !self.service_fresh_parameter_values() {
            return false;
        }
        // Also closes a zero-work initial window before native GUI is authorized.
        self.initial_signal_phase = InitialSignalPhase::AwaitingFirstSignal;
        true
    }

    /// Fresh loader calls only after proving a strictly new native incarnation.
    #[cfg(target_os = "windows")]
    pub(super) fn service_fresh_parameter_values(&mut self) -> bool {
        if !matches!(
            self.configuration_phase,
            super::super::native_configuration_control::NativeConfigurationPhase::Admitted
        ) || self.native_author_delivery.is_some()
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
            || source.pending_events() != source.pending_dsp_feedback_values()
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
        let count = plugin.visit_controller_parameters_bounded(
            admitted.len(),
            &mut frame,
            &mut |index, observed| {
                let expected = admitted.get(index).ok_or_else(invalid_native_parameter)?;
                if observed.id() != expected.id
                    || !observed.title_matches(&expected.name)
                    || observed.default_normalized().to_bits()
                        != expected.default_normalized.to_bits()
                    || observed.step_count() != expected.step_count
                    || observed.flags() != expected.flags
                    || !observed.normalized().is_finite()
                    || !(0.0..=1.0).contains(&observed.normalized())
                {
                    return Err(invalid_native_parameter());
                }
                if !expected.read_only {
                    observations.values[writable] = InitialParameterValue {
                        id: observed.id(),
                        normalized: observed.normalized(),
                    };
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
    pub(super) fn service_fresh_parameter_values(&mut self) -> bool {
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
        if !source.closed_empty() || source.pending_dsp_feedback_values() != 0 {
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
        // No source is drained and no newer callback is retired by this consumer.
        if !matches!(plugin.editor_event_source_status(), Ok(after) if after == source)
            || plugin.pending_restart_cohort() != Some(cohort)
        {
            return true;
        }
        let _ = plugin.retire_restart_cohort(cohort);
        true
    }
}
#[cfg(target_os = "windows")]
fn invalid_native_parameter() -> vst3_host::Error {
    vst3_host::Error::InvalidParameter("controller identity or normalized value changed".to_owned())
}
