//! Immutable terminal Host math only; no native parameter/source permission.

use super::*;

fn refused() -> AudioCompileError {
    AudioCompileError::InvalidPreparedGraph(
        "live Host handoff requires only one static terminal Output fader change".to_owned(),
    )
}

fn compatible(old: &CompiledAudioProgram, next: &CompiledAudioProgram) -> bool {
    // Borrow the entire canonical closure, excluding only the admitted scalar.
    // Notes are deliberately excluded until their live origin migration is qualified.
    old.output.fader_db.is_finite()
        && next.output.fader_db.is_finite()
        && old.output.fader_db.to_bits() != next.output.fader_db.to_bits()
        && old.output.fader_automation.is_none()
        && next.output.fader_automation.is_none()
        && old.output.post_fader.processors.is_empty()
        && next.output.post_fader.processors.is_empty()
        && old.note_lanes.is_empty()
        && next.note_lanes.is_empty()
        && old.output.input_trim_db.to_bits() == next.output.input_trim_db.to_bits()
        && old.output.pre_fader == next.output.pre_fader
        && old.output.post_fader == next.output.post_fader
        && old.musical_time_reference == next.musical_time_reference
        && old.output_id == next.output_id
        && old.contributions == next.contributions
        && old.processing_scopes == next.processing_scopes
        && old.transitions == next.transitions
        && old.track_channels == next.track_channels
        && old.buses == next.buses
        && old.bus_order == next.bus_order
        && old.routes == next.routes
        && old.sidechain_routes == next.sidechain_routes
}

impl PreparedAudioPlan {
    pub(crate) fn requested_host_output_fader_logical_bytes(
        &self,
        next: &CompiledAudioProgram,
    ) -> Result<usize, AudioCompileError> {
        if !compatible(&self.program, next) {
            return Err(refused());
        }
        // No selected Note payload is admitted. The cloned schedule keeps all
        // existing processor and route shapes; this is logical policy only.
        Self::requested_base_logical_bytes(next)?
            .checked_add(self.schedule.notes.storage_bytes().map_err(|_| refused())?)
            .ok_or_else(refused)
    }

    pub(crate) fn prepare_host_output_fader(
        &self,
        next_program: Arc<CompiledAudioProgram>,
    ) -> Result<Self, AudioCompileError> {
        if !compatible(&self.program, &next_program) {
            return Err(refused());
        }
        let mut schedule = self.schedule.clone();
        let mut outputs = 0;
        for node in &mut schedule.nodes {
            if let PreparedNodeOrigin::Output(id) = node.origin {
                if id != next_program.output_id {
                    return Err(refused());
                }
                node.strip = next_program.output.clone();
                outputs += 1;
            }
        }
        if outputs != 1 {
            return Err(refused());
        }
        Ok(Self {
            preparation_id: mondrian_core::AudioPreparedPlanId::new(),
            program: next_program,
            contract: self.contract,
            kernel_backend: self.kernel_backend,
            schedule,
        })
    }
}
