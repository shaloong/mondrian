//! Same-contract native parameter preparation over the sole compiled Program.
//! This module prepares immutable data only. It does not install a Plan, call
//! DSP, restore native state, issue an owner lease or acknowledge a callback.

use super::*;
use crate::plan::CompiledProcessor;
use mondrian_core::{
    NativeAudioEditorInsertion as Insertion, NativeAudioEditorLocator,
    NativeAudioEditorOccurrenceOwner as Owner,
};

fn refused(reason: &'static str) -> AudioCompileError {
    AudioCompileError::InvalidPreparedGraph(reason.to_owned())
}

fn strip_target(locator: NativeAudioEditorLocator, owner: Owner, insertion: Insertion) -> bool {
    locator.owner == owner && locator.insertion == insertion
}

fn compatible_rack(
    old: &CompiledRack,
    next: &CompiledRack,
    allow_target: bool,
    locator: NativeAudioEditorLocator,
    changed: &mut usize,
) -> Result<(), AudioCompileError> {
    if old.processors.len() != next.processors.len() {
        return Err(refused("native author handoff changed processor topology"));
    }
    for (old, next) in old.processors.iter().zip(&next.processors) {
        if allow_target && old.instance_id == locator.instance {
            if old.instance_id != next.instance_id
                || old.definition != next.definition
                || old.parameters.len() != next.parameters.len()
                || old.parameters.iter().zip(&next.parameters).any(
                    |((old_id, old), (next_id, next))| {
                        old_id != next_id || old.schema != next.schema || old.ui != next.ui
                    },
                )
            {
                return Err(refused(
                    "native author handoff changed definition or parameter schema",
                ));
            }
            *changed = (*changed)
                .checked_add(1)
                .ok_or_else(|| refused("native author handoff occurrence extent overflow"))?;
        } else if old != next {
            return Err(refused(
                "native author handoff changed an unrelated processor",
            ));
        }
    }
    Ok(())
}

fn compatible_strip(
    old: &CompiledChannelStrip,
    next: &CompiledChannelStrip,
    owner: Owner,
    locator: NativeAudioEditorLocator,
    changed: &mut usize,
) -> Result<(), AudioCompileError> {
    if old.input_trim_db.to_bits() != next.input_trim_db.to_bits()
        || old.fader_db.to_bits() != next.fader_db.to_bits()
        || old.fader_automation != next.fader_automation
    {
        return Err(refused("native author handoff changed channel mathematics"));
    }
    compatible_rack(
        &old.pre_fader,
        &next.pre_fader,
        strip_target(locator, owner, Insertion::PreFader),
        locator,
        changed,
    )?;
    compatible_rack(
        &old.post_fader,
        &next.post_fader,
        strip_target(locator, owner, Insertion::PostFader),
        locator,
        changed,
    )
}

fn compatible_program(
    old: &CompiledAudioProgram,
    next: &CompiledAudioProgram,
    locator: NativeAudioEditorLocator,
) -> Result<(), AudioCompileError> {
    if old.output_id != next.output_id
        || old.contributions != next.contributions
        || old.transitions != next.transitions
        || old.routes != next.routes
        || old.sidechain_routes != next.sidechain_routes
        || old.bus_order != next.bus_order
        || old.processing_scopes.keys().ne(next.processing_scopes.keys())
        || old.track_channels.keys().ne(next.track_channels.keys())
        || old.buses.keys().ne(next.buses.keys())
    {
        return Err(refused(
            "native author handoff changed signal topology or source/time mapping",
        ));
    }
    if let Owner::Contribution { .. } = locator.owner
        && locator.insertion != Insertion::Scope
    {
        return Err(refused("native author handoff Scope insertion is invalid"));
    }
    // The exact selected occurrence was admitted against its actual retained
    // Hosted receipt. Other public-output materializations share the canonical
    // Scope, not that selected Clip's edit ID. Their whole Programs must still
    // pass unchanged topology/time-map/schema eligibility below.
    let mut changed = 0;
    for (id, old) in &old.processing_scopes {
        let next = &next.processing_scopes[id];
        if old.id != next.id
            || old.input_gain_db.to_bits() != next.input_gain_db.to_bits()
            || old.input_gain_automation != next.input_gain_automation
        {
            return Err(refused("native author handoff changed Scope mathematics"));
        }
        let allow = matches!(locator.owner, Owner::Contribution { scope_id, .. }
            if scope_id == *id)
            && locator.insertion == Insertion::Scope;
        compatible_rack(&old.rack, &next.rack, allow, locator, &mut changed)?;
    }
    for (id, old) in &old.track_channels {
        let next = &next.track_channels[id];
        if old.muted != next.muted {
            return Err(refused("native author handoff changed a Track mute gate"));
        }
        compatible_strip(
            &old.strip,
            &next.strip,
            Owner::Track(*id),
            locator,
            &mut changed,
        )?;
    }
    for (id, old) in &old.buses {
        compatible_strip(old, &next.buses[id], Owner::Bus(*id), locator, &mut changed)?;
    }
    compatible_strip(
        &old.output,
        &next.output,
        Owner::Output(old.output_id),
        locator,
        &mut changed,
    )?;
    if changed != 1 {
        return Err(refused(
            "native author handoff requires one exact canonical processor",
        ));
    }
    Ok(())
}

pub(crate) fn processor(
    program: &CompiledAudioProgram,
    occurrence: AudioProcessorOccurrence,
) -> Result<&CompiledProcessor, AudioCompileError> {
    let rack = match (occurrence.owner, occurrence.insertion) {
        (
            AudioProcessorOccurrenceOwner::Contribution { scope_id, .. },
            AudioProcessorInsertionPoint::Scope,
        ) => {
            &program
                .processing_scopes
                .get(&scope_id)
                .ok_or_else(|| refused("native parameter Scope is absent"))?
                .rack
        }
        (owner, insertion) => {
            let strip = match owner {
                AudioProcessorOccurrenceOwner::Track(id) => {
                    &program
                        .track_channels
                        .get(&id)
                        .ok_or_else(|| refused("native parameter Track is absent"))?
                        .strip
                }
                AudioProcessorOccurrenceOwner::Bus(id) => program
                    .buses
                    .get(&id)
                    .ok_or_else(|| refused("native parameter Bus is absent"))?,
                AudioProcessorOccurrenceOwner::Output(id) if id == program.output_id => {
                    &program.output
                }
                _ => return Err(refused("native parameter strip address is invalid")),
            };
            match insertion {
                AudioProcessorInsertionPoint::PreFader => &strip.pre_fader,
                AudioProcessorInsertionPoint::PostFader => &strip.post_fader,
                AudioProcessorInsertionPoint::Scope => {
                    return Err(refused("native parameter Scope insertion is invalid"))
                }
            }
        }
    };
    rack.processors
        .iter()
        .find(|processor| processor.instance_id == occurrence.instance_id)
        .ok_or_else(|| refused("native parameter processor is absent"))
}

impl PreparedAudioPlan {
    /// Prepare a new immutable Plan for one canonical native author target.
    /// Every actual materialization of its shared Scope uses the same canonical
    /// curves/state. The caller must admit current/new/retired ownership overlap
    /// and install this Plan at the sole Runtime/Media linearization before Ack.
    pub(crate) fn prepare_native_author_parameters(
        &self,
        next_program: Arc<CompiledAudioProgram>,
        locator: NativeAudioEditorLocator,
    ) -> Result<Self, AudioCompileError> {
        compatible_program(&self.program, &next_program, locator)?;
        let sample_rate = AudioSampleRate::new(self.contract.sample_rate)
            .map_err(|_| refused("native author handoff sample rate is invalid"))?;
        let mut schedule = self.schedule.clone();
        for prepared in &mut schedule.processors {
            let old = processor(&self.program, prepared.occurrence)?;
            let next = processor(&next_program, prepared.occurrence)?;
            if old == next {
                continue;
            }
            if prepared.parameter_ids.len() != prepared.parameter_curves.len()
                || prepared.parameter_ids.iter().ne(next.parameters.keys())
            {
                return Err(refused(
                    "native author handoff changed prepared parameter lane identity",
                ));
            }
            let owner_time_offset = prepared.owner_time_offset;
            for ((id, curve), next_parameter) in prepared
                .parameter_ids
                .iter()
                .zip(&mut prepared.parameter_curves)
                .zip(next.parameters.values())
            {
                if id != &next_parameter.schema.parameter_id
                    || next_parameter
                        .automation
                        .as_ref()
                        .is_some_and(|curve| id != &curve.parameter_id)
                {
                    return Err(refused(
                        "native author handoff parameter identity is invalid",
                    ));
                }
                // Reuse the exact occurrence's original owner-time offset and
                // the one complete canonical curve lowering implementation.
                *curve = next_parameter
                    .automation
                    .as_ref()
                    .map(|source| {
                        PreparedAutomationCurve::build(source, owner_time_offset, sample_rate)
                    })
                    .transpose()?;
            }
            if old.opaque_state != next.opaque_state {
                let bytes = next.opaque_state.as_deref()
                    .ok_or_else(|| refused("native author handoff cannot erase the factory seed without a default-state capability"))?;
                let format = match &next.definition {
                    mondrian_timeline::audio::AudioProcessorDefinitionRef::Vst3 { .. } => {
                        crate::NativeConfigurationFormat::Vst3ComponentAndController
                    }
                    mondrian_timeline::audio::AudioProcessorDefinitionRef::Clap { .. } => {
                        crate::NativeConfigurationFormat::ClapState
                    }
                    _ => {
                        return Err(refused(
                            "native author handoff requires an actual native definition",
                        ))
                    }
                };
                prepared.factory = prepared
                    .factory
                    .with_native_author_seed(format, bytes)
                    .map_err(|error| AudioCompileError::InvalidPreparedGraph(error.to_string()))?;
            }
        }
        for node in &mut schedule.nodes {
            node.strip = match node.origin {
                PreparedNodeOrigin::Track(id) => next_program.track_channels[&id].strip.clone(),
                PreparedNodeOrigin::Bus(id) => next_program.buses[&id].clone(),
                PreparedNodeOrigin::Output(_) => next_program.output.clone(),
            };
        }
        for scope in &mut schedule.scopes {
            *scope = next_program.processing_scopes[&scope.id].clone();
        }
        let automation = automation_summary(
            &schedule.nodes,
            &schedule.contributions,
            &schedule.routes,
            &schedule.sidechain_bindings,
            &schedule.processors,
            self.contract,
        )?;
        schedule.summary.automation_curve_count = automation.curve_count;
        schedule.summary.automation_event_span_count = automation.span_count;
        schedule.summary.processor_parameter_lane_count = automation.parameter_lane_count;
        schedule.summary.maximum_parameter_events_per_block = automation.maximum_parameter_events;
        Ok(Self {
            preparation_id: mondrian_core::AudioPreparedPlanId::new(),
            program: next_program,
            contract: self.contract,
            kernel_backend: self.kernel_backend,
            schedule,
        })
    }
}
