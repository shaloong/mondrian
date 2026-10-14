//! Refusal/order controls for the new CLAP family, never native/DSP evidence.
use super::*;
use crate::compiler::observe_note_materialization;
use crate::AudioCompileError;
use mondrian_core::{AudioNoteId, AudioNoteLaneId, AudioProcessorInstanceId, AuthoringList};
use mondrian_timeline::audio::{
    AudioNote, AudioNoteLane, AudioNoteTarget, AudioProcessorDefinitionRef, AudioProcessorInstance,
    BUILTIN_GAIN_DEFINITION_ID,
};
use mondrian_timeline::Clip;
use std::sync::atomic::{AtomicUsize, Ordering};

fn time(sample: i64) -> mondrian_core::TimelineTime {
    mondrian_core::TimelineTime::new(sample, 48_000).unwrap()
}
fn contract() -> AudioRenderContract {
    AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Mono,
        max_block_frames: 32,
        processing_mode: crate::AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes: 16 * 1024 * 1024,
        public_output_lookahead_budget_frames: 4096,
        compensation_delay_scratch_budget_bytes: 1024 * 1024,
    }
}
fn score() -> Sequence {
    let mut sequence = Sequence::new("public Note preparation scope");
    sequence.settings.audio_channel_layout = AudioChannelLayout::Mono;
    let track = sequence.audio_tracks[0].id;
    let processor = AudioProcessorInstance {
        id: AudioProcessorInstanceId::new(),
        definition: AudioProcessorDefinitionRef::Clap {
            plugin_id: "test.preparation.refuses".to_owned(),
            schema_version: 1,
            binary_sha256: Some([7; 32]),
        },
        bypassed: false,
        parameters: Default::default(),
        opaque_state: None,
    };
    let id = processor.id;
    sequence
        .audio_program
        .track_channels
        .get_mut(&track)
        .unwrap()
        .strip
        .pre_fader
        .processors
        .push(processor);
    sequence.audio_program.note_lanes.push(AudioNoteLane {
        schema_version: 1,
        id: AudioNoteLaneId::new(),
        target: AudioNoteTarget {
            track_id: track,
            processor_id: id,
            input_port_key: "clap.note-input.0".to_owned(),
        },
        notes: AuthoringList::from(vec![AudioNote {
            id: AudioNoteId::new(),
            start: time(0),
            end: time(2),
            channel: 0,
            key: 60,
            on_velocity: 1.0,
            off_velocity: 0.0,
        }]),
    });
    sequence
}
struct NoMedia;
impl AudioMediaResolver for NoMedia {
    fn resolve(
        &self,
        _: AssetId,
        _: AudioSourceComponentId,
        _: u32,
    ) -> Result<ResolvedAudioSource, String> {
        panic!("scope refusal must precede media binding")
    }
}
#[derive(Default)]
struct RefusingResolver(AtomicUsize);
impl AudioProcessorResolver for RefusingResolver {
    fn prepare(
        &self,
        request: crate::AudioProcessorPrepareRequest<'_>,
    ) -> Result<Arc<dyn crate::AudioProcessorFactory>, crate::AudioProcessorHostError> {
        assert!(
            request.note_input_requested(),
            "exact targeted occurrence requests Notes"
        );
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(crate::AudioProcessorHostError::Unavailable(
            "owned resolver stop before create".to_owned(),
        ))
    }
}
fn build(
    sequence: &Sequence,
    resolver: &RefusingResolver,
    logical: usize,
) -> Result<AudioProgramRuntime, AudioRuntimeBuildError> {
    AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
        sequence,
        &[],
        &NoMedia,
        resolver,
        contract(),
        None,
        AudioRuntimeResourceGrant::new(usize::MAX, usize::MAX, logical),
    )
}
fn error(result: Result<AudioProgramRuntime, AudioRuntimeBuildError>) -> AudioRuntimeBuildError {
    result.err().expect("owned resolver never creates a native/DSP instance")
}
#[test]
fn clap_root_preclone_g_is_checked_before_owned_factories() {
    let sequence = score();
    let resolver = RefusingResolver::default();
    let (low, requests, owners) = observe_note_materialization(|| build(&sequence, &resolver, 0));
    assert_eq!(
        (requests, owners, resolver.0.load(Ordering::SeqCst)),
        (0, 0, 0)
    );
    let g = match error(low) {
        AudioRuntimeBuildError::ResourceGrantExceeded {
            category: AudioRuntimeResourceCategory::PreparedLogicalBytes,
            required,
            granted: 0,
            ..
        } => required,
        other => panic!("expected original preclone grant refusal: {other:?}"),
    };
    let (low, requests, owners) =
        observe_note_materialization(|| build(&sequence, &resolver, g - 1));
    assert!(
        matches!(error(low), AudioRuntimeBuildError::ResourceGrantExceeded {
        category: AudioRuntimeResourceCategory::PreparedLogicalBytes, required, granted, ..
    } if required == g && granted == g-1)
    );
    assert_eq!(
        (requests, owners, resolver.0.load(Ordering::SeqCst)),
        (0, 0, 0)
    );
    let (exact, requests, owners) = observe_note_materialization(|| build(&sequence, &resolver, g));
    assert!(matches!(error(exact), AudioRuntimeBuildError::Compile(
        AudioCompileError::ProcessorPreparation(crate::AudioProcessorHostError::Unavailable(ref detail))
    ) if detail == "owned resolver stop before create"));
    assert_eq!(
        (requests, owners, resolver.0.load(Ordering::SeqCst)),
        (1, 1, 1)
    );
    // This G only reaches the owned refusal; it is not a sufficient native grant.
}
#[test]
fn ordinary_plan_keeps_nonempty_clap_guard_before_factory() {
    let sequence = score();
    let program = Arc::new(
        compile_audio_program(
            &sequence,
            AudioCompileRequest::program(sequence.audio_program.outputs[0].id),
        )
        .unwrap(),
    );
    let resolver = RefusingResolver::default();
    let result = PreparedAudioPlan::prepare_with_processor_resolver(
        program,
        contract(),
        AudioKernelBackend::default(),
        &resolver,
    );
    assert!(matches!(
        result,
        Err(AudioCompileError::NoteExecutionNotSupported { .. })
    ));
    assert_eq!(resolver.0.load(Ordering::SeqCst), 0);
}
#[test]
fn mixed_clap_and_builtin_targets_refuse_before_any_factory() {
    let mut sequence = score();
    let track = sequence.audio_tracks[0].id;
    let builtin = AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1);
    let id = builtin.id;
    sequence
        .audio_program
        .track_channels
        .get_mut(&track)
        .unwrap()
        .strip
        .post_fader
        .processors
        .push(builtin);
    let mut lane = sequence.audio_program.note_lanes[0].clone();
    lane.id = AudioNoteLaneId::new();
    lane.notes[0].id = AudioNoteId::new();
    lane.target.processor_id = id;
    sequence.audio_program.note_lanes.push(lane);
    let resolver = RefusingResolver::default();
    assert!(matches!(error(build(&sequence, &resolver, usize::MAX)),
        AudioRuntimeBuildError::Compile(AudioCompileError::NoteExecutionNotSupported { processor_id, .. })
        if processor_id == id));
    assert_eq!(resolver.0.load(Ordering::SeqCst), 0);
}
#[test]
fn clap_root_nested_contribution_refuses_before_child_or_factory() {
    let mut sequence = score();
    sequence
        .add_nested_audio_clip(
            sequence.audio_tracks[0].id,
            Clip::new_nested_sequence(SequenceId::new(), time(0), time(32), None).unwrap(),
            ProgramOutputId::new(),
        )
        .unwrap();
    let resolver = RefusingResolver::default();
    assert!(matches!(
        error(build(&sequence, &resolver, usize::MAX)),
        AudioRuntimeBuildError::Compile(AudioCompileError::NoteExecutionNotSupported { .. })
    ));
    assert_eq!(resolver.0.load(Ordering::SeqCst), 0);
}
#[test]
fn clap_child_refuses_before_its_factory_even_with_original_receipt() {
    let sequence = score();
    let resolver = RefusingResolver::default();
    let mut ledger = AudioRuntimeAdmissionLedger::new(AudioRuntimeResourceGrant::unbounded());
    let result = AudioProgramRuntime::build_inner(
        &sequence,
        &[],
        &NoMedia,
        &resolver,
        contract(),
        None,
        None,
        None,
        &mut BTreeSet::new(),
        1,
        &mut ledger,
    );
    assert!(matches!(
        error(result),
        AudioRuntimeBuildError::Compile(AudioCompileError::NoteExecutionNotSupported { .. })
    ));
    assert_eq!(resolver.0.load(Ordering::SeqCst), 0);
}
#[test]
fn clap_window_refuses_before_factory_and_does_not_gain_direct_receipt() {
    let sequence = score();
    let resolver = RefusingResolver::default();
    let mut ledger = AudioRuntimeAdmissionLedger::new(AudioRuntimeResourceGrant::unbounded());
    let result = AudioProgramRuntime::build_inner(
        &sequence,
        &[],
        &NoMedia,
        &resolver,
        contract(),
        None,
        Some(
            DependencyWindow::from_half_open(TimelineTimeRange::new(time(0), time(32)).unwrap())
                .unwrap(),
        ),
        None,
        &mut BTreeSet::new(),
        0,
        &mut ledger,
    );
    assert!(matches!(
        error(result),
        AudioRuntimeBuildError::Compile(AudioCompileError::NoteExecutionNotSupported { .. })
    ));
    assert_eq!(resolver.0.load(Ordering::SeqCst), 0);
}
#[test]
fn clap_precompiled_arc_keeps_guard_and_does_not_recompile_or_prepare_factory() {
    let sequence = score();
    let range = TimelineTimeRange::new(time(0), time(32)).unwrap();
    let closure = crate::compile_audio_dependency_closure(&sequence, &[], None, range).unwrap();
    let original = closure.root_program().clone();
    let resolver = RefusingResolver::default();
    let (result, requests, owners) = observe_note_materialization(|| {
        AudioProgramRuntime::build_from_precompiled_closure_for_range_with_processor_resolver_and_resource_grant(
            &sequence, &[], &NoMedia, &resolver, contract(), None, range, &closure,
            AudioRuntimeResourceGrant::new(usize::MAX, usize::MAX, 0),
        )
    });
    assert!(matches!(
        error(result),
        AudioRuntimeBuildError::Compile(AudioCompileError::NoteExecutionNotSupported { .. })
    ));
    assert_eq!(
        (requests, owners, resolver.0.load(Ordering::SeqCst)),
        (0, 0, 0)
    );
    assert!(Arc::ptr_eq(closure.root_program(), &original));
}
