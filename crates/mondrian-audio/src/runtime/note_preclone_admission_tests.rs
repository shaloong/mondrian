//! Real compiler/Runtime admission boundary; public nonempty Notes stay refused.
use super::*;
use crate::compiler::observe_note_materialization;
use crate::{AudioCompileError, BuiltInAudioProcessorResolver};
use mondrian_core::{AudioNoteId, AudioNoteLaneId, AuthoringList, TimelineTime};
use mondrian_timeline::audio::{
    AudioNote, AudioNoteLane, AudioNoteTarget, AudioProcessorInstance, BUILTIN_GAIN_DEFINITION_ID,
};
use mondrian_timeline::{AudioAuthoringError, Clip};
use std::sync::atomic::{AtomicUsize, Ordering};

fn time(n: i64, d: i64) -> TimelineTime {
    TimelineTime::new(n, d).unwrap()
}

fn contract() -> AudioRenderContract {
    AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Mono,
        max_block_frames: 32,
        processing_mode: crate::AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes:
            AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
        public_output_lookahead_budget_frames:
            AudioRenderContract::DEFAULT_PUBLIC_OUTPUT_LOOKAHEAD_BUDGET_FRAMES,
        compensation_delay_scratch_budget_bytes:
            AudioRenderContract::DEFAULT_COMPENSATION_DELAY_SCRATCH_BUDGET_BYTES,
    }
}

fn score() -> Sequence {
    let mut sequence = Sequence::new("preclone admission");
    sequence.settings.audio_channel_layout = AudioChannelLayout::Mono;
    let track = sequence.audio_tracks[0].id;
    let processor = AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1);
    let processor_id = processor.id;
    sequence
        .audio_program
        .track_channels
        .get_mut(&track)
        .unwrap()
        .strip
        .pre_fader
        .processors
        .push(processor);
    for (key, count) in [("é", 1), ("notes.0", 2)] {
        sequence.audio_program.note_lanes.push(AudioNoteLane {
            schema_version: 1,
            id: AudioNoteLaneId::new(),
            target: AudioNoteTarget {
                track_id: track,
                processor_id,
                input_port_key: key.to_owned(),
            },
            notes: AuthoringList::from(
                (0..count)
                    .map(|n| AudioNote {
                        id: AudioNoteId::new(),
                        start: time(n, 48_000),
                        end: time(n + 1, 48_000),
                        channel: 3,
                        key: 60,
                        on_velocity: -0.0,
                        off_velocity: 0.5,
                    })
                    .collect::<Vec<_>>(),
            ),
        });
    }
    sequence
}

fn grant(bytes: usize) -> AudioRuntimeResourceGrant {
    AudioRuntimeResourceGrant::new(usize::MAX, usize::MAX, bytes)
}

#[derive(Default)]
struct FailingMedia(AtomicUsize);
impl AudioMediaResolver for FailingMedia {
    fn resolve(
        &self,
        _: AssetId,
        _: AudioSourceComponentId,
        _: u32,
    ) -> Result<ResolvedAudioSource, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err("intentional missing media".to_owned())
    }
}

#[derive(Default)]
struct CountingProcessors(AtomicUsize);
impl AudioProcessorResolver for CountingProcessors {
    fn prepare(
        &self,
        request: crate::AudioProcessorPrepareRequest<'_>,
    ) -> Result<Arc<dyn crate::AudioProcessorFactory>, crate::AudioProcessorHostError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        BuiltInAudioProcessorResolver.prepare(request)
    }
}

fn build(
    sequence: &Sequence,
    children: &[Sequence],
    media: &FailingMedia,
    processors: &CountingProcessors,
    render: AudioRenderContract,
    bytes: usize,
) -> Result<AudioProgramRuntime, AudioRuntimeBuildError> {
    AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
        sequence,
        children,
        media,
        processors,
        render,
        None,
        grant(bytes),
    )
}

fn error(result: Result<AudioProgramRuntime, AudioRuntimeBuildError>) -> AudioRuntimeBuildError {
    match result {
        Err(error) => error,
        Ok(_) => panic!("nonempty Notes stay unsupported"),
    }
}

fn required(sequence: &Sequence) -> usize {
    let media = FailingMedia::default();
    let processors = CountingProcessors::default();
    let (result, requests, owners) =
        observe_note_materialization(|| build(sequence, &[], &media, &processors, contract(), 0));
    assert_eq!(
        (
            requests,
            owners,
            media.0.load(Ordering::SeqCst),
            processors.0.load(Ordering::SeqCst)
        ),
        (0, 0, 0, 0)
    );
    match error(result) {
        AudioRuntimeBuildError::ResourceGrantExceeded {
            category: AudioRuntimeResourceCategory::PreparedLogicalBytes,
            sequence_id,
            output_id,
            required,
            granted: 0,
        } => {
            assert_eq!(sequence_id, sequence.id);
            assert_eq!(output_id, sequence.audio_program.outputs[0].id);
            assert!(required > 0);
            required
        }
        other => panic!("expected preclone grant refusal: {other:?}"),
    }
}

#[test]
fn g_minus_one_refuses_before_real_note_owners_and_exact_g_keeps_unsupported() {
    let sequence = score();
    let g = required(&sequence);
    let media = FailingMedia::default();
    let processors = CountingProcessors::default();
    let (low, requests, owners) = observe_note_materialization(|| {
        build(&sequence, &[], &media, &processors, contract(), g - 1)
    });
    assert_eq!((requests, owners), (0, 0));
    assert!(
        matches!(error(low), AudioRuntimeBuildError::ResourceGrantExceeded {
        category: AudioRuntimeResourceCategory::PreparedLogicalBytes, required, granted, ..
    } if required == g && granted == g - 1)
    );
    // A fresh public builder owns a fresh ledger after the failed build.
    let (exact, requests, owners) =
        observe_note_materialization(|| build(&sequence, &[], &media, &processors, contract(), g));
    assert_eq!((requests, owners), (1, 1));
    assert!(matches!(
        error(exact),
        AudioRuntimeBuildError::Compile(AudioCompileError::NoteExecutionNotSupported { .. })
    ));
    assert_eq!(processors.0.load(Ordering::SeqCst), 0);
}

#[test]
fn low_grant_precedes_media_failure_and_exact_grant_restores_media_error() {
    let mut sequence = score();
    sequence
        .add_media_audio_clip(
            sequence.audio_tracks[0].id,
            Clip::new(AssetId::new(), TimelineTime::ZERO, time(1, 1)).unwrap(),
            AudioSourceComponentId::primary(),
        )
        .unwrap();
    let g = required(&sequence);
    let media = FailingMedia::default();
    let processors = CountingProcessors::default();
    let (low, requests, owners) = observe_note_materialization(|| {
        build(&sequence, &[], &media, &processors, contract(), g - 1)
    });
    assert!(matches!(
        error(low),
        AudioRuntimeBuildError::ResourceGrantExceeded { .. }
    ));
    assert_eq!(
        (requests, owners, media.0.load(Ordering::SeqCst)),
        (0, 0, 0)
    );
    let (exact, requests, owners) =
        observe_note_materialization(|| build(&sequence, &[], &media, &processors, contract(), g));
    assert!(
        matches!(error(exact), AudioRuntimeBuildError::Media { reason, .. }
        if reason == "intentional missing media")
    );
    assert_eq!(
        (requests, owners, media.0.load(Ordering::SeqCst)),
        (1, 1, 1)
    );
    assert_eq!(processors.0.load(Ordering::SeqCst), 0);
}

#[test]
fn low_grant_precedes_missing_child_and_exact_grant_restores_missing_child() {
    let mut sequence = score();
    let missing = SequenceId::new();
    sequence
        .add_nested_audio_clip(
            sequence.audio_tracks[0].id,
            Clip::new_nested_sequence(missing, TimelineTime::ZERO, time(1, 1), None).unwrap(),
            ProgramOutputId::new(),
        )
        .unwrap();
    let g = required(&sequence);
    let media = FailingMedia::default();
    let processors = CountingProcessors::default();
    let (low, requests, owners) = observe_note_materialization(|| {
        build(&sequence, &[], &media, &processors, contract(), g - 1)
    });
    assert!(matches!(
        error(low),
        AudioRuntimeBuildError::ResourceGrantExceeded { .. }
    ));
    assert_eq!((requests, owners), (0, 0));
    let (exact, requests, owners) =
        observe_note_materialization(|| build(&sequence, &[], &media, &processors, contract(), g));
    assert!(
        matches!(error(exact), AudioRuntimeBuildError::MissingNestedSequence(id) if id == missing)
    );
    assert_eq!((requests, owners), (1, 1));
}

#[test]
fn low_grant_precedes_contract_and_endpoint_faults_then_restores_original_faults() {
    for endpoint_fault in [false, true] {
        let mut sequence = score();
        let mut render = contract();
        if endpoint_fault {
            sequence.audio_program.note_lanes[1].notes[1].start = time(1, 192_000);
            sequence.audio_program.note_lanes[1].notes[1].end = time(1, 96_000);
        } else {
            render.sample_rate = 0;
        }
        let g = required(&sequence);
        let media = FailingMedia::default();
        let processors = CountingProcessors::default();
        let (low, requests, owners) = observe_note_materialization(|| {
            build(&sequence, &[], &media, &processors, render, g - 1)
        });
        assert!(matches!(
            error(low),
            AudioRuntimeBuildError::ResourceGrantExceeded { .. }
        ));
        assert_eq!((requests, owners), (0, 0));
        let (exact, requests, owners) =
            observe_note_materialization(|| build(&sequence, &[], &media, &processors, render, g));
        assert_eq!((requests, owners), (1, 1));
        match error(exact) {
            AudioRuntimeBuildError::Compile(AudioCompileError::InvalidRenderContract)
                if !endpoint_fault => {}
            AudioRuntimeBuildError::Compile(AudioCompileError::CollapsedNoteDuration {
                start_sample: 1,
                end_sample: 1,
                ..
            }) if endpoint_fault => {}
            other => panic!("wrong sufficient-grant priority: {other:?}"),
        }
        assert_eq!(processors.0.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn author_output_and_non_note_compile_errors_still_precede_grant_refusal() {
    let media = FailingMedia::default();
    let processors = CountingProcessors::default();
    let mut bad_author = score();
    bad_author.audio_program.note_lanes[0].target.input_port_key = "\n".to_owned();
    let (result, requests, owners) = observe_note_materialization(|| {
        build(&bad_author, &[], &media, &processors, contract(), 0)
    });
    assert!(matches!(
        error(result),
        AudioRuntimeBuildError::Compile(AudioCompileError::InvalidAuthoring(
            AudioAuthoringError::InvalidNotePortKey(_)
        ))
    ));
    assert_eq!((requests, owners), (0, 0));
    let sequence = score();
    let missing = ProgramOutputId::new();
    let (result, requests, owners) = observe_note_materialization(|| {
        AudioProgramRuntime::build_with_compile_request_processor_resolver_and_resource_grant(
            &sequence,
            &[],
            &media,
            &processors,
            contract(),
            AudioCompileRequest::program(missing),
            grant(0),
        )
    });
    assert!(matches!(error(result), AudioRuntimeBuildError::Compile(
        AudioCompileError::OutputNotFound(id)) if id == missing));
    assert_eq!((requests, owners), (0, 0));
    let mut bad_graph = score();
    bad_graph
        .add_media_audio_clip(
            bad_graph.audio_tracks[0].id,
            Clip::new(AssetId::new(), TimelineTime::ZERO, time(1, 1)).unwrap(),
            AudioSourceComponentId::primary(),
        )
        .unwrap();
    bad_graph.audio_tracks[0].clips[0].duration = time(-1, 1);
    let (result, requests, owners) =
        observe_note_materialization(|| build(&bad_graph, &[], &media, &processors, contract(), 0));
    assert!(matches!(
        error(result),
        AudioRuntimeBuildError::Compile(AudioCompileError::InvalidPlacement(_))
    ));
    assert_eq!(
        (requests, owners, media.0.load(Ordering::SeqCst)),
        (0, 0, 0)
    );
}

#[test]
fn window_and_exact_precompiled_arc_families_keep_their_previous_boundary() {
    let sequence = score();
    let range = TimelineTimeRange::new(TimelineTime::ZERO, time(1, 1)).unwrap();
    let media = FailingMedia::default();
    let processors = CountingProcessors::default();
    let (direct, requests, owners) = observe_note_materialization(|| {
        AudioProgramRuntime::build_for_range_with_resource_grant(
            &sequence,
            &[],
            &media,
            contract(),
            None,
            range,
            grant(0),
        )
    });
    assert!(matches!(
        error(direct),
        AudioRuntimeBuildError::Compile(AudioCompileError::NoteExecutionNotSupported { .. })
    ));
    assert_eq!((requests, owners), (1, 1));
    let closure = crate::compile_audio_dependency_closure(&sequence, &[], None, range).unwrap();
    let original = closure.root_program().clone();
    let (precompiled, requests, owners) = observe_note_materialization(|| {
        AudioProgramRuntime::build_from_precompiled_closure_for_range_with_processor_resolver_and_resource_grant(
            &sequence, &[], &media, &processors, contract(), None, range, &closure, grant(0))
    });
    assert!(matches!(
        error(precompiled),
        AudioRuntimeBuildError::Compile(AudioCompileError::NoteExecutionNotSupported { .. })
    ));
    assert_eq!((requests, owners), (0, 0));
    assert!(Arc::ptr_eq(closure.root_program(), &original));
    assert_eq!(processors.0.load(Ordering::SeqCst), 0);
}

#[test]
fn actual_note_compiler_caller_preserves_ancestor_and_completed_sibling_charges() {
    let sequence = score();
    let g = required(&sequence);
    let output = sequence.audio_program.outputs[0].id;
    let mut ledger = AudioRuntimeAdmissionLedger::new(grant(g + 10 + 7 - 1));
    let _ancestor = ledger.reserve_prepared_plan(sequence.id, output, 10).unwrap();
    let sibling = ledger.reserve_prepared_plan(sequence.id, output, 9).unwrap();
    ledger
        .complete_prepared_session(
            sibling,
            AudioSessionResourceFootprint {
                prepared_logical_bytes: 7,
                ..AudioSessionResourceFootprint::default()
            },
        )
        .unwrap();
    let before = ledger.footprint;
    let (result, requests, owners) = observe_note_materialization(|| {
        crate::compiler::compile_audio_program_with_note_admission(
            &sequence,
            AudioCompileRequest::program(output),
            |graph, notes| ledger.reserve_compiled_note_plan(sequence.id, output, graph, notes),
        )
    });
    assert!(
        matches!(result, Err(AudioRuntimeBuildError::ResourceGrantExceeded {
        required, granted, category: AudioRuntimeResourceCategory::PreparedLogicalBytes, ..
    }) if required == g + 17 && granted == g + 16)
    );
    assert_eq!((requests, owners), (0, 0));
    assert_eq!(ledger.footprint, before);
    assert_eq!(ledger.footprint.prepared_logical_bytes, 17);
    assert_eq!(ledger.footprint.runtime_occurrences, 1);
}

#[test]
fn same_compiler_receipt_contains_checked_ac_counts_and_no_second_retained_a() {
    let sequence = score();
    let output = sequence.audio_program.outputs[0].id;
    let baseline = compile_audio_program(&sequence, AudioCompileRequest::program(output)).unwrap();
    let mut ledger = AudioRuntimeAdmissionLedger::new(grant(usize::MAX));
    let mut facts = None;
    let (program, receipt) = crate::compiler::compile_audio_program_with_note_admission(
        &sequence,
        AudioCompileRequest::program(output),
        |graph, notes| {
            facts = Some((
                notes.lanes,
                notes.events,
                notes.key_bytes,
                notes.compiler_bytes,
            ));
            ledger.reserve_compiled_note_plan(sequence.id, output, graph, notes)
        },
    )
    .unwrap();
    let ac = std::alloc::Layout::array::<crate::plan::CompiledNoteLane>(2).unwrap().size()
        + std::alloc::Layout::array::<AudioNote>(1).unwrap().size()
        + std::alloc::Layout::array::<AudioNote>(2).unwrap().size()
        + 9;
    assert_eq!(facts, Some((2, 6, 9, ac)));
    assert_eq!(program, baseline);
    assert_eq!(program.compiled_note_payload_bytes().unwrap(), ac);
    assert!(receipt.is_some());
    assert_eq!(ledger.footprint.prepared_logical_bytes, required(&sequence));
    // No public successful nonempty Session/completion is claimed here: its
    // original Unsupported guard still prevents actual common N production.
}

#[test]
fn real_descendant_admission_keeps_pending_note_parent_until_build_abandonment() {
    let mut child = Sequence::new("empty descendant");
    child.settings.audio_channel_layout = AudioChannelLayout::Mono;
    let media = FailingMedia::default();
    let processors = CountingProcessors::default();
    let child_runtime = build(&child, &[], &media, &processors, contract(), usize::MAX).unwrap();
    let child_n = child_runtime.resource_footprint().prepared_logical_bytes;
    let child_program = compile_audio_program(
        &child,
        AudioCompileRequest::program(child.audio_program.outputs[0].id),
    )
    .unwrap();
    assert_eq!(
        child_n,
        PreparedAudioPlan::requested_base_logical_bytes(&child_program).unwrap()
    );
    drop(child_runtime);
    let mut root = score();
    root.add_nested_audio_clip(
        root.audio_tracks[0].id,
        Clip::new_nested_sequence(child.id, TimelineTime::ZERO, time(1, 1), None).unwrap(),
        child.audio_program.outputs[0].id,
    )
    .unwrap();
    let root_r = required(&root);
    for short_by in [1, 0] {
        let mut ledger = AudioRuntimeAdmissionLedger::new(grant(root_r + child_n - short_by));
        let mut stack = BTreeSet::new();
        let result = AudioProgramRuntime::build_inner(
            &root,
            std::slice::from_ref(&child),
            &media,
            &processors,
            contract(),
            None,
            None,
            None,
            &mut stack,
            0,
            &mut ledger,
            None,
        );
        assert!(stack.is_empty());
        if short_by == 1 {
            assert!(
                matches!(error(result), AudioRuntimeBuildError::ResourceGrantExceeded {
                category: AudioRuntimeResourceCategory::PreparedLogicalBytes,
                sequence_id, required, granted, ..
            } if sequence_id == child.id && required == root_r + child_n
                && granted == root_r + child_n - 1)
            );
            assert_eq!(ledger.footprint.prepared_logical_bytes, root_r);
            assert_eq!(ledger.footprint.runtime_occurrences, 0);
        } else {
            assert!(matches!(
                error(result),
                AudioRuntimeBuildError::Compile(
                    AudioCompileError::NoteExecutionNotSupported { .. }
                )
            ));
            // The child's actual common N was completed in the same ledger;
            // the root's local R remains pending after its Unsupported error.
            assert_eq!(ledger.footprint.prepared_logical_bytes, root_r + child_n);
            assert_eq!(ledger.footprint.runtime_occurrences, 1);
            assert!(ledger.footprint.nested_window_bytes > 0);
        }
        // This consumed/failed builder ledger leaves scope, never retried/refunded.
    }
}

#[test]
fn request_overflow_is_checked_before_original_ledger_mutation() {
    let sequence = score();
    let output = sequence.audio_program.outputs[0].id;
    let program = compile_audio_program(&sequence, AudioCompileRequest::program(output)).unwrap();
    let mut ledger = AudioRuntimeAdmissionLedger::new(grant(usize::MAX));
    let before = ledger.footprint;
    let sum_overflow = ledger.reserve_compiled_note_plan(
        sequence.id,
        output,
        program.graph(),
        crate::plan::CompiledNoteStorageRequest {
            lanes: 2,
            events: 6,
            key_bytes: 9,
            compiler_bytes: usize::MAX,
        },
    );
    assert!(matches!(
        sum_overflow,
        Err(AudioRuntimeBuildError::ResourceExtentOverflow {
            category: AudioRuntimeResourceCategory::PreparedLogicalBytes,
            ..
        })
    ));
    assert_eq!(ledger.footprint, before);
    let phase_overflow = ledger.reserve_compiled_note_plan(
        sequence.id,
        output,
        program.graph(),
        crate::plan::CompiledNoteStorageRequest {
            lanes: usize::MAX,
            events: 6,
            key_bytes: 9,
            compiler_bytes: 1,
        },
    );
    assert!(matches!(
        phase_overflow,
        Err(AudioRuntimeBuildError::Compile(
            AudioCompileError::NoteEventCountOverflow
        ))
    ));
    assert_eq!(ledger.footprint, before);
}
