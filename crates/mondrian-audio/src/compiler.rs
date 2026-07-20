use crate::plan::{
    AudioCompileRequest, CompiledAudioContribution, CompiledAudioProgram, CompiledAudioSource,
    CompiledChannelStrip, CompiledProcessingScope, CompiledProcessor, CompiledRack, CompiledRoute,
    CompiledSourceTimeMap, CompiledTrackChannel, CompiledTransition,
};
use mondrian_core::{AudioProcessingScopeId, MixBusId, ProgramOutputId, TrackId};
use mondrian_timeline::audio::{
    AudioChannelStrip, AudioComponentSource, AudioProcessorRack, AudioProgramOutput, AudioRoute,
    AudioRouteDestination, AudioRouteSource, ProgramOutputMainSource,
};
use mondrian_timeline::{AudioAuthoringError, Sequence};
use std::collections::{BTreeMap, BTreeSet};

/// Compile one public output from the validated Track/Clip placement authority.
pub fn compile_audio_program(
    sequence: &Sequence,
    request: AudioCompileRequest,
) -> Result<CompiledAudioProgram, AudioCompileError> {
    sequence.audio_program.validate(
        &sequence.audio_tracks,
        &sequence.audio_roles,
        sequence.settings.audio_channel_layout,
    )?;
    let output = sequence
        .audio_program
        .outputs
        .iter()
        .find(|output| output.id == request.output_id)
        .ok_or(AudioCompileError::OutputNotFound(request.output_id))?;
    if !matches!(output.main_source, ProgramOutputMainSource::RoutedInputs) {
        return Err(AudioCompileError::SemanticProjectionNotExecutableYet);
    }

    let (mut routes, mut required_tracks, required_buses) =
        resolve_signal_closure(&sequence.audio_program.routes, request.output_id);
    if !request.audition.soloed_tracks.is_empty() {
        required_tracks.retain(|track| request.audition.soloed_tracks.contains(track));
        routes.retain(|route| match route.source {
            AudioRouteSource::Track { track_id, .. } => required_tracks.contains(&track_id),
            AudioRouteSource::Bus { .. } => true,
        });
    }

    let mut track_channels = BTreeMap::new();
    for track_id in &required_tracks {
        let track = sequence
            .audio_tracks
            .iter()
            .find(|track| track.id == *track_id)
            .ok_or(AudioCompileError::MissingTrackChannel(*track_id))?;
        let channel = sequence
            .audio_program
            .track_channels
            .get(track_id)
            .ok_or(AudioCompileError::MissingTrackChannel(*track_id))?;
        track_channels.insert(
            *track_id,
            CompiledTrackChannel {
                muted: track.is_muted,
                strip: compile_strip(&channel.strip)?,
            },
        );
    }

    let mut buses = BTreeMap::new();
    for bus in &sequence.audio_program.buses {
        if required_buses.contains(&bus.id) {
            buses.insert(bus.id, compile_strip(&bus.strip)?);
        }
    }
    let bus_order = topological_bus_order(&routes, &required_buses)?;

    let mut processing_scopes = BTreeMap::<AudioProcessingScopeId, CompiledProcessingScope>::new();
    let mut contributions = Vec::new();
    for track in sequence.audio_tracks.iter().filter(|track| required_tracks.contains(&track.id)) {
        for clip in &track.clips {
            if clip.is_disabled {
                continue;
            }
            let sequence_range =
                mondrian_core::TimelineTimeRange::new(clip.position, clip.duration)
                    .map_err(|error| AudioCompileError::InvalidPlacement(error.to_string()))?;
            for edit in clip.audio_components.iter().filter(|edit| edit.enabled) {
                let author_scope = sequence
                    .audio_program
                    .processing_scopes
                    .iter()
                    .find(|scope| scope.id == edit.processing.scope_id)
                    .ok_or(AudioCompileError::MissingProcessingScope(
                        edit.processing.scope_id,
                    ))?;
                if let std::collections::btree_map::Entry::Vacant(entry) =
                    processing_scopes.entry(author_scope.id)
                {
                    entry.insert(compile_scope(author_scope)?);
                }
                let source = match edit.source {
                    AudioComponentSource::Media { component_id } => {
                        CompiledAudioSource::Media { asset_id: clip.asset_id, component_id }
                    }
                    AudioComponentSource::NestedOutput { output_id } => {
                        CompiledAudioSource::NestedOutput {
                            sequence_id: clip.nested_sequence_id.ok_or_else(|| {
                                AudioCompileError::InvalidPlacement(format!(
                                    "nested audio Clip {} has no Sequence source",
                                    clip.id
                                ))
                            })?,
                            output_id,
                        }
                    }
                };
                contributions.push(CompiledAudioContribution {
                    edit_id: edit.id,
                    clip_id: clip.id,
                    track_id: track.id,
                    sequence_range,
                    source_time_map: CompiledSourceTimeMap {
                        sequence_start: clip.position,
                        source_in: clip.source_in,
                        speed: clip.speed,
                    },
                    source,
                    channel_mapping: edit.channel_mapping.clone(),
                    processing_scope: edit.processing.scope_id,
                    scope_in: edit.processing.scope_in,
                    local_time_in: edit.local_time_in,
                    volume_db: edit.volume_db,
                    volume_automation: edit.volume_automation.clone(),
                    pan: edit.pan,
                    pan_automation: edit.pan_automation.clone(),
                    fade_in: edit.fades.fade_in.map(|fade| (fade.duration, fade.curve)),
                    fade_out: edit.fades.fade_out.map(|fade| (fade.duration, fade.curve)),
                });
            }
        }
    }
    contributions.sort_by_key(|contribution| contribution.edit_id);
    let included = contributions
        .iter()
        .map(|contribution| contribution.edit_id)
        .collect::<BTreeSet<_>>();
    let transitions = sequence
        .audio_program
        .transitions
        .iter()
        .filter(|transition| {
            included.contains(&transition.left) && included.contains(&transition.right)
        })
        .map(|transition| CompiledTransition {
            left: transition.left,
            right: transition.right,
            sequence_range: transition.sequence_range,
            curve: transition.curve,
        })
        .collect();

    Ok(CompiledAudioProgram {
        output_id: request.output_id,
        contributions,
        processing_scopes,
        transitions,
        track_channels,
        buses,
        bus_order,
        output: compile_output(output)?,
        routes,
    })
}

fn compile_scope(
    scope: &mondrian_timeline::audio::AudioProcessingScope,
) -> Result<CompiledProcessingScope, AudioCompileError> {
    Ok(CompiledProcessingScope {
        id: scope.id,
        input_gain_db: scope.input_gain_db,
        input_gain_automation: scope.input_gain_automation.clone(),
        rack: compile_rack(&scope.processors)?,
    })
}

fn compile_output(output: &AudioProgramOutput) -> Result<CompiledChannelStrip, AudioCompileError> {
    compile_strip(&output.strip)
}

fn compile_strip(strip: &AudioChannelStrip) -> Result<CompiledChannelStrip, AudioCompileError> {
    Ok(CompiledChannelStrip {
        input_trim_db: strip.input_trim_db,
        pre_fader: compile_rack(&strip.pre_fader)?,
        fader_db: strip.fader_db,
        fader_automation: strip.fader_automation.clone(),
        post_fader: compile_rack(&strip.post_fader)?,
    })
}

fn compile_rack(rack: &AudioProcessorRack) -> Result<CompiledRack, AudioCompileError> {
    let processors = rack
        .processors
        .iter()
        .filter(|processor| !processor.bypassed)
        .map(|processor| CompiledProcessor {
            instance_id: processor.id,
            definition: processor.definition.clone(),
            parameters: processor.parameters.clone(),
            opaque_state: processor.opaque_state.clone(),
        })
        .collect();
    Ok(CompiledRack { processors })
}

fn resolve_signal_closure(
    routes: &[AudioRoute],
    output_id: ProgramOutputId,
) -> (Vec<CompiledRoute>, BTreeSet<TrackId>, BTreeSet<MixBusId>) {
    let mut required_buses = BTreeSet::new();
    let mut required_tracks = BTreeSet::new();
    let mut selected = Vec::new();
    let mut pending_destinations = vec![AudioRouteDestination::Output(output_id)];
    while let Some(destination) = pending_destinations.pop() {
        for route in routes.iter().filter(|route| route.enabled && route.destination == destination)
        {
            if selected.iter().any(|selected: &CompiledRoute| selected.id == route.id) {
                continue;
            }
            selected.push(CompiledRoute {
                id: route.id,
                source: route.source,
                destination: route.destination,
                gain_db: route.gain_db,
                gain_automation: route.gain_automation.clone(),
            });
            match route.source {
                AudioRouteSource::Track { track_id, .. } => {
                    required_tracks.insert(track_id);
                }
                AudioRouteSource::Bus { bus_id, .. } => {
                    if required_buses.insert(bus_id) {
                        pending_destinations.push(AudioRouteDestination::Bus(bus_id));
                    }
                }
            }
        }
    }
    selected.sort_by_key(|route| route.id);
    (selected, required_tracks, required_buses)
}

fn topological_bus_order(
    routes: &[CompiledRoute],
    buses: &BTreeSet<MixBusId>,
) -> Result<Vec<MixBusId>, AudioCompileError> {
    let mut indegree = buses.iter().map(|id| (*id, 0_usize)).collect::<BTreeMap<_, _>>();
    for route in routes {
        if let (
            AudioRouteSource::Bus { bus_id: source, .. },
            AudioRouteDestination::Bus(destination),
        ) = (route.source, route.destination)
        {
            if buses.contains(&source) && buses.contains(&destination) {
                *indegree.entry(destination).or_default() += 1;
            }
        }
    }
    let mut ready = indegree
        .iter()
        .filter_map(|(id, degree)| (*degree == 0).then_some(*id))
        .collect::<BTreeSet<_>>();
    let mut order = Vec::new();
    while let Some(id) = ready.pop_first() {
        order.push(id);
        for route in routes {
            if matches!(route.source, AudioRouteSource::Bus { bus_id, .. } if bus_id == id) {
                if let AudioRouteDestination::Bus(destination) = route.destination {
                    if let Some(value) = indegree.get_mut(&destination) {
                        *value -= 1;
                        if *value == 0 {
                            ready.insert(destination);
                        }
                    }
                }
            }
        }
    }
    if order.len() != buses.len() {
        return Err(AudioCompileError::RouteCycle);
    }
    Ok(order)
}

/// Author validation, dependency, or lowering failure.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum AudioCompileError {
    /// Invalid persistent author state.
    #[error(transparent)]
    InvalidAuthoring(#[from] AudioAuthoringError),
    /// Requested public output is absent.
    #[error("audio Program Output {0} does not exist")]
    OutputNotFound(ProgramOutputId),
    /// Required Track mixer state is absent.
    #[error("audio Track mixer channel {0} is absent")]
    MissingTrackChannel(TrackId),
    /// Component references a missing processing definition.
    #[error("audio processing scope {0} is absent")]
    MissingProcessingScope(AudioProcessingScopeId),
    /// Clip placement or source mapping is invalid.
    #[error("invalid audio placement: {0}")]
    InvalidPlacement(String),
    /// Semantic projection authoring is preserved but not executable yet.
    #[error("semantic audio output projection is not executable yet")]
    SemanticProjectionNotExecutableYet,
    /// Instantaneous Route cycle reached compilation.
    #[error("audio routing contains a cycle")]
    RouteCycle,
    /// Render Contract contains zero or unbounded values.
    #[error("invalid audio Render Contract")]
    InvalidRenderContract,
    /// A processor definition could not be realized for the concrete Render Contract.
    #[error(transparent)]
    ProcessorPreparation(#[from] crate::AudioProcessorHostError),
    /// Semantic IR could not be lowered into a closed dense execution schedule.
    #[error("invalid prepared audio graph: {0}")]
    InvalidPreparedGraph(String),
}
