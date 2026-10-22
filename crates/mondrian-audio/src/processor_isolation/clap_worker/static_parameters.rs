//! First-view static Some exchange on the original, exclusively owned active CLAP
//! processor. Native output remains in the one existing Poll/author/Ack source.

use super::*;
use crate::NativeAudioEditorRefusal as Refusal;
use clack_extensions::params::{ParamInfoBuffer, ParamInfoFlags, PluginParams};

impl ClapWorker {
    /// All ordinary refusals precede the first native flush. Once it executes,
    /// errors are terminal: no rollback or no-op refusal is manufactured.
    pub(super) fn project_idle_static_parameters(
        &mut self,
        request: &crate::NativeAudioEditorOpenRequest,
    ) -> Result<Option<Refusal>, AudioProcessorHostError> {
        let values = request.static_parameter_values();
        if values.is_empty() {
            return Ok(None);
        }
        request.owned_layout()?;
        if !self.native_editor_binding_matches(request.binding()) {
            return Ok(Some(Refusal::OwnerChanged));
        }
        if self.native_author_delivery.is_some()
            || self.configuration_phase.blocked_request().is_some()
            || self.configuration_phase.ready_token().is_some()
            || !self.author_source_status().closed_empty()
            || self.output_events.has_refusals()
            || self
                .instance
                .access_shared_handler(|host| host.services.wrong_thread.load(Ordering::Acquire))
        {
            return Ok(Some(Refusal::ConfigurationPending));
        }
        let cohort = self.shared.restart_cohort();
        if cohort.pending() {
            return Ok(Some(Refusal::ConfigurationPending));
        }
        let source = self.shared.parameter_source.snapshot();
        let handle = self.instance.plugin_handle();
        let Some(params) = handle.get_extension::<PluginParams>() else {
            return Ok(Some(Refusal::UnsupportedAdapter));
        };
        if !params.supports_flush(&handle) {
            return Ok(Some(Refusal::UnsupportedAdapter));
        }
        if let Some(refusal) = self.validate_static_parameter_metadata(params, values)? {
            return Ok(Some(refusal));
        }
        // Prepare every event in the original prepared lane order, with no
        // growth of the already admitted B*L parameter/ordinal owners.
        if !prepare_input(&mut self.input_events, &self.parameter_lanes, values) {
            return Ok(Some(Refusal::ConfigurationPending));
        }
        if self.gui.close_requested()?
            || self.instance.access_shared_handler(|host| host.services.close_requested())
        {
            self.input_events.clear();
            return Ok(Some(Refusal::ConfigurationPending));
        }
        // Metadata and GUI capability queries can create real obligations.
        // Those stay pending; no native mutation has occurred on this path.
        if !self.native_editor_binding_matches(request.binding())
            || self.shared.restart_cohort() != cohort
            || self.shared.parameter_source.snapshot() != source
            || !self.author_source_status().closed_empty()
            || self.output_events.has_refusals()
            || self
                .instance
                .access_shared_handler(|host| host.services.wrong_thread.load(Ordering::Acquire))
        {
            self.input_events.clear();
            return Ok(Some(Refusal::ConfigurationPending));
        }
        let native =
            self.processor.as_mut().ok_or_else(|| unavailable("CLAP processor is closed"))?;
        {
            // Full-owner dispatch is after real DSP join. The exclusive SDK
            // handle forbids concurrent process; only flush owns the audio role.
            let _audio_role = self.shared.enter_audio_thread();
            let mut output = self.output_events.writer(ClapOutputOrigin::ActiveFlush)?;
            params.flush_active(
                &mut native.plugin_handle(),
                &self.input_events.as_input(),
                &mut output.as_output(),
            );
        }
        self.input_events.clear();
        // Healthy new output and new request_flush remain intact for Poll. In
        // particular DONT_RECORD is not discarded and no Ack is synthesized.
        let after = self.shared.parameter_source.snapshot();
        if !self.native_editor_binding_matches(request.binding())
            || self.shared.restart_cohort() != cohort
            || after.incomplete != 0
            || after.publishing_output
            || self.output_events.has_refusals()
            || self
                .instance
                .access_shared_handler(|host| host.services.wrong_thread.load(Ordering::Acquire))
        {
            return Err(invalid(
                "CLAP static parameter exchange changed native ownership or failed",
            ));
        }
        if self.validate_static_parameter_metadata(params, values)?.is_some() {
            return Err(invalid(
                "CLAP static parameter metadata changed during exchange",
            ));
        }
        // The post-call metadata query can itself report callbacks. Keep valid
        // parameter prefixes, but reject sticky failure or changed restart owner.
        let after = self.shared.parameter_source.snapshot();
        if !self.native_editor_binding_matches(request.binding())
            || self.shared.restart_cohort() != cohort
            || after.incomplete != 0
            || after.publishing_output
            || self.output_events.has_refusals()
            || self
                .instance
                .access_shared_handler(|host| host.services.wrong_thread.load(Ordering::Acquire))
        {
            return Err(invalid(
                "CLAP static parameter metadata query changed native ownership or failed",
            ));
        }
        Ok(None)
    }

    /// Processing metadata is checked over the complete native census; only
    /// selected lanes need to admit this host write/active-flush operation.
    fn validate_static_parameter_metadata(
        &mut self,
        params: PluginParams,
        values: &[(u32, u64)],
    ) -> Result<Option<Refusal>, AudioProcessorHostError> {
        for (index, (lane, _)) in values.iter().enumerate() {
            if self.parameter_lanes.get(*lane as usize).is_none_or(|(id, _, _, _)| {
                !self.parameter_signatures.iter().any(|signature| signature.id() == id.get())
            }) || values[..index].iter().any(|(previous, _)| previous == lane)
            {
                return Err(invalid(
                    "CLAP static parameter processing lane is not admitted",
                ));
            }
        }
        let handle = self.instance.plugin_handle();
        if params.count(&handle) as usize != self.parameter_signatures.len() {
            return Err(invalid("CLAP static parameter census changed"));
        }
        for (index, signature) in self.parameter_signatures.iter().enumerate() {
            let mut buffer = ParamInfoBuffer::new();
            let info = params
                .get_info(&handle, index as u32, &mut buffer)
                .ok_or_else(|| invalid("CLAP static parameter metadata unavailable"))?;
            if !signature.matches(&info) {
                return Err(invalid("CLAP static parameter processing metadata changed"));
            }
            for &(lane, bits) in values {
                if self.parameter_lanes[lane as usize].0.get() == signature.id()
                    && !selected_value_is_supported(
                        info.flags,
                        info.min_value,
                        info.max_value,
                        f64::from_bits(bits),
                    )
                {
                    return Ok(Some(Refusal::UnsupportedAdapter));
                }
            }
        }
        Ok(None)
    }
}

fn selected_value_is_supported(flags: ParamInfoFlags, min: f64, max: f64, value: f64) -> bool {
    !flags.intersects(ParamInfoFlags::IS_READONLY | ParamInfoFlags::REQUIRES_PROCESS)
        && min.is_finite()
        && max.is_finite()
        && min <= max
        && value.is_finite()
        && value >= min
        && value <= max
        && (!flags.contains(ParamInfoFlags::IS_STEPPED)
            || value.fract() == 0.0 && value.abs() <= (1_u64 << f64::MANTISSA_DIGITS) as f64)
}

fn prepare_input(
    input: &mut ClapInputEvents,
    lanes: &[(ClapId, f64, f64, bool)],
    values: &[(u32, u64)],
) -> bool {
    input.clear();
    for (lane, (id, _, _, _)) in lanes.iter().enumerate() {
        let Some((_, bits)) = values.iter().find(|(selected, _)| *selected as usize == lane) else {
            continue;
        };
        if !input.try_push(ParamValueEvent::new(
            0,
            *id,
            Pckn::match_all(),
            f64::from_bits(*bits),
        )) {
            input.clear();
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selected_static_values_keep_plain_domain_and_do_not_require_automatable() {
        for flags in [
            ParamInfoFlags::empty(),
            ParamInfoFlags::IS_HIDDEN,
            ParamInfoFlags::IS_AUTOMATABLE,
        ] {
            assert!(selected_value_is_supported(flags, 0.0, 1.0, -0.0));
            assert!(selected_value_is_supported(flags, 0.0, 1.0, 0.25));
        }
        for flags in [
            ParamInfoFlags::IS_READONLY,
            ParamInfoFlags::REQUIRES_PROCESS,
        ] {
            assert!(!selected_value_is_supported(flags, 0.0, 1.0, 0.25));
        }
        for value in [f64::NAN, f64::INFINITY, -0.1, 1.1] {
            assert!(!selected_value_is_supported(
                ParamInfoFlags::empty(),
                0.0,
                1.0,
                value
            ));
        }
        assert!(selected_value_is_supported(
            ParamInfoFlags::IS_STEPPED,
            0.0,
            4.0,
            3.0
        ));
        assert!(!selected_value_is_supported(
            ParamInfoFlags::IS_STEPPED,
            0.0,
            4.0,
            0.25
        ));
    }
    #[test]
    fn static_input_uses_native_mapping_prepared_order_and_refuses_actual_capacity_without_growth()
    {
        let lanes = [
            (ClapId::new(900), 0.0, 1.0, false),
            (ClapId::new(77), 0.0, 1.0, false),
            (ClapId::new(0), 0.0, 1.0, false),
        ];
        let selected = [(2, 0.75_f64.to_bits()), (0, (-0.0_f64).to_bits())];
        let mut input = ClapInputEvents::try_with_note_capacity(2, 0).unwrap();
        let bytes = input.allocated_bytes().unwrap();
        assert!(prepare_input(&mut input, &lanes, &selected));
        let actual: Vec<_> = input
            .as_input()
            .iter()
            .map(|event| {
                let Some(clack_host::events::spaces::CoreEventSpace::ParamValue(value)) =
                    event.as_core_event()
                else {
                    panic!("typed scalar expected")
                };
                assert!(value.cookie().as_raw().is_null());
                assert_eq!(event.header().time(), 0);
                (value.param_id().unwrap().get(), value.value().to_bits())
            })
            .collect();
        assert_eq!(
            actual,
            vec![(900, (-0.0_f64).to_bits()), (0, 0.75_f64.to_bits())]
        );
        assert_eq!(input.allocated_bytes().unwrap(), bytes);
        let mut small = ClapInputEvents::try_with_note_capacity(1, 0).unwrap();
        let bytes = small.allocated_bytes().unwrap();
        assert!(!prepare_input(&mut small, &lanes, &selected));
        assert_eq!(small.as_input().len(), 0);
        assert_eq!(small.allocated_bytes().unwrap(), bytes);
        assert!(prepare_input(&mut small, &lanes, &[]));
        assert_eq!(small.as_input().len(), 0);
    }
}
