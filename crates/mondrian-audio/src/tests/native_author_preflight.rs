//! Public Preserved handoff preflight and ownership controls.
//! Uses actual Runtime/Source/Hosted ownership with independent owned processors.
//! This is logical-policy and Rust-owned lifecycle evidence, not native DLL proof.

use super::*;
use crate::processor_isolation::measure_host_allocations_for_test;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
struct PreflightCalls {
    observations: AtomicUsize,
    binds: AtomicUsize,
    creates: AtomicUsize,
}
impl PreflightCalls {
    fn snapshot(&self) -> [usize; 3] {
        [
            self.observations.load(Ordering::Relaxed),
            self.binds.load(Ordering::Relaxed),
            self.creates.load(Ordering::Relaxed),
        ]
    }
}
#[derive(Default)]
struct PreflightRegistry {
    probes: Mutex<Vec<Arc<Mutex<HandoffProbe>>>>,
    calls: PreflightCalls,
}
struct PreflightResolver(Arc<PreflightRegistry>);
struct PreflightFactory(Arc<PreflightRegistry>, Option<Vec<u8>>, bool);
struct PreflightProcessor {
    inner: HandoffProcessor,
    registry: Arc<PreflightRegistry>,
}
impl AudioProcessorResolver for PreflightResolver {
    fn prepare(
        &self,
        request: AudioProcessorPrepareRequest<'_>,
    ) -> Result<Arc<dyn AudioProcessorFactory>, AudioProcessorHostError> {
        if matches!(request.definition(), mondrian_timeline::AudioProcessorDefinitionRef::Clap { plugin_id, .. }
            if plugin_id == "test.mondrian.handoff")
            || matches!(request.definition(), mondrian_timeline::AudioProcessorDefinitionRef::Vst3 { class_id, .. }
                if class_id == "portable.actual.rescan")
        {
            Ok(Arc::new(PreflightFactory(
                Arc::clone(&self.0),
                None,
                matches!(
                    request.definition(),
                    mondrian_timeline::AudioProcessorDefinitionRef::Vst3 { .. }
                ),
            )))
        } else {
            BuiltInAudioProcessorResolver.prepare(request)
        }
    }
}
impl AudioProcessorFactory for PreflightFactory {
    fn native_child_settlement_requirement(
        &self,
    ) -> mondrian_core::NativeChildSettlementRequirement {
        // This portable author model has no OS child, despite its synthetic ABI facts.
        mondrian_core::NativeChildSettlementRequirement::NotApplicable
    }

    fn execution_contract(&self) -> AudioProcessorExecutionContract {
        AudioProcessorExecutionContract::new(0, AudioProcessorTail::None, true, true, true, 0)
            .expect("same portable zero-latency contract")
    }
    fn visit_host_owned_storage<'a>(
        &'a self,
        visitor: &mut dyn FnMut(
            AudioProcessorFactoryStorageBorrow<'a>,
        ) -> Result<(), AudioProcessorFactoryStorageError>,
    ) -> Result<(), AudioProcessorFactoryStorageError> {
        // The same explicit unresolved Arc payload frontier as the old fixture.
        visitor(AudioProcessorFactoryStorageBorrow::arc_payload(&self.0))?;
        if let Some(seed) = &self.1 {
            visitor(AudioProcessorFactoryStorageBorrow::complete(seed)?)?;
        }
        Ok(())
    }
    fn with_native_configuration_seed(
        &self,
        format: NativeConfigurationFormat,
        configuration: &[u8],
    ) -> Result<Option<Arc<dyn AudioProcessorFactory>>, AudioProcessorHostError> {
        // Same portable cold-seed contract as HandoffFactory: retain the exact
        // scalar bytes and report their actual owner, without live state mutation.
        assert_eq!(
            format,
            if self.2 {
                NativeConfigurationFormat::Vst3ComponentAndController
            } else {
                NativeConfigurationFormat::ClapState
            }
        );
        assert_eq!(configuration.len(), 8);
        Ok(Some(Arc::new(Self(
            Arc::clone(&self.0),
            Some(configuration.to_vec()),
            self.2,
        ))))
    }
    fn create(
        &self,
        _native_child_settlement: Option<&mondrian_core::NativeChildSettlementRegistrar>,
    ) -> Result<Box<dyn AudioProcessor>, AudioProcessorHostError> {
        let ordinal = self.0.calls.creates.fetch_add(1, Ordering::Relaxed);
        let probe = Arc::new(Mutex::new(HandoffProbe {
            vst_rescan_abi: self.2,
            ..Default::default()
        }));
        self.0.probes.lock().expect("fixture registry").push(Arc::clone(&probe));
        Ok(Box::new(PreflightProcessor {
            inner: HandoffProcessor {
                probe,
                // Reserve room for enter_state's actual serial increment.
                object_serial: NonZeroU64::new((ordinal as u64 + 1) * 32)
                    .expect("bounded distinct owned object"),
            },
            registry: Arc::clone(&self.0),
        }))
    }
}
impl AudioProcessor for PreflightProcessor {
    fn bind_native_child_settlement(
        &mut self,
        _: &mondrian_core::NativeChildSettlementRegistrar,
    ) -> Result<
        mondrian_core::NativeChildSettlementBinding,
        mondrian_core::NativeChildSettlementError,
    > {
        Ok(mondrian_core::NativeChildSettlementBinding::NotApplicable)
    }

    fn native_observations_supported(&self) -> bool {
        self.inner.native_observations_supported()
    }
    fn native_audio_object_observation(
        &mut self,
    ) -> Result<Option<NativeAudioObjectObservation>, AudioProcessorHostError> {
        self.registry.calls.observations.fetch_add(1, Ordering::Relaxed);
        self.inner.native_audio_object_observation()
    }
    fn poll_native_observations(
        &mut self,
        consumption: NativeAudioObservationConsumption,
    ) -> Result<NativeAudioObservations, AudioProcessorHostError> {
        self.registry.calls.observations.fetch_add(1, Ordering::Relaxed);
        self.inner.poll_native_observations(consumption)
    }
    fn bind_native_audio_object(
        &mut self,
        request: NativeAudioObjectBindRequest,
    ) -> Result<NativeAudioObjectBindOutcome, AudioProcessorHostError> {
        self.registry.calls.binds.fetch_add(1, Ordering::Relaxed);
        self.inner.bind_native_audio_object(request)
    }
    fn enter_state(&mut self, start: i64) -> Result<(), AudioProcessorHostError> {
        self.inner.enter_state(start)
    }
    fn process(
        &mut self,
        context: AudioProcessorProcessContext,
        audio: &mut dyn AudioProcessorAudioIo,
        parameters: AudioParameterEventBatch<'_>,
    ) -> Result<(), AudioProcessorHostError> {
        self.inner.process(context, audio, parameters)
    }
    fn open_native_audio_editor(
        &mut self,
        request: NativeAudioEditorOpenRequest,
    ) -> Result<NativeAudioEditorOutcome, AudioProcessorHostError> {
        self.inner.open_native_audio_editor(request)
    }
    fn close_native_audio_editor(
        &mut self,
        binding: NativeAudioObjectBindRequest,
    ) -> Result<NativeAudioEditorOutcome, AudioProcessorHostError> {
        self.inner.close_native_audio_editor(binding)
    }
}

#[derive(Clone, Copy)]
enum PreflightPlacement {
    Output,
    ScopeTwoContributions,
    NestedTwoPlacements,
}
struct PreflightSpec {
    canonical: Sequence,
    root: Sequence,
    dependencies: Vec<Sequence>,
    instance: mondrian_core::AudioProcessorInstanceId,
    placement: PreflightPlacement,
}
fn preflight_spec(placement: PreflightPlacement) -> PreflightSpec {
    use mondrian_core::automation::{ParameterSchema, PropertyValue};
    let mut canonical = sequence_with_audio_clip();
    let parameter = ParameterId::new_static("native.gain");
    let mut processor = AudioProcessorInstance {
        id: mondrian_core::AudioProcessorInstanceId::new(),
        definition: mondrian_timeline::AudioProcessorDefinitionRef::Clap {
            plugin_id: "test.mondrian.handoff".to_owned(),
            schema_version: 1,
            binary_sha256: None,
        },
        bypassed: false,
        parameters: Default::default(),
        opaque_state: None,
    };
    processor.parameters.insert(
        parameter.clone(),
        mondrian_timeline::audio::AudioProcessorParameter::from_schema(ParameterSchema::v1(
            parameter.clone(),
            PropertyValue::Double(0.5),
        ))
        .expect("owned schema"),
    );
    processor
        .set_parameter_automation(
            ExactAutomationCurve::new(parameter, 0.5).expect("initial constant author curve"),
        )
        .expect("schema-checked curve");
    let instance = processor.id;
    if matches!(placement, PreflightPlacement::ScopeTwoContributions) {
        let scope = canonical.audio_tracks[0].clips[0].audio_components[0].processing.scope_id;
        canonical
            .audio_program
            .processing_scopes
            .iter_mut()
            .find(|item| item.id == scope)
            .expect("actual first Scope")
            .processors
            .processors
            .push(processor);
        let track = canonical.audio_tracks[0].id;
        let second = canonical
            .add_media_audio_clip(
                track,
                Clip::new(AssetId::new(), TimelineTime::ZERO, tt(4, 1)).expect("second media Clip"),
                AudioSourceComponentId::primary(),
            )
            .expect("second actual Contribution");
        canonical.audio_tracks[0]
            .clips
            .iter_mut()
            .find(|clip| clip.id == second)
            .expect("second Clip")
            .audio_components[0]
            .processing
            .scope_id = scope;
        canonical.audio_program.compact_for_tracks(&canonical.audio_tracks);
    } else {
        canonical.audio_program.outputs[0].strip.pre_fader.processors.push(processor);
    }
    let (root, dependencies) = if matches!(placement, PreflightPlacement::NestedTwoPlacements) {
        let mut root = Sequence::new("whole-tree prospective author policy");
        root.settings.audio_channel_layout = AudioChannelLayout::Mono;
        let track = root.audio_tracks[0].id;
        for name in ["first distinct child", "second distinct child"] {
            root.add_nested_audio_clip(
                track,
                Clip::new_nested_sequence(
                    canonical.id,
                    TimelineTime::ZERO,
                    tt(4, 1),
                    Some(name.into()),
                )
                .expect("nested Clip"),
                canonical.audio_program.outputs[0].id,
            )
            .expect("nested canonical placement");
        }
        (root, vec![canonical.clone()])
    } else {
        (canonical.clone(), Vec::new())
    };
    PreflightSpec { canonical, root, dependencies, instance, placement }
}
fn preflight_contract() -> AudioRenderContract {
    AudioRenderContract {
        sample_rate: 2,
        channel_layout: AudioChannelLayout::Mono,
        max_block_frames: 1,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes:
            AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
        public_output_lookahead_budget_frames:
            AudioRenderContract::DEFAULT_PUBLIC_OUTPUT_LOOKAHEAD_BUDGET_FRAMES,
        compensation_delay_scratch_budget_bytes:
            AudioRenderContract::DEFAULT_COMPENSATION_DELAY_SCRATCH_BUDGET_BYTES,
    }
}
#[derive(Debug, Default, PartialEq)]
struct PreflightCursorSet {
    sessions: Vec<AudioSessionCursorEvidence>,
    processors: Vec<AudioProcessorCursorEvidence>,
}
impl AudioContinuityEvidenceVisitor for PreflightCursorSet {
    fn session(&mut self, evidence: AudioSessionCursorEvidence) -> Result<(), AudioExecutionError> {
        self.sessions.push(evidence);
        Ok(())
    }
    fn processor(
        &mut self,
        evidence: AudioProcessorCursorEvidence,
    ) -> Result<(), AudioExecutionError> {
        self.processors.push(evidence);
        Ok(())
    }
}
fn preflight_cursors(runtime: &AudioProgramRuntime) -> PreflightCursorSet {
    let mut cursors = PreflightCursorSet::default();
    runtime.visit_continuity_evidence(&mut cursors).expect("actual retained tree");
    cursors
}
struct PreflightFixture {
    inner: HandoffFixture,
    registry: Arc<PreflightRegistry>,
}
fn preflight_build(spec: &PreflightSpec, logical: usize) -> PreflightFixture {
    preflight_build_with_pcm(
        spec,
        logical,
        if matches!(spec.placement, PreflightPlacement::Output) {
            0.5
        } else {
            1.0
        },
    )
}
fn preflight_build_with_pcm(
    spec: &PreflightSpec,
    logical: usize,
    expected_pcm: f32,
) -> PreflightFixture {
    let registry = Arc::new(PreflightRegistry::default());
    let mut runtime = AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
        &spec.root,
        &spec.dependencies,
        &RampResolver,
        &PreflightResolver(Arc::clone(&registry)),
        preflight_contract(),
        None,
        AudioRuntimeResourceGrant::new(3, 64 * 1024 * 1024, logical),
    )
    .expect("genuinely fitting original frozen Runtime grant");
    runtime
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(947),
            start_sample: 0,
        })
        .expect("actual Root entry");
    let mut pcm = [91.0];
    runtime
        .render_into(AudioRenderRequest { transport_activity: None, start_sample: 0, frames: 1 }, &mut pcm)
        .expect("each actual occurrence entered once");
    assert_eq!(pcm, [expected_pcm]);
    let source = mondrian_core::AudioOwnerInstallationSource::new();
    let (registration, verifier) = source.register().expect("actual registration");
    runtime
        .bind_native_owner_installation(verifier)
        .expect("actual Runtime verifier");
    let claim = source
        .begin_commit(source.reserve_install(&registration, None).expect("initial reserve"))
        .expect("actual Source CAS")
        .commit()
        .expect("initial claim");
    runtime
        .install_native_owner_bindings(&claim, &ExecutionCancellationToken::new())
        .expect("distinct owned native bindings");
    let root = runtime.resource_root_identity();
    let cursors = preflight_cursors(&runtime);
    let cursor = cursors
        .processors
        .iter()
        .find(|cursor| cursor.occurrence.instance_id == spec.instance)
        .expect("real selected occurrence");
    let epoch = match cursor.state {
        AudioProcessorCursorState::Active { epoch, .. } => epoch,
        other => panic!("fixture did not enter its actual occurrence: {other:?}"),
    };
    let target = runtime
        .native_owner_lease(AudioNativeProcessorAddress {
            root,
            local: cursor.identity,
            epoch: Some(epoch),
            occurrence: cursor.occurrence,
        })
        .expect("actual installed owner lease")
        .editor_target();
    let probe = registry
        .probes
        .lock()
        .expect("independent probes")
        .iter()
        .find(|probe| {
            probe
                .lock()
                .expect("owned probe")
                .binding
                .is_some_and(|binding| binding.binding == target.binding)
        })
        .cloned()
        .expect("selected actual object");
    assert!(matches!(
        runtime
            .control_native_editor(
                Intent::Open {
                    target,
                    title: mondrian_core::NativeAudioEditorTitle::new("prospective policy".into())
                        .unwrap(),
                },
                &ExecutionCancellationToken::new()
            )
            .unwrap(),
        Outcome::Opened { .. }
    ));
    PreflightFixture {
        inner: HandoffFixture {
            sequence: spec.canonical.clone(),
            runtime,
            source,
            probe,
            target,
            attachment: target,
        },
        registry,
    }
}
fn preflight_programs(fixture: &mut PreflightFixture, value: f64) -> [NativeAudioAuthorProgram; 1] {
    let target = fixture.inner.target;
    let processor = match target.owner {
        mondrian_core::NativeAudioEditorOccurrenceOwner::Output(_) => {
            &mut fixture.inner.sequence.audio_program.outputs[0].strip.pre_fader.processors[0]
        }
        mondrian_core::NativeAudioEditorOccurrenceOwner::Contribution { scope_id, .. } => {
            &mut fixture
                .inner
                .sequence
                .audio_program
                .processing_scopes
                .iter_mut()
                .find(|scope| scope.id == scope_id)
                .unwrap()
                .processors
                .processors[0]
        }
        other => panic!("fixture owner unexpectedly changed: {other:?}"),
    };
    processor
        .set_parameter_automation(
            ExactAutomationCurve::new(ParameterId::new_static("native.gain"), value).unwrap(),
        )
        .unwrap();
    [NativeAudioAuthorProgram {
        sequence: fixture.inner.sequence.id,
        program: Arc::new(
            compile_audio_program(
                &fixture.inner.sequence,
                AudioCompileRequest::program(fixture.inner.sequence.audio_program.outputs[0].id),
            )
            .unwrap(),
        ),
    }]
}
fn preflight_selected_weight(fixture: &PreflightFixture) -> usize {
    let target = fixture.inner.target;
    fixture
        .inner
        .runtime
        .find_resource_session(AudioSessionCursorIdentity {
            prepared_plan_id: target.local_plan,
            session_id: target.local_session,
        })
        .unwrap()
        .resource_plan()
        .session_resource_footprint()
        .unwrap()
        .prepared_logical_bytes
}
fn preflight_install(
    fixture: &mut PreflightFixture,
    proposal: &mondrian_core::NativeAudioAuthorProposal,
    programs: &[NativeAudioAuthorProgram],
) {
    assert!(!proposal.terminal());
    assert!(proposal.state().is_none());
    assert!(
        !fixture.inner.runtime.native_author_state_entry_required(proposal).unwrap(),
        "this ordinary test must take the unchanged Preserved branch"
    );
    let old_footprint = fixture.inner.runtime.resource_footprint();
    let old_cursors = preflight_cursors(&fixture.inner.runtime);
    let old_creates = fixture.registry.calls.creates.load(Ordering::Relaxed);
    let old_objects: Vec<_> = fixture
        .registry
        .probes
        .lock()
        .unwrap()
        .iter()
        .map(|probe| {
            let probe = probe.lock().unwrap();
            (probe.entries, probe.binding.unwrap().expected_object)
        })
        .collect();
    let (registration, destination) = fixture.inner.source.register().unwrap();
    let old = fixture.inner.source.current_claim().unwrap().unwrap();
    let reservation = fixture.inner.source.reserve_install(&registration, Some(&old)).unwrap();
    let mut authorization =
        fixture.inner.source.authorize_runtime_installation(reservation).unwrap();
    let mut prepared = fixture
        .inner
        .runtime
        .prepare_native_author_parameter_handoff(
            programs,
            proposal,
            destination,
            &ExecutionCancellationToken::new(),
        )
        .expect("same existing overlap and native gates");
    assert_eq!(
        fixture.inner.runtime.resource_footprint(),
        old_footprint,
        "preparation must not publish a pending or replacement footprint"
    );
    let prepared_cursors = preflight_cursors(&fixture.inner.runtime);
    assert_eq!(
        prepared_cursors.sessions, old_cursors.sessions,
        "preparation leaves every installed Plan and Session unchanged"
    );
    assert_eq!(prepared_cursors.processors, old_cursors.processors);
    assert_eq!(
        fixture.inner.source.current_claim().unwrap().unwrap().generation(),
        old.generation()
    );
    let receipt = fixture
        .inner
        .runtime
        .install_native_author_parameter_handoff(
            &mut prepared,
            &mut authorization,
            &ExecutionCancellationToken::new(),
        )
        .expect("existing same-object final install");
    assert_eq!(
        receipt.continuity,
        NativeAudioParameterHandoffContinuity::Preserved
    );
    assert_eq!(
        fixture.registry.calls.creates.load(Ordering::Relaxed),
        old_creates,
        "parameter preparation must not create substitute processors"
    );
    let current_objects: Vec<_> = fixture
        .registry
        .probes
        .lock()
        .unwrap()
        .iter()
        .map(|probe| {
            let probe = probe.lock().unwrap();
            (probe.entries, probe.binding.unwrap().expected_object)
        })
        .collect();
    assert_eq!(
        current_objects, old_objects,
        "all actual object incarnations/entry counts survive"
    );
    let next_cursors = preflight_cursors(&fixture.inner.runtime);
    assert_eq!(next_cursors.sessions.len(), old_cursors.sessions.len());
    assert_eq!(next_cursors.processors.len(), old_cursors.processors.len());
    for old in &old_cursors.sessions {
        let next = next_cursors
            .sessions
            .iter()
            .find(|next| next.identity.session_id == old.identity.session_id)
            .expect("same actual Session");
        assert_eq!(
            next.state, old.state,
            "same epoch, public and execution cursors"
        );
    }
    for old in &old_cursors.processors {
        let next = next_cursors
            .processors
            .iter()
            .find(|next| {
                next.identity.session_id == old.identity.session_id
                    && next.occurrence == old.occurrence
            })
            .expect("same actual processor occurrence");
        assert_eq!(
            next.state, old.state,
            "no Enter/reset or DSP prefix advance during handoff"
        );
    }
    fixture.inner.target = receipt.target;
    drop(prepared);
    fixture.inner.ack(proposal);
}
fn preflight_assert_early_denial(
    fixture: &mut PreflightFixture,
    proposal: &mondrian_core::NativeAudioAuthorProposal,
    programs: &[NativeAudioAuthorProgram],
    required: usize,
    granted: usize,
) {
    assert!(!proposal.terminal());
    assert!(proposal.state().is_none());
    assert!(
        !fixture.inner.runtime.native_author_state_entry_required(proposal).unwrap(),
        "repeated Scope/nested occurrences do not qualify StateEntry here"
    );
    let (_, destination) = fixture.inner.source.register().unwrap();
    let cancellation = ExecutionCancellationToken::new();
    let old_root = fixture.inner.runtime.resource_root_identity();
    let old_footprint = fixture.inner.runtime.resource_footprint();
    let old_claim = fixture.inner.source.current_claim().unwrap().unwrap();
    let calls = fixture.registry.calls.snapshot();
    // Every DTO, compiler owner, registration and assertion input is prebuilt.
    let (result, allocations) = measure_host_allocations_for_test(|| {
        fixture.inner.runtime.prepare_native_author_parameter_handoff(
            programs,
            proposal,
            destination,
            &cancellation,
        )
    });
    assert_eq!(
        allocations, [0; 4],
        "public prepare must refuse before any local deep clone/scratch"
    );
    assert!(
        matches!(result, Err(NativeAudioParameterHandoffError::Resources(
        AudioRuntimeBuildError::ResourceOverlapGrantExceeded {
            category: AudioRuntimeResourceCategory::PreparedLogicalBytes,
            required: actual_required, granted: actual_granted,
        }
    )) if actual_required == required && actual_granted == granted)
    );
    assert_eq!(
        fixture.registry.calls.snapshot(),
        calls,
        "no object observation, Bind or factory creation"
    );
    assert_eq!(fixture.inner.runtime.resource_root_identity(), old_root);
    assert_eq!(fixture.inner.runtime.resource_footprint(), old_footprint);
    let claim = fixture.inner.source.current_claim().unwrap().unwrap();
    assert_eq!(claim.generation(), old_claim.generation());
    let mut untouched = [93.0];
    assert!(matches!(
        fixture.inner.runtime.render_into(
            AudioRenderRequest { transport_activity: None, start_sample: 1, frames: 1 },
            &mut untouched,
        ),
        Err(AudioExecutionError::NativeAuthorPending(_))
    ));
    assert_eq!(untouched, [93.0]);
}

#[test]
fn native_author_preflight_root_refuses_before_clone_or_native_binding() {
    let spec = preflight_spec(PreflightPlacement::Output);
    let baseline = preflight_build(&spec, usize::MAX);
    let current = baseline.inner.runtime.resource_footprint().prepared_logical_bytes;
    let required = current.checked_add(preflight_selected_weight(&baseline)).unwrap();
    drop(baseline);
    let mut fixture = preflight_build(&spec, required - 1);
    let proposal = fixture.inner.value(0.75);
    let programs = preflight_programs(&mut fixture, 0.75);
    preflight_assert_early_denial(&mut fixture, &proposal, &programs, required, required - 1);
}

#[test]
fn native_author_preflight_whole_nested_tree_refuses_before_first_local_clone() {
    let spec = preflight_spec(PreflightPlacement::NestedTwoPlacements);
    let baseline = preflight_build(&spec, usize::MAX);
    assert_eq!(
        baseline.inner.runtime.resource_footprint().runtime_occurrences,
        3
    );
    let current = baseline.inner.runtime.resource_footprint().prepared_logical_bytes;
    let local = preflight_selected_weight(&baseline);
    let required = current.checked_add(local.checked_mul(2).unwrap()).unwrap();
    // Each child alone fits; the original Root closure is added exactly once.
    assert!(current + local < required);
    drop(baseline);
    let mut fixture = preflight_build(&spec, required - 1);
    let proposal = fixture.inner.value(0.75);
    let programs = preflight_programs(&mut fixture, 0.75);
    preflight_assert_early_denial(&mut fixture, &proposal, &programs, required, required - 1);
}

#[test]
fn native_author_preflight_nested_exact_grant_preserves_final_footprint_and_pcm() {
    let spec = preflight_spec(PreflightPlacement::NestedTwoPlacements);
    let mut baseline = preflight_build(&spec, usize::MAX);
    let initial = baseline.inner.runtime.resource_footprint();
    let required = initial.prepared_logical_bytes + 2 * preflight_selected_weight(&baseline);
    let control_proposal = baseline.inner.value(0.75);
    let control_programs = preflight_programs(&mut baseline, 0.75);
    preflight_install(&mut baseline, &control_proposal, &control_programs);
    let accepted_final = baseline.inner.runtime.resource_footprint();
    drop(baseline);
    let mut fixture = preflight_build(&spec, required);
    let proposal = fixture.inner.value(0.75);
    let programs = preflight_programs(&mut fixture, 0.75);
    preflight_install(&mut fixture, &proposal, &programs);
    assert_eq!(
        fixture.inner.runtime.resource_footprint(),
        accepted_final,
        "same actual final overlap publication as the generous-grant control"
    );
    assert_eq!(
        accepted_final.prepared_logical_bytes,
        initial.prepared_logical_bytes
    );
    let mut pcm = [93.0];
    fixture
        .inner
        .runtime
        .render_into(AudioRenderRequest { transport_activity: None, start_sample: 1, frames: 1 }, &mut pcm)
        .unwrap();
    assert_eq!(
        pcm,
        [3.0],
        "two actual child instances consume .75 exactly once"
    );
}

#[test]
fn native_author_preflight_scope_two_contributions_exact_grant_preserves_policy() {
    let spec = preflight_spec(PreflightPlacement::ScopeTwoContributions);
    let mut baseline = preflight_build(&spec, usize::MAX);
    let initial = baseline.inner.runtime.resource_footprint();
    let local = preflight_selected_weight(&baseline);
    let summary = baseline
        .inner
        .runtime
        .find_resource_session(baseline.inner.runtime.resource_root_identity())
        .unwrap()
        .resource_plan()
        .schedule_summary();
    assert_eq!(
        summary.processor_occurrence_count, 2,
        "one canonical Scope has two actual occurrences"
    );
    assert_eq!(initial.runtime_occurrences, 1);
    assert_eq!(local, initial.prepared_logical_bytes);
    let control_proposal = baseline.inner.value(0.75);
    let control_programs = preflight_programs(&mut baseline, 0.75);
    preflight_install(&mut baseline, &control_proposal, &control_programs);
    let accepted_final = baseline.inner.runtime.resource_footprint();
    drop(baseline);
    let mut fixture = preflight_build(&spec, initial.prepared_logical_bytes + local);
    let proposal = fixture.inner.value(0.75);
    let programs = preflight_programs(&mut fixture, 0.75);
    preflight_install(&mut fixture, &proposal, &programs);
    assert_eq!(
        fixture.inner.runtime.resource_footprint(),
        accepted_final,
        "retain the original actual event/lane-capacity publication"
    );
    assert_eq!(
        accepted_final.prepared_logical_bytes,
        initial.prepared_logical_bytes
    );
    let mut pcm = [93.0];
    fixture
        .inner
        .runtime
        .render_into(AudioRenderRequest { transport_activity: None, start_sample: 1, frames: 1 }, &mut pcm)
        .unwrap();
    assert_eq!(
        pcm,
        [3.0],
        "shared Scope author data does not collapse actual execution occurrences"
    );
}

#[test]
fn native_author_preflight_unchanged_nested_peer_is_not_recharged() {
    let spec = preflight_spec(PreflightPlacement::NestedTwoPlacements);
    let mut baseline = preflight_build(&spec, usize::MAX);
    let initial = baseline.inner.runtime.resource_footprint();
    let local = preflight_selected_weight(&baseline);
    let control_proposal = baseline.inner.value(0.5);
    let control_programs = preflight_programs(&mut baseline, 0.5);
    preflight_install(&mut baseline, &control_proposal, &control_programs);
    let accepted_final = baseline.inner.runtime.resource_footprint();
    drop(baseline);
    let mut fixture = preflight_build(&spec, initial.prepared_logical_bytes + local);
    let old = preflight_cursors(&fixture.inner.runtime);
    let selected = fixture.inner.target.local_session;
    let proposal = fixture.inner.value(0.5);
    let programs = preflight_programs(&mut fixture, 0.5);
    let current = fixture
        .inner
        .runtime
        .find_resource_session(AudioSessionCursorIdentity {
            prepared_plan_id: fixture.inner.target.local_plan,
            session_id: selected,
        })
        .unwrap()
        .resource_plan();
    assert_eq!(
        current.program(),
        programs[0].program.as_ref(),
        "actual unchanged canonical Program"
    );
    preflight_install(&mut fixture, &proposal, &programs);
    let next = preflight_cursors(&fixture.inner.runtime);
    for prior in &old.sessions {
        let now = next
            .sessions
            .iter()
            .find(|entry| entry.identity.session_id == prior.identity.session_id)
            .unwrap();
        if prior.identity.session_id == selected {
            assert_ne!(
                now.identity.prepared_plan_id,
                prior.identity.prepared_plan_id
            );
        } else {
            assert_eq!(
                now.identity, prior.identity,
                "unchanged peers and Root keep their exact Plan"
            );
        }
    }
    assert_eq!(fixture.inner.runtime.resource_footprint(), accepted_final);
    let mut pcm = [93.0];
    fixture
        .inner
        .runtime
        .render_into(AudioRenderRequest { transport_activity: None, start_sample: 1, frames: 1 }, &mut pcm)
        .unwrap();
    assert_eq!(pcm, [2.0]);
}

#[test]
fn native_author_preflight_selected_equal_scope_still_charges_one_actual_plan() {
    let spec = preflight_spec(PreflightPlacement::ScopeTwoContributions);
    let baseline = preflight_build(&spec, usize::MAX);
    let current = baseline.inner.runtime.resource_footprint().prepared_logical_bytes;
    assert_eq!(preflight_selected_weight(&baseline), current);
    drop(baseline);
    let mut fixture = preflight_build(&spec, current * 2 - 1);
    let proposal = fixture.inner.value(0.5);
    let programs = preflight_programs(&mut fixture, 0.5);
    let current_plan = fixture
        .inner
        .runtime
        .find_resource_session(fixture.inner.runtime.resource_root_identity())
        .unwrap()
        .resource_plan();
    assert_eq!(current_plan.program(), programs[0].program.as_ref());
    preflight_assert_early_denial(
        &mut fixture,
        &proposal,
        &programs,
        current * 2,
        current * 2 - 1,
    );
}

#[path = "native_author_held_handles.rs"]
mod held_handle_controls;

#[path = "native_author_nested_charge.rs"]
mod nested_charge_controls;
#[path = "native_author_scope_repeat.rs"]
mod scope_repeat_controls;

#[path = "native_author_held_fee_public.rs"]
mod held_fee_public_controls;

#[test]
fn vst_rescan_selected_singleton_preserves_all_native_peer_admission_guards() {
    use super::continuing_rescan_controls::{hold, install, prepare, rescan};
    let mut spec = preflight_spec(PreflightPlacement::Output);
    let rack = &mut spec.canonical.audio_program.outputs[0].strip.pre_fader;
    rack.processors[0].definition = mondrian_timeline::AudioProcessorDefinitionRef::Vst3 {
        class_id: "portable.actual.rescan".into(),
        vendor: None,
        schema_version: 1,
        binary_sha256: None,
    };
    let mut peer = rack.processors[0].clone();
    peer.id = mondrian_core::AudioProcessorInstanceId::new();
    rack.processors.insert(0, peer);
    spec.root = spec.canonical.clone();
    let mut f = preflight_build_with_pcm(&spec, 64 * 1024 * 1024, 0.25);
    let selected = Arc::clone(&f.inner.probe);
    let peer = f
        .registry
        .probes
        .lock()
        .unwrap()
        .iter()
        .find(|probe| !Arc::ptr_eq(probe, &selected))
        .unwrap()
        .clone();
    let a = crate::NativeRequestCohort::new(
        161,
        mondrian_core::NativeAudioReprepareRequest::Vst3Restart { flags: 4 },
    )
    .unwrap();
    selected.lock().unwrap().live_request = Some(crate::NativeObservedRequest::Cohort(a));
    hold(&mut f.inner);
    // This Root polls one actual producer per call. The inserted empty peer is
    // first; observing it must not drain or alter the selected source.
    let root = f.inner.runtime.resource_root_identity();
    assert_eq!(
        f.inner
            .runtime
            .find_resource_session(root)
            .unwrap()
            .native_observation_sources(),
        2
    );
    let snapshots = |registry: &PreflightRegistry| {
        registry
            .probes
            .lock()
            .unwrap()
            .iter()
            .map(|owned| {
                let p = owned.lock().unwrap();
                (
                    (
                        p.binding,
                        p.revision,
                        p.open,
                        p.retained,
                        p.request_cohort,
                        p.reported_request_cohort,
                    ),
                    (
                        p.pending.clone(),
                        p.configuration.clone(),
                        p.retained_configuration.clone(),
                        p.accepted_seed.clone(),
                        p.catchup_pending,
                        p.catchup_polls,
                        p.source_failure,
                    ),
                    (
                        p.entries,
                        p.processed.clone(),
                        p.opens,
                        p.closes,
                        p.creates,
                        p.binds,
                        p.ack_attempts,
                        p.accepted_acks,
                    ),
                    (
                        p.vst_rescan_abi,
                        p.live_request,
                        p.live_after_ack,
                        p.parameter_only,
                    ),
                )
            })
            .collect::<Vec<_>>()
    };
    let queued = snapshots(&f.registry);
    let old_cursors = preflight_cursors(&f.inner.runtime);
    let old_claim = f.inner.source.current_claim().unwrap().unwrap();
    let old_verifier = f.inner.runtime.native_owner_installation_verifier().unwrap();
    let old_footprint = f.inner.runtime.resource_footprint();
    let old_grant = f.inner.runtime.resource_grant();
    let calls = f.registry.calls.snapshot();
    assert!(f
        .inner
        .runtime
        .poll_native_observations(&ExecutionCancellationToken::new())
        .expect("actual peer errors cannot become an empty completion")
        .is_none());
    assert_eq!(
        f.registry.calls.snapshot(),
        [calls[0].checked_add(1).unwrap(), calls[1], calls[2]]
    );
    assert_eq!(snapshots(&f.registry), queued);
    assert_eq!(f.inner.runtime.resource_root_identity(), root);
    assert_eq!(f.inner.runtime.resource_footprint(), old_footprint);
    assert_eq!(f.inner.runtime.resource_grant(), old_grant);
    assert_eq!(preflight_cursors(&f.inner.runtime), old_cursors);
    let unchanged = f.inner.source.current_claim().unwrap().unwrap();
    old_verifier.validate(&unchanged).unwrap();
    assert_eq!(unchanged.generation(), old_claim.generation());
    let proposal = rescan(&mut f.inner, a);
    let programs = super::continuing_rescan_controls::programs(&mut f.inner, &proposal);
    let before = preflight_cursors(&f.inner.runtime);
    let binds = f.registry.calls.binds.load(Ordering::Relaxed);
    let creates = f.registry.calls.creates.load(Ordering::Relaxed);
    for source_failure in [true, false] {
        {
            let mut p = peer.lock().unwrap();
            p.source_failure =
                source_failure.then_some(crate::NativeAudioAuthorSourceFailure::InvalidSource);
            p.live_request =
                (!source_failure).then_some(crate::NativeObservedRequest::RequestOnly(
                    mondrian_core::NativeAudioReprepareRequest::ClapRestart,
                ));
        }
        let (_, destination) = f.inner.source.register().unwrap();
        let error = f
            .inner
            .runtime
            .prepare_native_author_parameter_handoff(
                &programs,
                &proposal,
                destination,
                &ExecutionCancellationToken::new(),
            )
            .err()
            .expect("peer hold must not be borrowed from selected A");
        let expected = if source_failure {
            NativeAudioAuthorHoldReason::SourceFailure(
                crate::NativeAudioAuthorSourceFailure::InvalidSource,
            )
        } else {
            NativeAudioAuthorHoldReason::PendingCallbacks
        };
        assert!(
            matches!(error,NativeAudioParameterHandoffError::Admission(NativeAudioAuthorAdmissionError::Pending{reason,..}) if reason==expected),
            "actual={error:?}"
        );
        assert_eq!(f.registry.calls.binds.load(Ordering::Relaxed), binds);
        assert_eq!(f.registry.calls.creates.load(Ordering::Relaxed), creates);
        assert_eq!(preflight_cursors(&f.inner.runtime), before);
    }
    {
        let mut p = peer.lock().unwrap();
        p.source_failure = None;
        p.live_request = None;
    }
    let (mut prepared, mut authorization) = prepare(&mut f.inner, &proposal);
    install(&mut f.inner, &proposal, &mut prepared, &mut authorization);
    f.inner.ack(&proposal);
    let mut pcm = [91.0];
    f.inner
        .runtime
        .render_into(AudioRenderRequest { transport_activity: None, start_sample: 1, frames: 1 }, &mut pcm)
        .unwrap();
    assert_eq!(
        pcm,
        [0.75],
        "same two processors, selected new .75 and unchanged peer .5"
    );
    assert_eq!(f.registry.calls.creates.load(Ordering::Relaxed), creates);
    assert_eq!(f.registry.calls.binds.load(Ordering::Relaxed), binds + 2);
    assert_eq!(selected.lock().unwrap().entries, 1);
    assert_eq!(peer.lock().unwrap().entries, 1);
    drop(prepared);
}

#[path = "native_author_terminal_batch.rs"]
mod terminal_batch_controls;
