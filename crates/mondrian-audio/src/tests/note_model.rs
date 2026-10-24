use super::*;
use mondrian_core::{AudioNoteId, AudioNoteLaneId, AuthoringList};
use mondrian_timeline::audio::{AudioNote, AudioNoteLane, AudioNoteTarget};
use mondrian_timeline::AudioAuthoringError;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
struct CountingResolver {
    calls: AtomicUsize,
}

impl AudioProcessorResolver for CountingResolver {
    fn prepare(
        &self,
        request: AudioProcessorPrepareRequest<'_>,
    ) -> Result<Arc<dyn AudioProcessorFactory>, AudioProcessorHostError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        BuiltInAudioProcessorResolver.prepare(request)
    }
}

fn contract(sample_rate: u32) -> AudioRenderContract {
    AudioRenderContract {
        sample_rate,
        channel_layout: AudioChannelLayout::Mono,
        max_block_frames: 256,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes:
            AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
        public_output_lookahead_budget_frames:
            AudioRenderContract::DEFAULT_PUBLIC_OUTPUT_LOOKAHEAD_BUDGET_FRAMES,
        compensation_delay_scratch_budget_bytes:
            AudioRenderContract::DEFAULT_COMPENSATION_DELAY_SCRATCH_BUDGET_BYTES,
    }
}

fn score_sequence() -> Sequence {
    let mut sequence = Sequence::new("owned score");
    sequence.settings.audio_channel_layout = AudioChannelLayout::Mono;
    let track_id = sequence.audio_tracks[0].id;
    let processor = AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1);
    let processor_id = processor.id;
    sequence
        .audio_program
        .track_channels
        .get_mut(&track_id)
        .expect("channel")
        .strip
        .pre_fader
        .processors
        .push(processor);
    sequence.audio_program.note_lanes.push(AudioNoteLane {
        schema_version: 1,
        id: AudioNoteLaneId::new(),
        target: AudioNoteTarget {
            track_id,
            processor_id,
            input_port_key: "notes.0".to_owned(),
        },
        notes: AuthoringList::from(vec![AudioNote {
            id: AudioNoteId::new(),
            start: tt(1001, 30000),
            end: tt(2002, 30000),
            channel: 3,
            key: 60,
            on_velocity: -0.0,
            off_velocity: 0.5,
        }]),
    });
    sequence
}

fn compiled(sequence: &Sequence) -> CompiledAudioProgram {
    compile_audio_program(
        sequence,
        AudioCompileRequest::program(sequence.audio_program.outputs[0].id),
    )
    .expect("validated selected closure")
}

fn prepare(
    program: CompiledAudioProgram,
    sample_rate: u32,
    resolver: &CountingResolver,
) -> Result<PreparedAudioPlan, AudioCompileError> {
    PreparedAudioPlan::prepare_with_processor_resolver(
        Arc::new(program),
        contract(sample_rate),
        AudioKernelBackend::ScalarReference,
        resolver,
    )
}

#[test]
fn selected_score_is_preserved_but_prepare_refuses_before_any_resolver() {
    let sequence = score_sequence();
    let program = compiled(&sequence);
    assert_eq!(program.note_lanes.len(), 1);
    assert_eq!(
        program.execution_demand(),
        AudioProgramExecutionDemand::RequiresExecution
    );
    let expected = AudioCompileError::NoteExecutionNotSupported {
        lane_id: sequence.audio_program.note_lanes[0].id,
        processor_id: sequence.audio_program.note_lanes[0].target.processor_id,
    };
    for rate in [44_100, 48_000] {
        let resolver = CountingResolver::default();
        assert_eq!(
            prepare(program.clone(), rate, &resolver).expect_err("model-only refusal"),
            expected
        );
        assert_eq!(resolver.calls.load(Ordering::Relaxed), 0);
    }
}

#[test]
fn all_selected_endpoints_are_checked_before_the_first_unsupported_lane() {
    for rate in [44_100_u32, 48_000] {
        let mut sequence = score_sequence();
        let mut ids = [AudioNoteLaneId::new(), AudioNoteLaneId::new()];
        ids.sort();
        sequence.audio_program.note_lanes[0].id = ids[0];
        let mut later = sequence.audio_program.note_lanes[0].clone();
        later.id = ids[1];
        later.notes[0].id = AudioNoteId::new();
        later.notes[0].start = tt(1, i64::from(rate) * 4);
        later.notes[0].end = tt(1, i64::from(rate) * 2);
        let note_id = later.notes[0].id;
        sequence.audio_program.note_lanes.push(later);
        // Independent rational expectation: ceil(1/4) = ceil(1/2) = 1.
        let resolver = CountingResolver::default();
        assert_eq!(
            prepare(compiled(&sequence), rate, &resolver).expect_err("collapsed later lane"),
            AudioCompileError::CollapsedNoteDuration {
                lane_id: ids[1],
                note_id,
                start_sample: 1,
                end_sample: 1,
            }
        );
        assert_eq!(resolver.calls.load(Ordering::Relaxed), 0);
        assert!(
            sequence.audio_program.note_lanes[1].notes[0].end
                > sequence.audio_program.note_lanes[1].notes[0].start
        );
    }
}

#[test]
fn grid_dependent_collapse_and_i64_projection_overflow_are_specific_refusals() {
    let mut sequence = score_sequence();
    sequence.audio_program.note_lanes[0].notes[0].start = tt(1, 100_000);
    sequence.audio_program.note_lanes[0].notes[0].end = tt(1, 50_000);
    let resolver = CountingResolver::default();
    assert!(matches!(
        prepare(compiled(&sequence), 48_000, &resolver),
        Err(AudioCompileError::CollapsedNoteDuration { start_sample: 1, end_sample: 1, .. })
    ));
    // At 96k the same exact author endpoints project to 1 and 2, so the
    // specific collapse is gone, although native Notes remain unsupported.
    assert!(matches!(
        prepare(compiled(&sequence), 96_000, &resolver),
        Err(AudioCompileError::NoteExecutionNotSupported { .. })
    ));
    sequence.audio_program.note_lanes[0].notes[0].start = tt(i64::MAX - 1, 1);
    sequence.audio_program.note_lanes[0].notes[0].end = tt(i64::MAX, 1);
    assert!(matches!(
        prepare(compiled(&sequence), 48_000, &resolver),
        Err(AudioCompileError::NoteSampleProjection {
            source: mondrian_core::AudioTimeError::PositionOverflow,
            ..
        })
    ));
    assert_eq!(resolver.calls.load(Ordering::Relaxed), 0);
}

#[test]
fn compiled_score_owns_immutable_values_and_deterministic_identity_ranks() {
    let mut sequence = score_sequence();
    let mut second = sequence.audio_program.note_lanes[0].clone();
    second.id = AudioNoteLaneId::new();
    second.notes[0].id = AudioNoteId::new();
    let mut extra_note = second.notes[0].clone();
    extra_note.id = AudioNoteId::new();
    second.notes.push(extra_note);
    sequence.audio_program.note_lanes.push(second);
    let program = compiled(&sequence);
    assert!(program.note_lanes.windows(2).all(|pair| pair[0].id < pair[1].id));
    assert!(program
        .note_lanes
        .iter()
        .all(|lane| lane.notes.windows(2).all(|pair| pair[0].id < pair[1].id)));
    sequence.audio_program.note_lanes.reverse();
    for lane in &mut sequence.audio_program.note_lanes {
        lane.notes.reverse();
    }
    assert_eq!(compiled(&sequence), program);
    sequence.audio_program.note_lanes[0].target.input_port_key = "changed".to_owned();
    sequence.audio_program.note_lanes[0].notes[0].key = 64;
    assert!(program.note_lanes.iter().all(|lane| lane.target.input_port_key == "notes.0"));
    assert!(program
        .note_lanes
        .iter()
        .flat_map(|lane| &lane.notes)
        .all(|note| note.key == 60));
}

#[test]
fn bypass_empty_muted_unreachable_and_solo_pruned_scores_keep_note_free_preparation() {
    let original = score_sequence();
    for case in 0..5 {
        let mut sequence = original.clone();
        let track_id = sequence.audio_tracks[0].id;
        match case {
            0 => sequence.audio_program.track_channels.get_mut(&track_id).expect("channel")
                .strip.pre_fader.processors[0].bypassed = true,
            1 => sequence.audio_program.note_lanes[0].notes.clear(),
            2 => sequence.audio_tracks[0].is_muted = true,
            3 => sequence.audio_program.routes.retain(|route|
                !matches!(route.source, AudioRouteSource::Track { track_id: id, .. } if id == track_id)),
            4 => {},
            _ => unreachable!(),
        }
        let mut request = AudioCompileRequest::program(sequence.audio_program.outputs[0].id);
        if case == 4 {
            request.audition.soloed_tracks.insert(sequence.audio_tracks[1].id);
        }
        let program = compile_audio_program(&sequence, request).expect("selected score closure");
        assert!(program.note_lanes.is_empty());
        let resolver = CountingResolver::default();
        prepare(program, 48_000, &resolver).expect("former note-free preparation");
        assert_eq!(
            resolver.calls.load(Ordering::Relaxed),
            if case == 1 { 1 } else { 0 }
        );
        assert_eq!(sequence.audio_program.note_lanes.len(), 1);
    }
}

#[test]
fn pre_mute_signal_retains_pre_rack_score_but_not_pruned_post_rack_score() {
    let mut sequence = score_sequence();
    let track_id = sequence.audio_tracks[0].id;
    let post = AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1);
    let post_id = post.id;
    sequence
        .audio_program
        .track_channels
        .get_mut(&track_id)
        .expect("channel")
        .strip
        .post_fader
        .processors
        .push(post);
    let mut post_lane = sequence.audio_program.note_lanes[0].clone();
    post_lane.id = AudioNoteLaneId::new();
    post_lane.target.processor_id = post_id;
    post_lane.notes[0].id = AudioNoteId::new();
    sequence.audio_program.note_lanes.push(post_lane);
    assert_eq!(compiled(&sequence).note_lanes.len(), 2);
    sequence.audio_tracks[0].is_muted = true;
    for route in &mut sequence.audio_program.routes {
        if let AudioRouteSource::Track { track_id: id, port } = &mut route.source
            && *id == track_id
        {
            *port = AudioChannelStripOutputPort::PreFader;
        }
    }
    let program = compiled(&sequence);
    assert_eq!(program.note_lanes.len(), 1);
    assert_eq!(
        program.note_lanes[0].target.processor_id,
        sequence.audio_program.note_lanes[0].target.processor_id
    );
    assert!(program.track_channels[&track_id].strip.post_fader.processors.is_empty());
    assert_eq!(sequence.audio_program.note_lanes.len(), 2);
    let resolver = CountingResolver::default();
    assert!(matches!(
        prepare(program, 48_000, &resolver),
        Err(AudioCompileError::NoteExecutionNotSupported { .. })
    ));
    assert_eq!(resolver.calls.load(Ordering::Relaxed), 0);
}

#[test]
fn unreachable_invalid_score_still_fails_complete_author_validation() {
    let mut sequence = score_sequence();
    let track_id = sequence.audio_tracks[0].id;
    sequence.audio_program.routes.retain(|route|
        !matches!(route.source, AudioRouteSource::Track { track_id: id, .. } if id == track_id));
    sequence.audio_program.note_lanes[0].target.input_port_key = "\n".to_owned();
    assert!(matches!(
        compile_audio_program(
            &sequence,
            AudioCompileRequest::program(sequence.audio_program.outputs[0].id)
        ),
        Err(AudioCompileError::InvalidAuthoring(
            AudioAuthoringError::InvalidNotePortKey(_)
        ))
    ));
}

// Uses existing Runtime construction and media provider, without new executor or ledger.
fn nested_score_root(child: &Sequence) -> Sequence {
    let mut root = Sequence::new("nested score admission");
    root.settings.audio_channel_layout = AudioChannelLayout::Mono;
    root.add_nested_audio_clip(
        root.audio_tracks[0].id,
        Clip::new_nested_sequence(child.id, TimelineTime::ZERO, tt(4, 1), None)
            .expect("existing nested Clip constructor"),
        child.audio_program.outputs[0].id,
    )
    .expect("existing nested audio authoring");
    root
}

#[test]
fn selected_nested_score_reaches_child_runtime_preparation_refusal() {
    let child = score_sequence();
    let lane = &child.audio_program.note_lanes[0];
    let (lane_id, processor_id) = (lane.id, lane.target.processor_id);
    let child_program = compiled(&child);
    assert_eq!(child_program.note_lanes.len(), 1);
    assert_eq!(
        child_program.execution_demand(),
        AudioProgramExecutionDemand::RequiresExecution
    );
    let root = nested_score_root(&child);
    // The public Runtime must recurse to the child's own Plan. No assertion is
    // made that earlier root/sibling media or processor resolution never ran.
    match AudioProgramRuntime::build(
        &root,
        std::slice::from_ref(&child),
        &RampResolver,
        contract(48_000),
        None,
    ) {
        Err(AudioRuntimeBuildError::Compile(AudioCompileError::NoteExecutionNotSupported {
            lane_id: actual_lane,
            processor_id: actual_processor,
        })) => {
            assert_eq!(actual_lane, lane_id);
            assert_eq!(actual_processor, processor_id);
        }
        Err(other) => panic!("expected nested child note refusal, got {other:?}"),
        Ok(_) => panic!("selected note-bearing child must not prepare as silence"),
    }
}

#[test]
fn empty_nested_score_preserves_existing_runtime_and_silent_pcm() {
    let mut child = score_sequence();
    child.audio_program.note_lanes[0].notes.clear();
    let child_program = compiled(&child);
    assert!(child_program.note_lanes.is_empty());
    // The active built-in rack is unchanged; this proves admission still works,
    // rather than weakening the negative by bypassing its processor.
    assert_eq!(
        child_program.execution_demand(),
        AudioProgramExecutionDemand::RequiresExecution
    );
    let root = nested_score_root(&child);
    let mut runtime = AudioProgramRuntime::build(
        &root,
        std::slice::from_ref(&child),
        &RampResolver,
        contract(48_000),
        None,
    )
    .expect("existing empty-score nested Runtime");
    let mut pcm = [123.0_f32; 32];
    runtime
        .render_into(AudioRenderRequest { transport_activity: None, start_sample: 0, frames: 32 }, &mut pcm)
        .expect("existing silent child render");
    assert_eq!(pcm, [0.0; 32]);
    assert_eq!(child.audio_program.note_lanes.len(), 1);
    assert!(child.audio_program.note_lanes[0].notes.is_empty());
}
