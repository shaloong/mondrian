//! Initial-only native value invalidation consumer. No runtime author service.

use super::*;
use vst3_host::controller_parameters::ControllerParameterQueryFrame;

impl Vst3Worker {
    /// The existing one-shot initial signal is the lifecycle authority. None DSP
    /// values alone cannot prove this phase after a runtime reset/reconfiguration.
    pub(super) fn service_first_parameter_values(&mut self) {
        if !self.initial_signal_available
            || self.last_end.is_some()
            || self.prepared_author_entry.is_some()
            || self.last_parameter_values.iter().any(Option::is_some)
        {
            return;
        }
        self.service_fresh_parameter_values();
    }

    /// Called only at first-signal admission or inside the actual fresh-object
    /// loader after its strict incarnation comparison, before any new-object DSP.
    /// Refusal retains the object and original typed pending reprepare request.
    pub(super) fn service_fresh_parameter_values(&mut self) {
        if !matches!(
            self.configuration_phase,
            super::super::native_configuration_control::NativeConfigurationPhase::Admitted
        ) || self.native_author_delivery.is_some()
            || self.last_parameter_values.iter().any(Option::is_some)
        {
            return;
        }
        let Some(plugin) = self.plugin.as_mut() else {
            return;
        };
        let Some(cohort) = plugin.pending_restart_cohort() else {
            return;
        };
        if cohort.flags().bits() != 4 || cohort.exhausted() {
            return;
        }
        let Ok(source) = plugin.editor_event_source_status() else {
            return;
        };
        if !source.closed_empty() || source.pending_dsp_feedback_values() != 0 {
            return;
        }
        let admitted = &self.payload.parameters;
        if admitted.len() > MAX_PARAMETERS {
            return;
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
            return;
        }
        // Reentrant edits and repeated same-bit callbacks are later obligations.
        // No source is drained and no newer callback is retired by this consumer.
        if !matches!(plugin.editor_event_source_status(), Ok(after) if after == source)
            || plugin.pending_restart_cohort() != Some(cohort)
        {
            return;
        }
        let _ = plugin.retire_restart_cohort(cohort);
    }
}
