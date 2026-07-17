use super::*;
use mondrian_core::{
    AssetId, AudioComponentEditId, AudioRouteId, AudioSourceComponentId, ExactAutomationCurve,
    ExactAutomationKeyframe, ParameterId, TimelineTime,
};
use mondrian_timeline::audio::{
    AudioChannelStripOutputPort, AudioMixBus, AudioProcessorInstance, AudioRoute,
    AudioRouteDestination, AudioRouteSource, BUILTIN_GAIN_DEFINITION_ID, GAIN_DB_PARAMETER_ID,
};
use mondrian_timeline::{Clip, Sequence};
use std::collections::BTreeMap;
use std::sync::Arc;

struct RampSource;

impl AudioPcmSource for RampSource {
    fn sample(
        &mut self,
        _edit: AudioComponentEditId,
        source_frame: i64,
        _channel: usize,
    ) -> Result<f32, AudioExecutionError> {
        Ok(source_frame.max(0) as f32 + 1.0)
    }
}

struct RampDecodedSource;

impl AudioDecodedSource for RampDecodedSource {
    fn sample(&self, frame: i64, _channel: usize) -> f32 {
        frame.max(0) as f32 + 1.0
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
    let output = sequence.audio_program.outputs[0].id;
    let compiled = compile_audio_program(sequence, AudioCompileRequest::program(output))
        .expect("compiled program");
    Arc::new(
        PreparedAudioPlan::prepare(
            Arc::new(compiled),
            AudioRenderContract {
                sample_rate: 2,
                channels: 1,
                max_block_frames: max_frames,
                processing_mode: AudioProcessingMode::Offline,
            },
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
fn clip_track_bus_output_math_is_unclipped_and_block_invariant() {
    let mut sequence = sequence_with_audio_clip();
    let track_id = sequence.audio_tracks[0].id;
    let output_id = sequence.audio_program.outputs[0].id;
    let scope_id = sequence.audio_tracks[0].clips[0].audio_components[0].processing.scope_id;
    let mut processor = AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1);
    let curve = gain_curve(0.0, 6.0);
    processor.parameters.insert(curve.parameter_id.clone(), curve);
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
    let mut source = RampSource;
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
fn track_mute_zeros_post_mute_route_without_reinterpreting_the_graph() {
    let mut sequence = sequence_with_audio_clip();
    sequence.audio_tracks[0].is_muted = true;
    let mut source = RampSource;
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
    let mut pcm = vec![0.0; 4];
    runtime
        .render_into(AudioRenderRequest { start_sample: 0, frames: 4 }, &mut pcm)
        .expect("nested render");
    assert_eq!(pcm, vec![1.0, 2.0, 3.0, 4.0]);
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
