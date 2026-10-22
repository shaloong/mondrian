//! One pre-view controller projection from exact canonical constant Some lanes.
//! This is not processor input, a state restore, an author edit or a value cache.
use super::*;
use vst3_host::controller_parameters::ControllerParameterQueryFrame;

impl Vst3Worker {
    /// False is a no-setter refusal. Once mutation starts every failure is an
    /// error, so callers cannot reuse an object under a fictitious no-op result.
    pub(super) fn project_idle_controller_values(
        &mut self,
        request: &crate::NativeAudioEditorOpenRequest,
    ) -> Result<bool, AudioProcessorHostError> {
        let values = request.controller_values();
        if values.is_empty() {
            return Ok(true);
        }
        request.owned_layout()?;
        if self.native_author_delivery.is_some()
            || self.prepared_author_entry.is_some()
            || self.configuration_phase.blocked_request().is_some()
            || self.configuration_phase.ready_token().is_some()
        {
            return Ok(false);
        }
        // Validate the entire selection before any native setter; native ID zero is a
        // normal admitted ID. Stepped values use the original author domain.
        for (index, (lane, bits)) in values.iter().enumerate() {
            if values[..index].iter().any(|(previous, _)| previous == lane) {
                return Err(invalid("duplicate native editor controller projection"));
            }
            projected_parameter(
                &self.payload.parameters,
                &self.payload.parameter_ids,
                *lane,
                *bits,
            )?;
        }
        if !self.author_binding_current(request.binding())? {
            return Ok(false);
        }
        let plugin = self
            .plugin
            .as_mut()
            .ok_or_else(|| unavailable("native controller owner closed"))?;
        let source = plugin.editor_event_source_status().map_err(native_editor_removal_failed)?;
        let cohort = plugin
            .pending_restart_cohort()
            .ok_or_else(|| unavailable("native controller cohort unavailable"))?;
        if !native_author::status(source)?.closed_empty() || cohort.pending() {
            return Ok(false);
        }
        // Reuse the original complete processing-metadata visitor/fixed frame.
        // A legal live caption remains presentation; all processing fields and
        // current normalized values retain the existing strict admission.
        let mut frame = ControllerParameterQueryFrame::default();
        initial_parameter_values::visit_admitted_controller_values(
            plugin,
            &self.payload.parameters,
            &mut frame,
            &mut |_, _| Ok(()),
        )
        .map_err(native_editor_removal_failed)?;
        if plugin.editor_event_source_status().map_err(native_editor_removal_failed)? != source
            || plugin.pending_restart_cohort() != Some(cohort)
        {
            return Ok(false);
        }
        let mut editor = plugin.borrow_editor_control().map_err(native_editor_removal_failed)?;
        let owner = editor.attachment_owner().map_err(native_editor_removal_failed)?;
        if owner.serial() != request.binding().expected_object.object_serial() {
            return Ok(false);
        }
        for (lane, bits) in values {
            let (native_id, normalized, host_editing) = projected_parameter(
                &self.payload.parameters,
                &self.payload.parameter_ids,
                *lane,
                *bits,
            )?;
            editor
                .project_controller_parameter(native_id, normalized, host_editing)
                .map_err(native_editor_removal_failed)?;
            if editor.attachment_owner().map_err(native_editor_removal_failed)? != owner
                || editor.editor_event_source_status().map_err(native_editor_removal_failed)?
                    != source
                || editor.pending_restart_cohort().map_err(native_editor_removal_failed)?
                    != Some(cohort)
            {
                return Err(invalid(
                    "native controller source changed during constant projection",
                ));
            }
        }
        drop(editor);
        if !self.author_binding_current(request.binding())? {
            return Err(invalid(
                "native controller owner changed during constant projection",
            ));
        }
        // View creation may legitimately append initialization callbacks later.
        // Only this completed synchronization phase is required to be unchanged.
        Ok(true)
    }
}

/// The preflight and setter pass consume one admitted schema, with no batch
/// copy. Static nonautomatable lanes remain legal; hidden lanes omit the
/// optional HostEditing bracket as required by that interface's contract.
fn projected_parameter(
    parameters: &[Vst3ParameterDescriptor],
    admitted: &[u32],
    lane: u32,
    author_bits: u64,
) -> Result<(u32, f64, bool), AudioProcessorHostError> {
    let index = usize::try_from(lane)
        .map_err(|_| invalid("native editor processing lane exceeds address space"))?;
    let id = *admitted
        .get(index)
        .ok_or_else(|| invalid("native editor controller projection lane is not admitted"))?;
    let descriptor = parameters
        .iter()
        .find(|parameter| parameter.id == id)
        .ok_or_else(|| invalid("native editor controller projection ID is absent"))?;
    if descriptor.read_only {
        return Err(invalid(
            "native editor controller projection is not writable",
        ));
    }
    Ok((
        id,
        descriptor.normalized_from_author_scalar(f64::from_bits(author_bits))?,
        !descriptor.can_automate && descriptor.flags & (1 << 4) == 0,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn projection_selection_respects_admitted_lane_author_domain_and_conditional_host_editing() {
        let mut parameter = Vst3ParameterDescriptor {
            id: 0,
            name: "Gain".to_owned(),
            default_normalized: 0.5,
            step_count: 0,
            can_automate: true,
            read_only: false,
            flags: 1,
        };
        let mut other = parameter.clone();
        other.id = 900;
        let mut third = parameter.clone();
        third.id = 77;
        // Neither the complete native census nor native IDs define processing
        // lane order. Native ID zero is legal at a nonzero admitted lane.
        let parameters = [parameter.clone(), third, other];
        let admitted = [900, 77, 0];
        let (id, value, bracket) =
            projected_parameter(&parameters, &admitted, 2, (-0.0_f64).to_bits()).unwrap();
        assert_eq!(id, 0);
        assert_eq!(value.to_bits(), (-0.0_f64).to_bits());
        assert!(!bracket);
        assert_eq!(
            projected_parameter(&parameters, &admitted, 0, 0.25_f64.to_bits()).unwrap(),
            (900, 0.25, false)
        );
        assert_eq!(
            projected_parameter(&parameters, &admitted, 1, 0.25_f64.to_bits()).unwrap(),
            (77, 0.25, false)
        );
        assert!(projected_parameter(&parameters, &admitted, 3, 0.25_f64.to_bits()).is_err());
        assert!(
            projected_parameter(&parameters, &admitted, 900, 0.25_f64.to_bits()).is_err(),
            "native ID cannot stand in for a processing lane"
        );
        assert!(
            projected_parameter(&parameters, &[42], 0, 0.25_f64.to_bits()).is_err(),
            "mapped native descriptor must exist"
        );
        for value in [f64::NAN, f64::INFINITY, -0.01, 1.01] {
            assert!(projected_parameter(&[parameter.clone()], &[0], 0, value.to_bits()).is_err());
        }
        parameter.can_automate = false;
        parameter.flags = 0;
        assert_eq!(
            projected_parameter(&[parameter.clone()], &[0], 0, 0.25_f64.to_bits()).unwrap(),
            (0, 0.25, true)
        );
        parameter.flags = 1 << 4;
        assert_eq!(
            projected_parameter(&[parameter.clone()], &[0], 0, 0.25_f64.to_bits()).unwrap(),
            (0, 0.25, false)
        );
        parameter.step_count = 4;
        assert_eq!(
            projected_parameter(&[parameter.clone()], &[0], 0, 3.0_f64.to_bits()).unwrap(),
            (0, 0.75, false)
        );
        assert!(projected_parameter(&[parameter.clone()], &[0], 0, 0.5_f64.to_bits()).is_err());
        parameter.read_only = true;
        assert!(projected_parameter(&[parameter], &[0], 0, 3.0_f64.to_bits()).is_err());
    }
}
