//! Same-contract native parameter preparation over the sole compiled Program.
//! This module prepares immutable data only. It does not install a Plan, call
//! DSP, restore native state, issue an owner lease or acknowledge a callback.

use super::*;
use crate::plan::{CompiledNoteLane, CompiledProcessor};
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

// Plain AudioNote PartialEq loses the distinction between -0.0 and +0.0.
// Cached Note IR must preserve the complete immutable score and velocity bits.
fn same_note_score(old: &[CompiledNoteLane], next: &[CompiledNoteLane]) -> bool {
    old.len() == next.len()
        && old.iter().zip(next).all(|(old, next)| {
            old.id == next.id
                && old.target == next.target
                && old.notes.len() == next.notes.len()
                && old.notes.iter().zip(&next.notes).all(|(old, next)| {
                    old.id == next.id
                        && old.start == next.start
                        && old.end == next.end
                        && old.channel == next.channel
                        && old.key == next.key
                        && old.on_velocity.to_bits() == next.on_velocity.to_bits()
                        && old.off_velocity.to_bits() == next.off_velocity.to_bits()
                })
        })
}

fn compatible_program(
    old: &CompiledAudioProgram,
    next: &CompiledAudioProgram,
    locator: NativeAudioEditorLocator,
) -> Result<(), AudioCompileError> {
    if !same_note_score(&old.note_lanes, &next.note_lanes) {
        return Err(refused(
            "native author handoff changed immutable Note score",
        ));
    }
    if old.musical_time_reference != next.musical_time_reference
        || old.output_id != next.output_id
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
    /// Borrow the compatible canonical Program before cloning any prepared data.
    /// Cached immutable Note storage is carried by the exact bitwise score gate;
    /// this is the existing logical policy, not a physical allocation bound.
    pub(crate) fn requested_native_author_logical_bytes(
        &self,
        next_program: &CompiledAudioProgram,
        locator: NativeAudioEditorLocator,
    ) -> Result<usize, AudioCompileError> {
        compatible_program(self.program.as_ref(), next_program, locator)?;
        let base = Self::requested_base_logical_bytes(next_program)?;
        let notes = self
            .schedule
            .notes
            .storage_bytes()
            .map_err(|_| refused("native author cached Note storage extent overflow"))?;
        let prepared = base
            .checked_add(notes)
            .ok_or_else(|| refused("native author prepared logical extent overflow"))?;
        let compiled_notes = next_program
            .compiled_note_payload_bytes()
            .map_err(|_| refused("native author compiled Note storage extent overflow"))?;
        prepared
            .checked_add(compiled_notes)
            .ok_or_else(|| refused("native author prepared logical extent overflow"))
    }

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
        let note_clone_upper = self
            .schedule
            .notes
            .storage_bytes()
            .map_err(|_| refused("native author cached Note storage extent overflow"))?;
        let mut schedule = self.schedule.clone();
        // Late logical acceptance of the actual aggregate clone, before any
        // processor/factory interaction; this is not preallocation admission.
        let cloned_notes = schedule
            .notes
            .storage_bytes()
            .map_err(|_| refused("native author cloned Note storage extent overflow"))?;
        if cloned_notes > note_clone_upper {
            return Err(refused(
                "native author cloned Note storage exceeds its logical bound",
            ));
        }
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

#[cfg(test)]
mod note_handoff_tests {
    use super::*;
    use crate::plan::CompiledTrackChannel;
    use mondrian_core::{
        AudioNoteId, AudioNoteLaneId, AudioProcessorInstanceId, ProgramOutputId, SequenceId,
        TrackId,
    };
    use mondrian_timeline::audio::{AudioNote, AudioNoteTarget, AudioProcessorDefinitionRef};
    use std::collections::BTreeMap;

    fn score() -> Vec<CompiledNoteLane> {
        vec![CompiledNoteLane {
            id: AudioNoteLaneId::new(),
            target: AudioNoteTarget {
                track_id: TrackId::new(),
                processor_id: AudioProcessorInstanceId::new(),
                input_port_key: " exact.port ".to_owned(),
            },
            notes: vec![AudioNote {
                id: AudioNoteId::new(),
                start: TimelineTime::ZERO,
                end: TimelineTime::new(1, 1).unwrap(),
                channel: 4,
                key: 60,
                on_velocity: -0.0,
                off_velocity: f64::from_bits(0x3fd3_3333_3333_3335),
            }],
        }]
    }

    #[test]
    fn unchanged_score_retains_exact_velocity_bits_including_signed_zero() {
        let original = score();
        assert!(same_note_score(&original, &original.clone()));
        let mut changed = original.clone();
        changed[0].notes[0].on_velocity = 0.0;
        assert_eq!(changed, original); // Ordinary f64 PartialEq loses this bit.
        assert!(!same_note_score(&original, &changed));
        let mut original = original;
        // Comparator-only malformed author fixture: this checks bit equality,
        // not permission for a nonfinite velocity to enter a validated score.
        original[0].notes[0].off_velocity = f64::from_bits(0x7ff8_0000_0000_0001);
        let mut changed = original.clone();
        assert!(same_note_score(&original, &changed));
        changed[0].notes[0].off_velocity = f64::from_bits(0x7ff8_0000_0000_0002);
        assert!(!same_note_score(&original, &changed));
    }

    #[test]
    fn every_note_identity_target_endpoint_and_scalar_change_refuses_reuse() {
        let original = score();
        let mutations: &[fn(&mut [CompiledNoteLane])] = &[
            |value| value[0].id = AudioNoteLaneId::new(),
            |value| value[0].target.track_id = TrackId::new(),
            |value| value[0].target.processor_id = AudioProcessorInstanceId::new(),
            |value| value[0].target.input_port_key = "exact.port".to_owned(),
            |value| value[0].notes[0].id = AudioNoteId::new(),
            |value| value[0].notes[0].start = TimelineTime::new(1, 2).unwrap(),
            |value| value[0].notes[0].end = TimelineTime::new(2, 1).unwrap(),
            |value| value[0].notes[0].channel = 5,
            |value| value[0].notes[0].key = 61,
            |value| value[0].notes[0].on_velocity = 0.5,
            |value| value[0].notes[0].off_velocity = f64::from_bits(0x3fd3_3333_3333_3334),
        ];
        for mutate in mutations {
            let mut changed = original.clone();
            mutate(&mut changed);
            assert!(!same_note_score(&original, &changed));
        }
    }

    #[test]
    fn note_and_lane_extents_and_order_are_part_of_the_cached_ir_guard() {
        let mut original = score();
        let mut second_note = original[0].notes[0].clone();
        second_note.id = AudioNoteId::new();
        original[0].notes.push(second_note);
        let mut second_lane = original[0].clone();
        second_lane.id = AudioNoteLaneId::new();
        original.push(second_lane);
        let mut changed = original.clone();
        changed.reverse();
        assert!(!same_note_score(&original, &changed));
        changed = original.clone();
        changed[0].notes.reverse();
        assert!(!same_note_score(&original, &changed));
        changed = original.clone();
        changed.pop();
        assert!(!same_note_score(&original, &changed));
        changed = original.clone();
        changed[0].notes.pop();
        assert!(!same_note_score(&original, &changed));
        assert!(!same_note_score(&[], &original));
        assert!(same_note_score(&[], &[]));
    }

    #[test]
    fn actual_compatible_program_checks_score_before_any_schedule_clone() {
        let lanes = score();
        let target = lanes[0].target.clone();
        let strip = CompiledChannelStrip {
            input_trim_db: 0.0,
            pre_fader: CompiledRack::default(),
            fader_db: 0.0,
            fader_automation: None,
            post_fader: CompiledRack::default(),
        };
        let mut track = CompiledTrackChannel { muted: false, strip: strip.clone() };
        track.strip.pre_fader.processors.push(CompiledProcessor {
            instance_id: target.processor_id,
            definition: AudioProcessorDefinitionRef::Clap {
                plugin_id: "owned.handoff.metadata".to_owned(),
                schema_version: 1,
                binary_sha256: Some([1; 32]),
            },
            parameters: BTreeMap::new(),
            opaque_state: None,
        });
        let program = CompiledAudioProgram {
            musical_time_reference: None,
            output_id: ProgramOutputId::new(),
            contributions: Vec::new(),
            processing_scopes: BTreeMap::new(),
            transitions: Vec::new(),
            track_channels: BTreeMap::from([(target.track_id, track)]),
            buses: BTreeMap::new(),
            bus_order: Vec::new(),
            output: strip,
            routes: Vec::new(),
            sidechain_routes: Vec::new(),
            note_lanes: lanes,
        };
        let locator = NativeAudioEditorLocator {
            sequence: SequenceId::new(),
            instance: target.processor_id,
            owner: Owner::Track(target.track_id),
            insertion: Insertion::PreFader,
        };
        assert!(compatible_program(&program, &program.clone(), locator).is_ok());
        // The same immutable signal identity includes the exact reference origin,
        // even though this CLAP slice consumes only its constant tempo.
        let mut with_music = program.clone();
        with_music.musical_time_reference = Some(
            mondrian_core::MusicalTimeReference::new(
                TimelineTime::new(-1, 7).unwrap(),
                TimelineTime::new(3, 4).unwrap(),
            )
            .unwrap(),
        );
        for next in [program.clone(), {
            let mut next = with_music.clone();
            next.musical_time_reference = Some(
                mondrian_core::MusicalTimeReference::new(
                    TimelineTime::new(2, 7).unwrap(),
                    TimelineTime::new(3, 4).unwrap(),
                )
                .unwrap(),
            );
            next
        }] {
            assert_eq!(
                compatible_program(&with_music, &next, locator),
                Err(refused(
                    "native author handoff changed signal topology or source/time mapping"
                ))
            );
        }

        let mut changed = program.clone();
        changed.note_lanes[0].notes[0].on_velocity = 0.0;
        assert_eq!(
            compatible_program(&program, &changed, locator),
            Err(refused(
                "native author handoff changed immutable Note score"
            ))
        );
        assert_eq!(
            program.note_lanes[0].notes[0].on_velocity.to_bits(),
            (-0.0_f64).to_bits()
        );
    }
}
