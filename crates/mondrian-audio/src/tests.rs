use super::*;
use mondrian_core::{
    AssetId, AudioComponentEditId, AudioRouteId, AudioSourceComponentId,
    AutomationSegmentInterpolation, ExactAutomationCurve, ExactAutomationKeyframe,
    ExactBezierHandle, ExecutionCancellationToken, ParameterId, TimeScale, TimelineTime,
};
use mondrian_timeline::audio::{
    AudioChannelStripOutputPort, AudioMixBus, AudioProcessorInstance, AudioRoute,
    AudioRouteDestination, AudioRouteSource, BUILTIN_GAIN_DEFINITION_ID, GAIN_DB_PARAMETER_ID,
};
use mondrian_timeline::{Clip, Sequence};
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Default)]
struct RampSource {
    block_reads: usize,
}

impl AudioPcmSource for RampSource {
    fn read_indexed_interleaved(
        &mut self,
        _edit: AudioComponentEditId,
        source_frames: &[i64],
        channels: usize,
        destination: &mut [f32],
    ) -> Result<(), AudioExecutionError> {
        self.block_reads = self.block_reads.saturating_add(1);
        for (frame, source_frame) in source_frames.iter().copied().enumerate() {
            let value = if source_frame < 0 {
                0.0
            } else {
                source_frame as f32 + 1.0
            };
            destination[frame * channels..(frame + 1) * channels].fill(value);
        }
        Ok(())
    }
}

struct RampDecodedSource;

impl AudioDecodedSource for RampDecodedSource {
    fn read_interleaved(
        &self,
        start_frame: i64,
        frames: usize,
        channels: usize,
        destination: &mut [f32],
        _cancellation: &ExecutionCancellationToken,
    ) -> Result<(), String> {
        if destination.len() != frames.saturating_mul(channels) {
            return Err("invalid destination extent".to_owned());
        }
        for frame in 0..frames {
            let value = start_frame.saturating_add(frame as i64).max(0) as f32 + 1.0;
            destination[frame * channels..(frame + 1) * channels].fill(value);
        }
        Ok(())
    }
}

struct RampResolver;

impl AudioMediaResolver for RampResolver {
    fn resolve(
        &self,
        _asset_id: AssetId,
        _component_id: AudioSourceComponentId,
        _contract: AudioRenderContract,
    ) -> Result<Arc<dyn AudioDecodedSource>, String> {
        Ok(Arc::new(RampDecodedSource))
    }
}

fn tt(numerator: i64, denominator: i64) -> TimelineTime {
    TimelineTime::new(numerator, denominator).expect("valid time")
}

fn gain_curve(start_db: f64, end_db: f64) -> ExactAutomationCurve {
    let parameter = ParameterId::new(GAIN_DB_PARAMETER_ID).expect("gain parameter");
    let mut curve = ExactAutomationCurve::new(parameter, start_db).expect("curve");
    curve
        .set_keyframe(ExactAutomationKeyframe::linear(
            TimelineTime::ZERO,
            start_db,
        ))
        .expect("first key");
    curve
        .set_keyframe(ExactAutomationKeyframe::linear(TimelineTime::ONE, end_db))
        .expect("second key");
    curve
}

fn hold_then_bezier_gain_curve() -> ExactAutomationCurve {
    let parameter = ParameterId::new(GAIN_DB_PARAMETER_ID).expect("gain parameter");
    let mut curve = ExactAutomationCurve::new(parameter, 0.0).expect("curve");
    let mut first = ExactAutomationKeyframe::linear(TimelineTime::ZERO, 0.0);
    first.interpolation_to_next = AutomationSegmentInterpolation::Hold;
    let mut middle = ExactAutomationKeyframe::linear(TimelineTime::ONE, 6.0);
    middle.interpolation_to_next = AutomationSegmentInterpolation::Bezier;
    middle.out_handle = Some(ExactBezierHandle { time_offset: tt(1, 4), value_offset: -1.5 });
    let mut last = ExactAutomationKeyframe::linear(tt(2, 1), 0.0);
    last.in_handle = Some(ExactBezierHandle { time_offset: tt(-1, 4), value_offset: 1.5 });
    for keyframe in [first, middle, last] {
        curve.set_keyframe(keyframe).expect("valid prepared event key");
    }
    curve
}

fn sequence_with_audio_clip() -> Sequence {
    let mut sequence = Sequence::new("audio");
    let track_id = sequence.audio_tracks[0].id;
    let clip =
        Clip::new(mondrian_core::AssetId::new(), TimelineTime::ZERO, tt(4, 1)).expect("clip");
    sequence
        .add_media_audio_clip(track_id, clip, AudioSourceComponentId::primary())
        .expect("authored audio Clip");
    sequence
}

fn prepared(sequence: &Sequence, max_frames: usize) -> Arc<PreparedAudioPlan> {
    prepared_with_backend(sequence, max_frames, AudioKernelBackend::RuntimeVectorized)
}

fn prepared_with_backend(
    sequence: &Sequence,
    max_frames: usize,
    backend: AudioKernelBackend,
) -> Arc<PreparedAudioPlan> {
    let output = sequence.audio_program.outputs[0].id;
    let compiled = compile_audio_program(sequence, AudioCompileRequest::program(output))
        .expect("compiled program");
    Arc::new(
        PreparedAudioPlan::prepare_with_backend(
            Arc::new(compiled),
            AudioRenderContract {
                sample_rate: 2,
                channels: 1,
                max_block_frames: max_frames,
                processing_mode: AudioProcessingMode::Offline,
            },
            backend,
        )
        .expect("prepared plan"),
    )
}

#[test]
fn compiler_derives_track_and_range_from_real_clip_placement() {
    let sequence = sequence_with_audio_clip();
    let clip = &sequence.audio_tracks[0].clips[0];
    let output = sequence.audio_program.outputs[0].id;
    let compiled =
        compile_audio_program(&sequence, AudioCompileRequest::program(output)).expect("compiled");
    assert_eq!(compiled.contributions().len(), 1);
    assert_eq!(compiled.contributions()[0].clip_id, clip.id);
    assert_eq!(
        compiled.contributions()[0].track_id,
        sequence.audio_tracks[0].id
    );
    assert_eq!(
        compiled.contributions()[0].sequence_range.start,
        clip.position
    );
    assert_eq!(
        compiled.contributions()[0].sequence_range.duration,
        clip.duration
    );
}

#[test]
fn source_adapter_is_crossed_once_per_contribution_block() {
    let sequence = sequence_with_audio_clip();
    let mut source = RampSource::default();
    let pcm = render_audio(
        prepared(&sequence, 4),
        &mut source,
        AudioRenderRequest { start_sample: 0, frames: 4 },
    )
    .expect("block render");

    assert_eq!(source.block_reads, 1);
    assert_eq!(pcm, vec![1.0, 2.0, 3.0, 4.0]);
}

#[test]
fn prepared_source_schedule_preserves_fractional_forward_retime() {
    let mut sequence = sequence_with_audio_clip();
    let clip = &mut sequence.audio_tracks[0].clips[0];
    clip.source_in = tt(1, 4);
    clip.speed.set_scale(TimeScale::new(3, 2).expect("exact forward scale"));

    let mut source = RampSource::default();
    let pcm = render_audio(
        prepared(&sequence, 4),
        &mut source,
        AudioRenderRequest { start_sample: 0, frames: 4 },
    )
    .expect("fractional forward render");

    assert_eq!(source.block_reads, 1);
    assert_eq!(pcm, vec![1.0, 3.0, 4.0, 6.0]);
}

#[test]
fn prepared_source_schedule_preserves_fractional_reverse_retime() {
    let mut sequence = sequence_with_audio_clip();
    let clip = &mut sequence.audio_tracks[0].clips[0];
    clip.source_in = tt(3, 1);
    clip.speed.set_scale(TimeScale::new(-1, 2).expect("exact reverse scale"));

    let mut source = RampSource::default();
    let pcm = render_audio(
        prepared(&sequence, 4),
        &mut source,
        AudioRenderRequest { start_sample: 0, frames: 4 },
    )
    .expect("fractional reverse render");

    assert_eq!(source.block_reads, 1);
    assert_eq!(pcm, vec![7.0, 6.0, 6.0, 5.0]);
}

#[test]
fn clip_track_bus_output_math_is_unclipped_and_block_invariant() {
    let mut sequence = sequence_with_audio_clip();
    let track_id = sequence.audio_tracks[0].id;
    let output_id = sequence.audio_program.outputs[0].id;
    let scope_id = sequence.audio_tracks[0].clips[0].audio_components[0].processing.scope_id;
    let mut processor = AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1);
    let curve = gain_curve(0.0, 6.0);
    processor.set_parameter_automation(curve).expect("valid gain automation");
    sequence
        .audio_program
        .processing_scopes
        .iter_mut()
        .find(|scope| scope.id == scope_id)
        .expect("scope")
        .processors
        .processors
        .push(processor);

    sequence.audio_program.routes.clear();
    let bus_id = mondrian_core::MixBusId::new();
    sequence.audio_program.buses.push(AudioMixBus {
        id: bus_id,
        name: "Bus".to_owned(),
        strip: mondrian_timeline::AudioChannelStrip::default(),
    });
    sequence.audio_program.routes.extend([
        AudioRoute {
            id: AudioRouteId::new(),
            source: AudioRouteSource::Track {
                track_id,
                port: AudioChannelStripOutputPort::PostMute,
            },
            destination: AudioRouteDestination::Bus(bus_id),
        },
        AudioRoute {
            id: AudioRouteId::new(),
            source: AudioRouteSource::Bus {
                bus_id,
                port: AudioChannelStripOutputPort::PostMute,
            },
            destination: AudioRouteDestination::Output(output_id),
        },
    ]);

    let plan = prepared(&sequence, 8);
    assert_eq!(
        plan.schedule_summary(),
        PreparedAudioScheduleSummary {
            node_count: 3,
            track_count: 1,
            bus_count: 1,
            contribution_count: 1,
            route_count: 2,
            transition_binding_count: 0,
            automation_curve_count: 1,
            automation_event_span_count: 2,
            scratch_slot_count: 2,
            output_latency_frames: 0,
            maximum_compensation_frames: 0,
        }
    );
    let mut source = RampSource::default();
    let whole = render_audio(
        Arc::clone(&plan),
        &mut source,
        AudioRenderRequest { start_sample: 0, frames: 4 },
    )
    .expect("whole");
    let first = render_audio(
        Arc::clone(&plan),
        &mut source,
        AudioRenderRequest { start_sample: 0, frames: 2 },
    )
    .expect("first");
    let second = render_audio(
        plan,
        &mut source,
        AudioRenderRequest { start_sample: 2, frames: 2 },
    )
    .expect("second");
    assert_eq!(whole, [first, second].concat());
    assert!(
        whole[2] > 3.0,
        "internal float PCM must not be clipped or tanh-shaped"
    );
}

#[test]
fn prepared_automation_event_spans_preserve_hold_bezier_and_block_partitioning() {
    let mut sequence = sequence_with_audio_clip();
    let scope_id = sequence.audio_tracks[0].clips[0].audio_components[0].processing.scope_id;
    let curve = hold_then_bezier_gain_curve();
    let mut processor = AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1);
    processor
        .set_parameter_automation(curve.clone())
        .expect("valid gain automation");
    sequence
        .audio_program
        .processing_scopes
        .iter_mut()
        .find(|scope| scope.id == scope_id)
        .expect("scope")
        .processors
        .processors
        .push(processor);

    let plan = prepared(&sequence, 6);
    let summary = plan.schedule_summary();
    assert_eq!(summary.automation_curve_count, 1);
    assert_eq!(summary.automation_event_span_count, 3);

    let mut source = RampSource::default();
    let whole = render_audio(
        Arc::clone(&plan),
        &mut source,
        AudioRenderRequest { start_sample: 0, frames: 6 },
    )
    .expect("whole prepared automation render");
    let mut partitioned = Vec::new();
    for start_sample in [0, 2, 4] {
        partitioned.extend(
            render_audio(
                Arc::clone(&plan),
                &mut source,
                AudioRenderRequest { start_sample, frames: 2 },
            )
            .expect("partitioned prepared automation render"),
        );
    }
    assert_eq!(whole, partitioned);

    let expected = (0..6)
        .map(|sample| {
            let gain_db = curve.evaluate(tt(sample, 2)).expect("reference curve");
            (sample as f32 + 1.0) * (10.0_f64.powf(gain_db / 20.0) as f32)
        })
        .collect::<Vec<_>>();
    assert_eq!(whole, expected);

    let mut session = AudioRenderSession::new(plan).expect("preallocated realtime Session");
    let capacity = session.capacity();
    let mut destination = vec![0.0; capacity.max_block_frames * capacity.channels];
    session
        .render_into(
            &mut source,
            AudioRenderRequest { start_sample: 0, frames: capacity.max_block_frames },
            &mut destination,
        )
        .expect("render within fixed allocation envelope");
    assert_eq!(session.capacity(), capacity);
}

#[test]
fn scalar_reference_and_runtime_vectorized_schedule_are_pcm_equivalent() {
    let mut sequence = sequence_with_audio_clip();
    while sequence.audio_tracks.len() < 8 {
        sequence.add_audio_track();
    }
    let additional_tracks =
        sequence.audio_tracks[1..8].iter().map(|track| track.id).collect::<Vec<_>>();
    for track_id in additional_tracks {
        let clip = Clip::new(AssetId::new(), TimelineTime::ZERO, tt(4, 1)).expect("clip");
        sequence
            .add_media_audio_clip(track_id, clip, AudioSourceComponentId::primary())
            .expect("authored audio Clip");
    }
    let scalar = prepared_with_backend(&sequence, 257, AudioKernelBackend::ScalarReference);
    let vectorized = prepared_with_backend(&sequence, 257, AudioKernelBackend::RuntimeVectorized);
    assert_eq!(scalar.schedule_summary(), vectorized.schedule_summary());
    assert_eq!(scalar.schedule_summary().track_count, 8);
    assert_eq!(scalar.schedule_summary().contribution_count, 8);
    assert_eq!(scalar.schedule_summary().route_count, 8);

    let mut scalar_source = RampSource::default();
    let mut vectorized_source = RampSource::default();
    let scalar_pcm = render_audio(
        scalar,
        &mut scalar_source,
        AudioRenderRequest { start_sample: 0, frames: 257 },
    )
    .expect("scalar render");
    let vectorized_pcm = render_audio(
        vectorized,
        &mut vectorized_source,
        AudioRenderRequest { start_sample: 0, frames: 257 },
    )
    .expect("vectorized render");
    assert_eq!(scalar_pcm, vectorized_pcm);
    assert_eq!(scalar_source.block_reads, 8);
    assert_eq!(vectorized_source.block_reads, 8);
}

#[test]
fn track_mute_zeros_post_mute_route_without_reinterpreting_the_graph() {
    let mut sequence = sequence_with_audio_clip();
    sequence.audio_tracks[0].is_muted = true;
    let mut source = RampSource::default();
    let pcm = render_audio(
        prepared(&sequence, 4),
        &mut source,
        AudioRenderRequest { start_sample: 0, frames: 4 },
    )
    .expect("muted render");
    assert_eq!(pcm, vec![0.0; 4]);
}

#[test]
fn unresolved_plugin_fails_closed_and_preserves_author_data() {
    let mut sequence = sequence_with_audio_clip();
    let track_id = sequence.audio_tracks[0].id;
    sequence
        .audio_program
        .track_channels
        .get_mut(&track_id)
        .expect("track channel")
        .strip
        .pre_fader
        .processors
        .push(AudioProcessorInstance {
            id: mondrian_core::AudioProcessorInstanceId::new(),
            definition: mondrian_timeline::AudioProcessorDefinitionRef::Clap {
                plugin_id: "com.example.effect".to_owned(),
                schema_version: 1,
            },
            bypassed: false,
            parameters: BTreeMap::new(),
            opaque_state: Some(vec![1, 2, 3]),
        });
    let output = sequence.audio_program.outputs[0].id;
    let error = compile_audio_program(&sequence, AudioCompileRequest::program(output))
        .expect_err("unresolved plugin must block");
    assert!(matches!(error, AudioCompileError::UnresolvedPlugin(_)));
}

#[test]
fn nested_public_output_uses_an_independent_recursive_session() {
    let child = sequence_with_audio_clip();
    let child_output = child.audio_program.outputs[0].id;

    let mut root = Sequence::new("root");
    let root_track = root.audio_tracks[0].id;
    let nested_clip = Clip::new_nested_sequence(
        child.id,
        TimelineTime::ZERO,
        tt(4, 1),
        Some("child".to_owned()),
    )
    .expect("nested clip");
    root.add_nested_audio_clip(root_track, nested_clip, child_output)
        .expect("nested audio authoring");

    let contract = AudioRenderContract {
        sample_rate: 2,
        channels: 1,
        max_block_frames: 8,
        processing_mode: AudioProcessingMode::Offline,
    };
    let mut runtime = AudioProgramRuntime::build(&root, &[child], &RampResolver, contract, None)
        .expect("recursive runtime");
    assert_eq!(runtime.output_latency_frames(), 0);
    let mut pcm = vec![0.0; 4];
    runtime
        .render_into(AudioRenderRequest { start_sample: 0, frames: 4 }, &mut pcm)
        .expect("nested render");
    assert_eq!(pcm, vec![1.0, 2.0, 3.0, 4.0]);
}

#[test]
fn direct_plan_preparation_rejects_unprepared_nested_latency() {
    let child = sequence_with_audio_clip();
    let child_output = child.audio_program.outputs[0].id;
    let mut root = Sequence::new("root");
    let root_track = root.audio_tracks[0].id;
    let nested_clip = Clip::new_nested_sequence(
        child.id,
        TimelineTime::ZERO,
        tt(4, 1),
        Some("child".to_owned()),
    )
    .expect("nested clip");
    root.add_nested_audio_clip(root_track, nested_clip, child_output)
        .expect("nested audio authoring");
    let root_output = root.audio_program.outputs[0].id;
    let program = Arc::new(
        compile_audio_program(&root, AudioCompileRequest::program(root_output))
            .expect("semantic program"),
    );
    let error = PreparedAudioPlan::prepare(
        program,
        AudioRenderContract {
            sample_rate: 2,
            channels: 1,
            max_block_frames: 8,
            processing_mode: AudioProcessingMode::Offline,
        },
    )
    .expect_err("nested latency must be supplied by recursive preparation");
    assert!(matches!(error, AudioCompileError::InvalidPreparedGraph(_)));
    assert!(error.to_string().contains("no prepared child-output latency"));
}

#[test]
fn nested_runtime_rejects_depth_beyond_the_shared_sequence_contract() {
    let mut child = sequence_with_audio_clip();
    let mut children = Vec::new();
    for index in 0..=mondrian_timeline::sequence::MAX_NESTED_SEQUENCE_RENDER_DEPTH {
        let child_output = child.audio_program.outputs[0].id;
        let mut parent = Sequence::new(format!("nested-{index}"));
        let parent_track = parent.audio_tracks[0].id;
        let nested_clip = Clip::new_nested_sequence(
            child.id,
            TimelineTime::ZERO,
            tt(4, 1),
            Some(child.name.clone()),
        )
        .expect("nested clip");
        parent
            .add_nested_audio_clip(parent_track, nested_clip, child_output)
            .expect("nested audio authoring");
        children.push(child);
        child = parent;
    }

    let contract = AudioRenderContract {
        sample_rate: 2,
        channels: 1,
        max_block_frames: 8,
        processing_mode: AudioProcessingMode::Offline,
    };
    let error = match AudioProgramRuntime::build(&child, &children, &RampResolver, contract, None) {
        Ok(_) => panic!("depth above contract must fail"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        AudioRuntimeBuildError::NestedDepthExceeded { .. }
    ));
}
