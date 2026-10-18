// Actual compiled/prepared Note owners and the existing logical policy.
use super::*;
use crate::{
    AudioCompileRequest, AudioMediaResolver, AudioNoteExecutionLimits, AudioParameterEvent,
    AudioParameterEventBatch, AudioParameterPlanResourceReplacement, AudioProcessingMode,
    AudioProcessor, AudioProcessorAudioIo, AudioProcessorExecutionContract, AudioProcessorFactory,
    AudioProcessorHostError, AudioProcessorNoteInputContract, AudioProcessorNoteInputPort,
    AudioProcessorPrepareRequest, AudioProcessorProcessContext, AudioProgramRuntime,
    AudioRuntimeResourceGrant, NativeHostAllocationOwner, ResolvedAudioSource,
};
use mondrian_core::{
    AssetId, AudioNoteId, AudioNoteLaneId, AudioProcessorInstanceId, AudioSourceComponentId,
    AuthoringList, NativeAudioEditorInsertion, NativeAudioEditorLocator,
    NativeAudioEditorOccurrenceOwner,
};
use mondrian_timeline::audio::{
    AudioNote, AudioNoteLane, AudioNoteTarget, AudioProcessorDefinitionRef, AudioProcessorInstance,
};
use mondrian_timeline::Sequence;
use std::sync::atomic::{AtomicUsize, Ordering};

const PORT: &str = " exact.é.port ";

#[test]
fn runtime_fresh_entry_origin_is_read_from_the_prepared_score_and_grid() {
    for rate in [48_000, 96_000] {
        let (mut sequence, locator) = sequence();
        sequence.settings.audio_sample_rate = rate;
        let calls = Arc::new(Calls::default());
        let mut runtime = AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
            &sequence,
            &[],
            &UnusedMedia,
            &Resolver(Arc::clone(&calls)),
            AudioRenderContract { sample_rate: rate, ..contract() },
            None,
            AudioRuntimeResourceGrant::new(1, 64 * 1024 * 1024, 64 * 1024 * 1024),
        )
        .unwrap();
        assert_eq!(runtime.state_entry_origin().unwrap(), None);
        runtime.require_state_entry_recursively_for_test();
        assert!(runtime.requires_state_entry());
        assert_eq!(
            runtime.state_entry_origin().unwrap(),
            None,
            "ordinary history has no fixed origin"
        );
        add_score(&mut sequence, locator);
        runtime
            .prepare_owned_note_model_for_test(Arc::new(compiled(&sequence)))
            .unwrap();
        let footprint = runtime.resource_footprint();
        let queries = calls.note_queries.load(Ordering::Relaxed);
        let creates = calls.creates.load(Ordering::Relaxed);
        for _ in 0..3 {
            assert_eq!(
                runtime.state_entry_origin().unwrap(),
                Some(mondrian_core::AudioSamplePosition::new(
                    0,
                    mondrian_core::AudioSampleRate::new(rate).unwrap(),
                ))
            );
        }
        assert_eq!(runtime.resource_footprint(), footprint);
        assert_eq!(calls.note_queries.load(Ordering::Relaxed), queries);
        assert_eq!(calls.creates.load(Ordering::Relaxed), creates);
    }
}

#[derive(Default)]
struct Calls {
    resolves: AtomicUsize,
    note_queries: AtomicUsize,
    creates: AtomicUsize,
    seed_queries: AtomicUsize,
}

struct Resolver(Arc<Calls>);
struct Factory(Arc<Calls>);
struct OwnedProcessor;

impl AudioProcessorResolver for Resolver {
    fn prepare(
        &self,
        _: AudioProcessorPrepareRequest<'_>,
    ) -> Result<Arc<dyn AudioProcessorFactory>, AudioProcessorHostError> {
        self.0.resolves.fetch_add(1, Ordering::Relaxed);
        Ok(Arc::new(Factory(Arc::clone(&self.0))))
    }
}

impl AudioProcessorFactory for Factory {
    fn execution_contract(&self) -> AudioProcessorExecutionContract {
        AudioProcessorExecutionContract::stateless()
    }

    fn note_input_contract(&self) -> AudioProcessorNoteInputContract {
        self.0.note_queries.fetch_add(1, Ordering::Relaxed);
        AudioProcessorNoteInputContract {
            completion: crate::AudioNoteCompletionPolicy::NativeEnd,
            ports: vec![AudioProcessorNoteInputPort {
                channel_count: 16,
                port_key: PORT.to_owned(),
                native_id: 17,
                native_index: 0,
            }],
            limits: AudioNoteExecutionLimits {
                input_events: 8,
                output_ends: 8,
                retained_records: 8,
                live_voices: 8,
            },
        }
    }

    fn with_native_configuration_seed(
        &self,
        _: crate::NativeConfigurationFormat,
        _: &[u8],
    ) -> Result<Option<Arc<dyn AudioProcessorFactory>>, AudioProcessorHostError> {
        self.0.seed_queries.fetch_add(1, Ordering::Relaxed);
        Err(AudioProcessorHostError::Unavailable(
            "not a native fixture".to_owned(),
        ))
    }

    fn create(
        &self,
        _native_child_settlement: Option<&mondrian_core::NativeChildSettlementRegistrar>,
    ) -> Result<Box<dyn AudioProcessor>, AudioProcessorHostError> {
        self.0.creates.fetch_add(1, Ordering::Relaxed);
        Ok(Box::new(OwnedProcessor))
    }
}

impl AudioProcessor for OwnedProcessor {
    fn enter_state(&mut self, _: i64) -> Result<(), AudioProcessorHostError> {
        Ok(())
    }

    fn process(
        &mut self,
        _: AudioProcessorProcessContext,
        _: &mut dyn AudioProcessorAudioIo,
        _: AudioParameterEventBatch<'_>,
    ) -> Result<(), AudioProcessorHostError> {
        Ok(())
    }
}

struct UnusedMedia;
impl AudioMediaResolver for UnusedMedia {
    fn resolve(
        &self,
        _: AssetId,
        _: AudioSourceComponentId,
        _: u32,
    ) -> Result<ResolvedAudioSource, String> {
        Err("this author graph has no media".to_owned())
    }
}

fn contract() -> AudioRenderContract {
    AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Mono,
        max_block_frames: 16,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes: 1024 * 1024,
        public_output_lookahead_budget_frames: 1024,
        compensation_delay_scratch_budget_bytes: 1024 * 1024,
    }
}

fn sequence() -> (Sequence, NativeAudioEditorLocator) {
    let mut sequence = Sequence::new("compiled Note policy owners");
    sequence.settings.audio_channel_layout = AudioChannelLayout::Mono;
    sequence.settings.audio_sample_rate = 48_000;
    let track = sequence.audio_tracks[0].id;
    let instance_id = AudioProcessorInstanceId::new();
    sequence
        .audio_program
        .track_channels
        .get_mut(&track)
        .unwrap()
        .strip
        .pre_fader
        .processors
        .push(AudioProcessorInstance {
            id: instance_id,
            definition: AudioProcessorDefinitionRef::Clap {
                plugin_id: "owned.compiled-note-policy".to_owned(),
                schema_version: 1,
                binary_sha256: Some([1; 32]),
            },
            bypassed: false,
            parameters: Default::default(),
            opaque_state: None,
        });
    let locator = NativeAudioEditorLocator {
        sequence: sequence.id,
        instance: instance_id,
        owner: NativeAudioEditorOccurrenceOwner::Track(track),
        insertion: NativeAudioEditorInsertion::PreFader,
    };
    (sequence, locator)
}

fn add_score(sequence: &mut Sequence, locator: NativeAudioEditorLocator) {
    let NativeAudioEditorOccurrenceOwner::Track(track_id) = locator.owner else {
        panic!("fixture is an actual Track target");
    };
    sequence.audio_program.note_lanes.push(AudioNoteLane {
        schema_version: 1,
        id: AudioNoteLaneId::new(),
        target: AudioNoteTarget {
            track_id,
            processor_id: locator.instance,
            input_port_key: PORT.to_owned(),
        },
        notes: AuthoringList::from(vec![AudioNote {
            id: AudioNoteId::new(),
            start: TimelineTime::ZERO,
            end: TimelineTime::new(1, 48_000).unwrap(),
            channel: 3,
            key: 60,
            on_velocity: -0.0,
            off_velocity: f64::from_bits(0x3fd3_3333_3333_3335),
        }]),
    });
}

fn compiled(sequence: &Sequence) -> CompiledAudioProgram {
    crate::compile_audio_program(
        sequence,
        AudioCompileRequest::program(sequence.audio_program.outputs[0].id),
    )
    .unwrap()
}

fn fixture() -> (PreparedAudioPlan, NativeAudioEditorLocator, Arc<Calls>) {
    let (mut sequence, locator) = sequence();
    let calls = Arc::new(Calls::default());
    let base = PreparedAudioPlan::prepare_with_processor_resolver(
        Arc::new(compiled(&sequence)),
        contract(),
        AudioKernelBackend::ScalarReference,
        &Resolver(Arc::clone(&calls)),
    )
    .unwrap();
    add_score(&mut sequence, locator);
    let score = base.with_owned_note_score_for_test(Arc::new(compiled(&sequence))).unwrap();
    assert_eq!(calls.resolves.load(Ordering::Relaxed), 1);
    assert_eq!(calls.creates.load(Ordering::Relaxed), 0);
    (score, locator, calls)
}

#[test]
fn cached_note_width_and_completion_drift_refuse_before_session_or_factory_create() {
    let (plan, _, calls) = fixture();
    let queries = calls.note_queries.load(Ordering::Relaxed);
    assert!(
        crate::processor_host::PreparedProcessorHost::allocate_storage(&plan.schedule, contract())
            .is_ok()
    );
    for change_policy in [false, true] {
        let mut invalid = plan.clone();
        let mut targets = invalid.schedule.notes.targets().to_vec();
        if change_policy {
            targets[0].completion = crate::AudioNoteCompletionPolicy::SuccessfulOffBlock;
        } else {
            targets[0].channel_count = 4;
        }
        invalid.schedule.notes = super::prepared_notes::PreparedNoteScore::from_test_parts(
            targets,
            invalid.schedule.notes.events().to_vec(),
        );
        assert!(matches!(
            crate::processor_host::PreparedProcessorHost::allocate_storage(
                &invalid.schedule,
                contract()
            ),
            Err(crate::AudioExecutionError::InvalidPreparedSchedule)
        ));
        assert_eq!(calls.creates.load(Ordering::Relaxed), 0);
        assert_eq!(calls.note_queries.load(Ordering::Relaxed), queries);
    }
}

// Independent existing owner receipts, not a second policy evaluator.
fn owner_bytes(program: &CompiledAudioProgram) -> usize {
    let mut owners = vec![program.note_lanes.host_allocation().unwrap()];
    for lane in &program.note_lanes {
        owners.push(lane.notes.host_allocation().unwrap());
        owners.push(lane.target.input_port_key.host_allocation().unwrap());
    }
    assert!(owners
        .iter()
        .filter(|owner| owner.layout().size() != 0)
        .all(|owner| owner.address() != 0));
    owners.iter().map(|owner| owner.layout().size()).sum()
}

#[test]
fn compiled_payload_observes_all_actual_retained_owners_and_spare_capacities() {
    let (plan, _, _) = fixture();
    let mut program = plan.program().clone();
    assert_eq!(
        program.compiled_note_payload_bytes().unwrap(),
        owner_bytes(&program)
    );
    let mut previous = program.compiled_note_payload_bytes().unwrap();
    let mut observed = owner_bytes(&program);
    program.note_lanes.reserve_exact(11);
    let next_observed = owner_bytes(&program);
    assert!(next_observed > observed);
    assert_eq!(
        program.compiled_note_payload_bytes().unwrap() - previous,
        next_observed - observed
    );
    previous = program.compiled_note_payload_bytes().unwrap();
    observed = next_observed;
    program.note_lanes[0].notes.reserve_exact(13);
    let next_observed = owner_bytes(&program);
    assert!(next_observed > observed);
    assert_eq!(
        program.compiled_note_payload_bytes().unwrap() - previous,
        next_observed - observed
    );
    previous = program.compiled_note_payload_bytes().unwrap();
    observed = next_observed;
    program.note_lanes[0].target.input_port_key.reserve_exact(29);
    let next_observed = owner_bytes(&program);
    assert!(next_observed > observed);
    assert_eq!(
        program.compiled_note_payload_bytes().unwrap() - previous,
        next_observed - observed
    );

    let mut empty = program.note_lanes[0].clone();
    empty.id = AudioNoteLaneId::new();
    empty.notes.clear();
    empty.notes.reserve_exact(7);
    empty.target.input_port_key.reserve_exact(31);
    assert!(empty.notes.host_allocation().unwrap().layout().size() > 0);
    program.note_lanes.push(empty);
    assert_eq!(
        program.compiled_note_payload_bytes().unwrap(),
        owner_bytes(&program)
    );
}

#[test]
fn all_empty_compiled_score_has_zero_policy_even_with_real_spare_owners() {
    let (plan, _, _) = fixture();
    let mut program = plan.program().clone();
    program.note_lanes[0].notes.clear();
    program.note_lanes[0].notes.reserve_exact(19);
    program.note_lanes[0].target.input_port_key.reserve_exact(23);
    program.note_lanes.reserve_exact(17);
    assert!(owner_bytes(&program) > 0);
    assert_eq!(program.compiled_note_payload_bytes().unwrap(), 0);
    let calls = Arc::new(Calls::default());
    let empty = PreparedAudioPlan::prepare_with_processor_resolver(
        Arc::new(program.clone()),
        contract(),
        AudioKernelBackend::ScalarReference,
        &Resolver(Arc::clone(&calls)),
    )
    .unwrap();
    program.note_lanes.clear();
    let scoreless = PreparedAudioPlan::prepare_with_processor_resolver(
        Arc::new(program),
        contract(),
        AudioKernelBackend::ScalarReference,
        &Resolver(Arc::clone(&calls)),
    )
    .unwrap();
    assert_eq!(
        empty.session_resource_footprint().unwrap(),
        scoreless.session_resource_footprint().unwrap()
    );
    assert_eq!(empty.schedule.notes.storage_bytes().unwrap(), 0);
    assert_eq!(calls.creates.load(Ordering::Relaxed), 0);
}

#[test]
fn local_footprint_adds_compiled_owner_once_and_shared_arc_keeps_per_role_weight() {
    let (plan, _, _) = fixture();
    let mut without_a = plan.clone();
    let mut scoreless = plan.program().clone();
    scoreless.note_lanes.clear();
    without_a.program = Arc::new(scoreless);
    // Same actual prepared schedule; only the retained compiled owner differs.
    let mut expected = without_a.session_resource_footprint().unwrap();
    expected.prepared_logical_bytes += owner_bytes(plan.program());
    assert_eq!(plan.session_resource_footprint().unwrap(), expected);
    let alias = plan.clone();
    assert!(Arc::ptr_eq(&alias.program, &plan.program));
    assert_eq!(alias.session_resource_footprint().unwrap(), expected);
    assert_eq!(
        plan.program().note_lanes[0].notes[0].on_velocity.to_bits(),
        (-0.0_f64).to_bits()
    );
    assert_eq!(
        plan.program().note_lanes[0].notes[0].off_velocity.to_bits(),
        0x3fd3_3333_3333_3335
    );
}

#[test]
fn requested_native_author_uses_next_actual_compiled_owner_once() {
    let (plan, locator, calls) = fixture();
    let mut next = plan.program().clone();
    let before = plan.requested_native_author_logical_bytes(&next, locator).unwrap();
    let old_owner = owner_bytes(&next);
    let original_plan = plan.preparation_id();
    let original_program = Arc::as_ptr(&plan.program);
    let original_prepared = plan.schedule.notes.storage_bytes().unwrap();
    let queries = calls.note_queries.load(Ordering::Relaxed);
    next.note_lanes.reserve_exact(11);
    next.note_lanes[0].notes.reserve_exact(13);
    next.note_lanes[0].target.input_port_key.reserve_exact(29);
    let delta = owner_bytes(&next) - old_owner;
    assert!(delta > 0);
    assert_eq!(
        plan.requested_native_author_logical_bytes(&next, locator).unwrap(),
        before + delta
    );
    assert_eq!(plan.preparation_id(), original_plan);
    assert_eq!(Arc::as_ptr(&plan.program), original_program);
    assert_eq!(
        plan.schedule.notes.storage_bytes().unwrap(),
        original_prepared
    );
    assert_eq!(calls.note_queries.load(Ordering::Relaxed), queries);
    assert_eq!(calls.creates.load(Ordering::Relaxed), 0);
    assert_eq!(calls.seed_queries.load(Ordering::Relaxed), 0);
}

#[test]
fn generic_clone_accepts_real_smaller_aggregate_and_preserves_note_identity_bits() {
    let (mut plan, locator, calls) = fixture();
    let mut targets = Vec::with_capacity(13);
    targets.extend(plan.schedule.notes.targets().iter().cloned());
    targets[0].input_port_key.reserve_exact(37);
    let mut events = Vec::with_capacity(17);
    events.extend_from_slice(plan.schedule.notes.events());
    plan.schedule.notes = PreparedNoteScore::from_test_parts(targets, events);
    let upper = plan.schedule.notes.storage_bytes().unwrap();
    let original_plan = plan.preparation_id();
    let original_program = Arc::as_ptr(&plan.program);
    let original_events = plan.schedule.notes.events().to_vec();
    let original_targets = plan.schedule.notes.targets().to_vec();
    let queries = calls.note_queries.load(Ordering::Relaxed);
    let next_program = Arc::clone(&plan.program);
    let next = plan
        .prepare_native_author_parameters(Arc::clone(&next_program), locator)
        .unwrap();
    assert!(next.schedule.notes.storage_bytes().unwrap() < upper);
    assert_ne!(next.preparation_id(), original_plan);
    assert!(Arc::ptr_eq(&next.program, &next_program));
    assert_eq!(next.schedule.notes.events(), original_events.as_slice());
    assert_eq!(next.schedule.notes.targets(), original_targets.as_slice());
    assert_eq!(
        next.schedule.notes.events()[0].velocity.to_bits(),
        (-0.0_f64).to_bits()
    );
    assert_eq!(
        next.schedule.notes.events()[1].velocity.to_bits(),
        0x3fd3_3333_3333_3335
    );
    assert_eq!(plan.preparation_id(), original_plan);
    assert_eq!(Arc::as_ptr(&plan.program), original_program);
    assert_eq!(plan.schedule.notes.storage_bytes().unwrap(), upper);
    assert_eq!(calls.note_queries.load(Ordering::Relaxed), queries);
    assert_eq!(calls.creates.load(Ordering::Relaxed), 0);
    assert_eq!(calls.seed_queries.load(Ordering::Relaxed), 0);
    assert!(
        next.session_resource_footprint().unwrap().prepared_logical_bytes
            <= plan.requested_native_author_logical_bytes(&next_program, locator).unwrap()
    );
}

#[test]
fn common_overlap_receipt_propagates_actual_compiled_payload_per_plan_role() {
    let (mut sequence, locator) = sequence();
    let calls = Arc::new(Calls::default());
    let grant = AudioRuntimeResourceGrant::new(1, 64 * 1024 * 1024, 64 * 1024 * 1024);
    let mut runtime = AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
        &sequence,
        &[],
        &UnusedMedia,
        &Resolver(Arc::clone(&calls)),
        contract(),
        None,
        grant,
    )
    .unwrap();
    add_score(&mut sequence, locator);
    runtime
        .prepare_owned_note_model_for_test(Arc::new(compiled(&sequence)))
        .unwrap();
    let initial = runtime.resource_footprint();
    let identity = runtime.resource_root_identity();
    let current = runtime.find_resource_session(identity).unwrap();
    let old = current.current_resource_footprint().unwrap();
    let mut program = current.resource_plan().program().clone();
    program.note_lanes.reserve_exact(11);
    program.note_lanes[0].target.input_port_key.reserve_exact(37);
    let next = current
        .resource_plan()
        .prepare_native_author_parameters(Arc::new(program), locator)
        .unwrap();
    let n = next.session_resource_footprint().unwrap();
    let mut no_a = next.clone();
    let mut empty = next.program().clone();
    empty.note_lanes.clear();
    no_a.program = Arc::new(empty);
    assert_eq!(
        n.prepared_logical_bytes
            - no_a.session_resource_footprint().unwrap().prepared_logical_bytes,
        owner_bytes(next.program())
    );
    let events: Vec<AudioParameterEvent> = Vec::new();
    let lanes: Vec<Range<usize>> = Vec::new();
    let replacements = [AudioParameterPlanResourceReplacement::new(
        current, &next, &events, &lanes,
    )];
    let extra = Vec::new();
    let admitted = runtime.admit_parameter_plan_overlap(&replacements, &extra).unwrap();
    assert_eq!(
        admitted.peak_prepared_logical_bytes(),
        initial.prepared_logical_bytes + n.prepared_logical_bytes
    );
    let commit = admitted.into_commit(&runtime, &replacements).unwrap();
    assert_eq!(
        commit.logical_role_weights(),
        (old.prepared_logical_bytes, n.prepared_logical_bytes)
    );
    assert_eq!(
        commit.footprint().prepared_logical_bytes,
        initial.prepared_logical_bytes - old.prepared_logical_bytes + n.prepared_logical_bytes
    );
    assert_eq!(runtime.resource_footprint(), initial);
    assert_eq!(runtime.resource_root_identity(), identity);
    assert_eq!(runtime.resource_grant(), grant);
    assert_eq!(
        current.resource_plan().program().note_lanes[0].notes[0].on_velocity.to_bits(),
        (-0.0_f64).to_bits()
    );
    assert_eq!(calls.seed_queries.load(Ordering::Relaxed), 0);
}

#[test]
fn legacy_public_refusal_and_bitwise_eligibility_still_precede_new_accounting() {
    let (plan, locator, calls) = fixture();
    let resolves = calls.resolves.load(Ordering::Relaxed);
    let queries = calls.note_queries.load(Ordering::Relaxed);
    let score = plan.program().clone();
    assert_eq!(
        PreparedAudioPlan::prepare_with_processor_resolver(
            Arc::new(score.clone()),
            contract(),
            AudioKernelBackend::ScalarReference,
            &Resolver(Arc::clone(&calls)),
        )
        .unwrap_err(),
        AudioCompileError::NoteExecutionNotSupported {
            lane_id: score.note_lanes[0].id,
            processor_id: locator.instance,
        }
    );
    let mut collapsed = score.clone();
    collapsed.note_lanes[0].notes[0].start = TimelineTime::new(1, 192_000).unwrap();
    collapsed.note_lanes[0].notes[0].end = TimelineTime::new(1, 96_000).unwrap();
    assert!(matches!(
        PreparedAudioPlan::prepare_with_processor_resolver(
            Arc::new(collapsed),
            contract(),
            AudioKernelBackend::ScalarReference,
            &Resolver(Arc::clone(&calls)),
        ),
        Err(AudioCompileError::CollapsedNoteDuration { .. })
    ));
    let mut changed = score;
    changed.note_lanes[0].notes[0].on_velocity = 0.0;
    changed.note_lanes.reserve_exact(17);
    let expected = AudioCompileError::InvalidPreparedGraph(
        "native author handoff changed immutable Note score".to_owned(),
    );
    assert_eq!(
        plan.requested_native_author_logical_bytes(&changed, locator),
        Err(expected.clone())
    );
    assert_eq!(
        plan.prepare_native_author_parameters(Arc::new(changed), locator).unwrap_err(),
        expected
    );
    assert_eq!(calls.resolves.load(Ordering::Relaxed), resolves);
    assert_eq!(calls.note_queries.load(Ordering::Relaxed), queries);
    assert_eq!(calls.creates.load(Ordering::Relaxed), 0);
    assert_eq!(calls.seed_queries.load(Ordering::Relaxed), 0);
}
