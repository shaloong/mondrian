use crate::plan::{
    AudioCompileRequest, CompiledAudioContribution, CompiledAudioProgram, CompiledAudioSource,
    CompiledChannelStrip, CompiledNoteLane, CompiledProcessingScope, CompiledProcessor,
    CompiledProcessorSidechainRoute, CompiledRack, CompiledRoute, CompiledSourceTimeMap,
    CompiledTrackChannel, CompiledTransition,
};
use mondrian_core::{
    AudioNoteId, AudioNoteLaneId, AudioProcessingScopeId, AudioProcessorInstanceId, MixBusId,
    ProgramOutputId, TrackId,
};
use mondrian_timeline::audio::{
    AudioChannelStrip, AudioChannelStripOutputPort, AudioComponentSource, AudioProcessorRack,
    AudioProcessorSidechainRoute, AudioProgramOutput, AudioRoute, AudioRouteDestination,
    AudioRouteSource, ProgramOutputMainSource,
};
use mondrian_timeline::{AudioAuthoringError, Sequence};
use std::collections::{BTreeMap, BTreeSet};

// Fresh compiler-owned Note payload acceptance; no Runtime grant or retained A.
fn compiled_note_array_bytes<T>(count: usize) -> Result<usize, AudioCompileError> {
    std::alloc::Layout::array::<T>(count)
        .map(|layout| layout.size())
        .map_err(|_| AudioCompileError::NoteEventCountOverflow)
}

fn compiled_note_payload_bytes(
    lanes: usize,
    note_bytes: usize,
    key_bytes: usize,
) -> Result<usize, AudioCompileError> {
    compiled_note_array_bytes::<CompiledNoteLane>(lanes)?
        .checked_add(note_bytes)
        .and_then(|bytes| bytes.checked_add(key_bytes))
        .ok_or(AudioCompileError::NoteEventCountOverflow)
}

fn requested_compiled_note_payload<'a>(
    mut lanes: impl Iterator<Item = &'a mondrian_timeline::audio::AudioNoteLane>,
) -> Result<(usize, usize), AudioCompileError> {
    let (count, note_bytes, key_bytes) =
        lanes.try_fold((0_usize, 0_usize, 0_usize), |(count, notes, keys), lane| {
            Ok::<_, AudioCompileError>((
                count.checked_add(1).ok_or(AudioCompileError::NoteEventCountOverflow)?,
                notes
                    .checked_add(compiled_note_array_bytes::<
                        mondrian_timeline::audio::AudioNote,
                    >(lane.notes.len())?)
                    .ok_or(AudioCompileError::NoteEventCountOverflow)?,
                keys.checked_add(lane.target.input_port_key.len())
                    .ok_or(AudioCompileError::NoteEventCountOverflow)?,
            ))
        })?;
    Ok((
        count,
        compiled_note_payload_bytes(count, note_bytes, key_bytes)?,
    ))
}

fn require_compiled_note_capacity(
    actual: usize,
    requested: usize,
) -> Result<(), AudioCompileError> {
    if actual > requested {
        return Err(AudioCompileError::InvalidPreparedGraph(
            "compiled Note storage capacity exceeds request".to_owned(),
        ));
    }
    Ok(())
}

fn reserve_compiled_note_rows<T>(count: usize) -> Result<Vec<T>, AudioCompileError> {
    compiled_note_array_bytes::<T>(count)?;
    let mut values = Vec::new();
    values.try_reserve_exact(count).map_err(|_| {
        AudioCompileError::InvalidPreparedGraph(
            "compiled Note storage allocation refused".to_owned(),
        )
    })?;
    require_compiled_note_capacity(values.capacity(), count)?;
    Ok(values)
}

fn require_compiled_note_payload(actual: usize, requested: usize) -> Result<(), AudioCompileError> {
    if actual > requested {
        return Err(AudioCompileError::InvalidPreparedGraph(
            "compiled Note payload exceeds request".to_owned(),
        ));
    }
    Ok(())
}

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

    let muted_tracks = sequence
        .audio_tracks
        .iter()
        .filter(|track| track.is_muted)
        .map(|track| track.id)
        .collect::<BTreeSet<_>>();
    let mut sidechain_source_buses = BTreeSet::new();
    let mut sidechain_source_tracks = BTreeSet::new();
    let mut selected_sidechain_ids = BTreeSet::new();
    let (mut routes, mut required_tracks, required_buses) = loop {
        let (routes, mut required_tracks, required_buses) = resolve_signal_closure(
            &sequence.audio_program.routes,
            request.output_id,
            &sidechain_source_buses,
            &muted_tracks,
        );
        required_tracks.extend(sidechain_source_tracks.iter().copied());
        let post_fader_tracks = post_fader_tracks(
            &routes,
            &sequence.audio_program.sidechain_routes,
            &selected_sidechain_ids,
        );
        let post_fader_buses = post_fader_buses(
            &routes,
            &sequence.audio_program.sidechain_routes,
            &selected_sidechain_ids,
        );
        let active_processors = active_author_processor_ids(
            sequence,
            output,
            &required_tracks,
            &required_buses,
            &post_fader_tracks,
            &post_fader_buses,
        );
        let selected = sequence
            .audio_program
            .sidechain_routes
            .iter()
            .filter(|route| {
                route.enabled
                    && active_processors.contains(&route.processor_id)
                    && !matches!(
                        route.source,
                        AudioRouteSource::Track {
                            track_id,
                            port: AudioChannelStripOutputPort::PostMute,
                        } if muted_tracks.contains(&track_id)
                    )
            })
            .map(|route| route.id)
            .collect::<BTreeSet<_>>();
        let next_source_buses = sequence
            .audio_program
            .sidechain_routes
            .iter()
            .filter(|route| selected.contains(&route.id))
            .filter_map(|route| match route.source {
                AudioRouteSource::Bus { bus_id, .. } => Some(bus_id),
                AudioRouteSource::Track { .. } => None,
            })
            .collect::<BTreeSet<_>>();
        let next_source_tracks = sequence
            .audio_program
            .sidechain_routes
            .iter()
            .filter(|route| selected.contains(&route.id))
            .filter_map(|route| match route.source {
                AudioRouteSource::Track { track_id, .. } => Some(track_id),
                AudioRouteSource::Bus { .. } => None,
            })
            .collect::<BTreeSet<_>>();
        if selected == selected_sidechain_ids
            && next_source_buses == sidechain_source_buses
            && next_source_tracks == sidechain_source_tracks
        {
            break (routes, required_tracks, required_buses);
        }
        selected_sidechain_ids = selected;
        sidechain_source_buses = next_source_buses;
        sidechain_source_tracks = next_source_tracks;
    };
    if !request.audition.soloed_tracks.is_empty() {
        required_tracks.retain(|track| request.audition.soloed_tracks.contains(track));
        routes.retain(|route| match route.source {
            AudioRouteSource::Track { track_id, .. } => required_tracks.contains(&track_id),
            AudioRouteSource::Bus { .. } => true,
        });
        selected_sidechain_ids.retain(|route_id| {
            sequence
                .audio_program
                .sidechain_routes
                .iter()
                .find(|route| route.id == *route_id)
                .is_some_and(|route| match route.source {
                    AudioRouteSource::Track { track_id, .. } => required_tracks.contains(&track_id),
                    AudioRouteSource::Bus { .. } => true,
                })
        });
    }
    let post_fader_tracks = post_fader_tracks(
        &routes,
        &sequence.audio_program.sidechain_routes,
        &selected_sidechain_ids,
    );
    let post_fader_buses = post_fader_buses(
        &routes,
        &sequence.audio_program.sidechain_routes,
        &selected_sidechain_ids,
    );

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
                strip: compile_source_strip(&channel.strip, post_fader_tracks.contains(track_id))?,
            },
        );
    }

    let mut buses = BTreeMap::new();
    for bus in &sequence.audio_program.buses {
        if required_buses.contains(&bus.id) {
            buses.insert(
                bus.id,
                compile_source_strip(&bus.strip, post_fader_buses.contains(&bus.id))?,
            );
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
                    AudioComponentSource::Media { component_id } => CompiledAudioSource::Media {
                        asset_id: clip.media_asset_id().ok_or_else(|| {
                            AudioCompileError::InvalidPlacement(format!(
                                "media audio edit {} belongs to non-media Clip {}",
                                edit.id, clip.id
                            ))
                        })?,
                        component_id,
                    },
                    AudioComponentSource::NestedOutput { output_id } => {
                        CompiledAudioSource::NestedOutput {
                            sequence_id: clip.nested_sequence_id().ok_or_else(|| {
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
                        source_origin: clip.source_origin(),
                        scale: clip.source_time_scale(),
                        sampling_boundary: clip.source_sampling_boundary(),
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

    let compiled_output = compile_output(output)?;
    let active_processor_ids = processing_scopes
        .values()
        .flat_map(|scope| scope.rack.processors.iter())
        .chain(track_channels.values().flat_map(|channel| {
            channel
                .strip
                .pre_fader
                .processors
                .iter()
                .chain(channel.strip.post_fader.processors.iter())
        }))
        .chain(buses.values().flat_map(|strip| {
            strip.pre_fader.processors.iter().chain(strip.post_fader.processors.iter())
        }))
        .chain(
            compiled_output
                .pre_fader
                .processors
                .iter()
                .chain(compiled_output.post_fader.processors.iter()),
        )
        .map(|processor| processor.instance_id)
        .collect::<BTreeSet<_>>();
    let mut sidechain_routes = sequence
        .audio_program
        .sidechain_routes
        .iter()
        .filter(|route| {
            selected_sidechain_ids.contains(&route.id)
                && active_processor_ids.contains(&route.processor_id)
        })
        .filter(|route| {
            !matches!(
                route.source,
                AudioRouteSource::Track {
                    track_id,
                    port: AudioChannelStripOutputPort::PostMute,
                } if muted_tracks.contains(&track_id)
            )
        })
        .map(|route| CompiledProcessorSidechainRoute {
            id: route.id,
            source: route.source,
            processor_id: route.processor_id,
            bus_key: route.bus_key.clone(),
            gain_db: route.gain_db,
            gain_automation: route.gain_automation.clone(),
        })
        .collect::<Vec<_>>();
    sidechain_routes.sort_by_key(|route| route.id);

    // Only the selected active Track rack is a Note consumer. Required Track
    // presence alone does not retain a pruned post-fader or bypassed processor.
    let selected_note_lane = |lane: &mondrian_timeline::audio::AudioNoteLane| {
        !lane.notes.is_empty()
            && track_channels.get(&lane.target.track_id).is_some_and(|channel| {
                [&channel.strip.pre_fader, &channel.strip.post_fader]
                    .into_iter()
                    .flat_map(|rack| &rack.processors)
                    .any(|processor| processor.instance_id == lane.target.processor_id)
            })
    };
    let (lane_count, requested_payload) = requested_compiled_note_payload(
        sequence.audio_program.note_lanes.iter().filter(|lane| selected_note_lane(lane)),
    )?;
    #[cfg(test)]
    note_materialization_tests::observe_request(lane_count, requested_payload);
    let mut note_lanes = reserve_compiled_note_rows::<CompiledNoteLane>(lane_count)?;
    for lane in sequence.audio_program.note_lanes.iter().filter(|lane| selected_note_lane(lane)) {
        let mut notes =
            reserve_compiled_note_rows::<mondrian_timeline::audio::AudioNote>(lane.notes.len())?;
        notes.extend(lane.notes.iter().cloned());
        // Global author validation already rejects duplicate lane/note IDs.
        // These two total-key sorts preserve canonical order without heap scratch.
        notes.sort_unstable_by_key(|note| note.id);
        let target = lane.target.clone();
        require_compiled_note_capacity(
            target.input_port_key.capacity(),
            lane.target.input_port_key.len(),
        )?;
        note_lanes.push(CompiledNoteLane { id: lane.id, target, notes });
    }
    note_lanes.sort_unstable_by_key(|lane| lane.id);
    #[cfg(test)]
    note_materialization_tests::observe_owners(&note_lanes);

    let program = CompiledAudioProgram {
        output_id: request.output_id,
        contributions,
        processing_scopes,
        transitions,
        track_channels,
        buses,
        bus_order,
        output: compiled_output,
        routes,
        sidechain_routes,
        note_lanes,
    };
    let actual_payload = program
        .compiled_note_payload_bytes()
        .map_err(|_| AudioCompileError::NoteEventCountOverflow)?;
    require_compiled_note_payload(actual_payload, requested_payload)?;
    Ok(program)
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

fn compile_source_strip(
    strip: &AudioChannelStrip,
    include_post_fader: bool,
) -> Result<CompiledChannelStrip, AudioCompileError> {
    if include_post_fader {
        return compile_strip(strip);
    }
    Ok(CompiledChannelStrip {
        input_trim_db: strip.input_trim_db,
        pre_fader: compile_rack(&strip.pre_fader)?,
        fader_db: 0.0,
        fader_automation: None,
        post_fader: CompiledRack::default(),
    })
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
            // This is the sole author -> execution ownership boundary. The
            // compiled plan intentionally owns ordinary execution containers
            // and does not expose authoring COW allocation identities.
            parameters: processor
                .parameters
                .iter()
                .map(|(parameter_id, parameter)| (parameter_id.clone(), parameter.clone()))
                .collect(),
            opaque_state: processor.opaque_state.as_ref().map(|state| state.as_slice().to_vec()),
        })
        .collect();
    Ok(CompiledRack { processors })
}

fn active_author_processor_ids(
    sequence: &Sequence,
    output: &AudioProgramOutput,
    required_tracks: &BTreeSet<TrackId>,
    required_buses: &BTreeSet<MixBusId>,
    post_fader_tracks: &BTreeSet<TrackId>,
    post_fader_buses: &BTreeSet<MixBusId>,
) -> BTreeSet<mondrian_core::AudioProcessorInstanceId> {
    let mut active = BTreeSet::new();
    let mut extend_rack = |rack: &AudioProcessorRack| {
        active.extend(
            rack.processors
                .iter()
                .filter(|processor| !processor.bypassed)
                .map(|processor| processor.id),
        );
    };
    extend_rack(&output.strip.pre_fader);
    extend_rack(&output.strip.post_fader);
    for track_id in required_tracks {
        if let Some(channel) = sequence.audio_program.track_channels.get(track_id) {
            extend_rack(&channel.strip.pre_fader);
            if post_fader_tracks.contains(track_id) {
                extend_rack(&channel.strip.post_fader);
            }
        }
    }
    for bus in sequence
        .audio_program
        .buses
        .iter()
        .filter(|bus| required_buses.contains(&bus.id))
    {
        extend_rack(&bus.strip.pre_fader);
        if post_fader_buses.contains(&bus.id) {
            extend_rack(&bus.strip.post_fader);
        }
    }
    let required_scope_ids = sequence
        .audio_tracks
        .iter()
        .filter(|track| required_tracks.contains(&track.id))
        .flat_map(|track| track.clips.iter().filter(|clip| !clip.is_disabled))
        .flat_map(|clip| clip.audio_components.iter().filter(|edit| edit.enabled))
        .map(|edit| edit.processing.scope_id)
        .collect::<BTreeSet<_>>();
    for scope in sequence
        .audio_program
        .processing_scopes
        .iter()
        .filter(|scope| required_scope_ids.contains(&scope.id))
    {
        extend_rack(&scope.processors);
    }
    active
}

fn post_fader_tracks(
    routes: &[CompiledRoute],
    sidechains: &[AudioProcessorSidechainRoute],
    selected_sidechains: &BTreeSet<mondrian_core::AudioRouteId>,
) -> BTreeSet<TrackId> {
    routes
        .iter()
        .map(|route| route.source)
        .chain(
            sidechains
                .iter()
                .filter(|route| selected_sidechains.contains(&route.id))
                .map(|route| route.source),
        )
        .filter_map(|source| match source {
            AudioRouteSource::Track { track_id, port }
                if port != AudioChannelStripOutputPort::PreFader =>
            {
                Some(track_id)
            }
            _ => None,
        })
        .collect()
}

fn post_fader_buses(
    routes: &[CompiledRoute],
    sidechains: &[AudioProcessorSidechainRoute],
    selected_sidechains: &BTreeSet<mondrian_core::AudioRouteId>,
) -> BTreeSet<MixBusId> {
    routes
        .iter()
        .map(|route| route.source)
        .chain(
            sidechains
                .iter()
                .filter(|route| selected_sidechains.contains(&route.id))
                .map(|route| route.source),
        )
        .filter_map(|source| match source {
            AudioRouteSource::Bus { bus_id, port }
                if port != AudioChannelStripOutputPort::PreFader =>
            {
                Some(bus_id)
            }
            _ => None,
        })
        .collect()
}

fn resolve_signal_closure(
    routes: &[AudioRoute],
    output_id: ProgramOutputId,
    additional_bus_destinations: &BTreeSet<MixBusId>,
    muted_tracks: &BTreeSet<TrackId>,
) -> (Vec<CompiledRoute>, BTreeSet<TrackId>, BTreeSet<MixBusId>) {
    let mut required_buses = additional_bus_destinations.clone();
    let mut required_tracks = BTreeSet::new();
    let mut selected = Vec::new();
    let mut pending_destinations = std::iter::once(AudioRouteDestination::Output(output_id))
        .chain(additional_bus_destinations.iter().copied().map(AudioRouteDestination::Bus))
        .collect::<Vec<_>>();
    while let Some(destination) = pending_destinations.pop() {
        for route in routes.iter().filter(|route| {
            route.enabled
                && route.destination == destination
                && !matches!(
                    route.source,
                    AudioRouteSource::Track {
                        track_id,
                        port: AudioChannelStripOutputPort::PostMute,
                    } if muted_tracks.contains(&track_id)
                )
        }) {
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
            && buses.contains(&source)
            && buses.contains(&destination)
        {
            *indegree.entry(destination).or_default() += 1;
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
            if matches!(route.source, AudioRouteSource::Bus { bus_id, .. } if bus_id == id)
                && let AudioRouteDestination::Bus(destination) = route.destination
                && let Some(value) = indegree.get_mut(&destination)
            {
                *value -= 1;
                if *value == 0 {
                    ready.insert(destination);
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
    /// A selected note cannot be projected onto the requested sample grid.
    #[error("audio note {note_id} in lane {lane_id} cannot be projected: {source}")]
    NoteSampleProjection {
        lane_id: AudioNoteLaneId,
        note_id: AudioNoteId,
        source: mondrian_core::AudioTimeError,
    },
    /// Strict exact-time duration can collapse after single-grid projection.
    #[error(
        "audio note {note_id} in lane {lane_id} collapses to samples {start_sample}..{end_sample}"
    )]
    CollapsedNoteDuration {
        lane_id: AudioNoteLaneId,
        note_id: AudioNoteId,
        start_sample: i64,
        end_sample: i64,
    },
    /// A selected score is preserved but has no admitted Note executor yet.
    #[error("audio note execution is unsupported for lane {lane_id}, processor {processor_id}")]
    NoteExecutionNotSupported {
        lane_id: AudioNoteLaneId,
        processor_id: AudioProcessorInstanceId,
    },
    /// The finite score's potential On/Off count must fit the host address range.
    #[error("audio note event count exceeds the supported address range")]
    NoteEventCountOverflow,
    /// Instantaneous Route cycle reached compilation.
    #[error("audio routing contains a cycle")]
    RouteCycle,
    /// Render Contract contains zero or unbounded values.
    #[error("invalid audio Render Contract")]
    InvalidRenderContract,
    /// A processor definition could not be realized for the concrete Render Contract.
    #[error(transparent)]
    ProcessorPreparation(#[from] crate::AudioProcessorHostError),
    /// Realized processor instances exceed the consumer's Session memory admission budget.
    #[error(
        "audio processors require {required_bytes} Session scratch bytes but the Render Contract admits {budget_bytes}"
    )]
    ProcessorScratchBudgetExceeded {
        required_bytes: usize,
        budget_bytes: usize,
    },
    /// Timeline alignment would exceed the consumer's admitted lookahead.
    #[error(
        "audio public output requires {required_frames} lookahead frames but the Render Contract admits {budget_frames}"
    )]
    PublicOutputLookaheadBudgetExceeded {
        required_frames: usize,
        budget_frames: usize,
    },
    /// PDC delay lines would exceed the consumer's Session memory budget.
    #[error(
        "audio compensation requires {required_bytes} Session scratch bytes but the Render Contract admits {budget_bytes}"
    )]
    CompensationScratchBudgetExceeded {
        required_bytes: usize,
        budget_bytes: usize,
    },
    /// Semantic IR could not be lowered into a closed dense execution schedule.
    #[error("invalid prepared audio graph: {0}")]
    InvalidPreparedGraph(String),
}

#[cfg(test)]
#[path = "compiler/note_materialization_tests.rs"]
mod note_materialization_tests;
