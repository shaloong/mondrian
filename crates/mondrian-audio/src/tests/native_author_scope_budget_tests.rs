// Actual Windows VST3 whole-tree budget denial before local clone/storage.
// TLS measures this host call; no SDK/IPC call counter is asserted here.

use super::*;
use crate::processor_isolation::measure_host_allocations_for_test;

#[derive(Default)]
struct BudgetCurrentSessions {
    identities: Vec<crate::AudioSessionCursorIdentity>,
}
impl AudioContinuityEvidenceVisitor for BudgetCurrentSessions {
    fn session(&mut self, evidence: AudioSessionCursorEvidence) -> Result<(), AudioExecutionError> {
        self.identities.push(evidence.identity);
        Ok(())
    }
    fn processor(
        &mut self,
        _evidence: AudioProcessorCursorEvidence,
    ) -> Result<(), AudioExecutionError> {
        Ok(())
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
struct BudgetCursors {
    sessions: Vec<AudioSessionCursorEvidence>,
    processors: Vec<AudioProcessorCursorEvidence>,
}
impl AudioContinuityEvidenceVisitor for BudgetCursors {
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
fn budget_cursors(runtime: &AudioProgramRuntime) -> BudgetCursors {
    let mut evidence = BudgetCursors::default();
    runtime
        .visit_continuity_evidence(&mut evidence)
        .expect("actual whole-tree cursors");
    evidence
}

fn budget_sum(mut weights: impl Iterator<Item = usize>) -> usize {
    weights
        .try_fold(0_usize, |sum, weight| sum.checked_add(weight))
        .expect("actual finite logical weights")
}
fn budget_current_claim(
    runtime: &AudioProgramRuntime,
    source: &mondrian_core::AudioOwnerInstallationSource,
    expected: &mondrian_core::AudioOwnerInstallationClaim,
) {
    let current = source.current_claim().expect("actual Source").expect("installed slot");
    assert_eq!(current.generation(), expected.generation());
    let verifier = runtime
        .native_owner_installation_verifier()
        .expect("actual same Runtime Source");
    verifier.validate(&current).expect("actual current claim");
    verifier.validate(expected).expect("same original claim");
}
macro_rules! budget_observe {
    ($runtime:expr) => {{
        let observation = ($runtime)
            .observe_state_entry_fees_for_test()
            .expect("borrow actual retained cohort and Root policy");
        assert_eq!(
            observation.held_logical_bytes,
            ($runtime)
                .check_resource_ceiling(
                    crate::AudioRuntimeResourceCategory::PreparedLogicalBytes,
                    0,
                )
                .expect("same authoritative logical checker")
        );
        assert_eq!(observation.root, ($runtime).resource_root_identity());
        assert_eq!(observation.published, ($runtime).resource_footprint());
        assert_eq!(observation.grant, ($runtime).resource_grant());
        observation
    }};
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BudgetWeights {
    current: usize,
    next: usize,
    old_locals: Vec<usize>,
    next_locals: Vec<(bool, usize)>,
}

fn run_scope_budget_case(
    nested: bool,
    logical_grant: usize,
    expected: Option<&BudgetWeights>,
) -> BudgetWeights {
    let cleanup_retry = true;

    use mondrian_core::NativeAudioAuthorCallback as Callback;
    let helper = std::env::var_os("MONDRIAN_VST3_TEST_HELPER")
        .map(PathBuf::from)
        .expect("fresh matching product helper");
    let binary = std::env::var_os("MONDRIAN_VST3_EDITOR_TEST_PLUGIN")
        .map(PathBuf::from)
        .expect("rebuilt owned real IPlugView fixture");
    let catalog = Arc::new(
        DiscoveredVst3AudioProcessorSpecResolver::discover(helper, [binary])
            .expect("actual exact native catalog"),
    );
    let descriptors = catalog.descriptors();
    assert_eq!(descriptors.len(), 1, "owned exact one native class");
    let contract = AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Stereo,
        max_block_frames: 4,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes: 128 * 1024 * 1024,
        public_output_lookahead_budget_frames:
            AudioRenderContract::DEFAULT_PUBLIC_OUTPUT_LOOKAHEAD_BUDGET_FRAMES,
        compensation_delay_scratch_budget_bytes:
            AudioRenderContract::DEFAULT_COMPENSATION_DELAY_SCRATCH_BUDGET_BYTES,
    };
    let processor = catalog
        .create_instance(
            &descriptors[0].class_id,
            contract,
            Some({
                let mut state = scope_initial_fixture_state();
                if cleanup_retry {
                    for offset in [52, 84] {
                        state[offset..offset + 4].copy_from_slice(&2u32.to_le_bytes());
                    }
                }
                state
            }),
        )
        .expect("genuine native schema/state probe");
    let instance_id = processor.id;
    let mut canonical = sequence_with_audio_clip();
    canonical.settings.audio_channel_layout = AudioChannelLayout::Stereo;
    let scope_id = canonical.audio_tracks[0].clips[0].audio_components[0].processing.scope_id;
    canonical
        .audio_program
        .processing_scopes
        .iter_mut()
        .find(|scope| scope.id == scope_id)
        .expect("canonical Scope")
        .processors
        .processors
        .push(processor);
    if !nested {
        let track = canonical.audio_tracks[0].id;
        let clip = Clip::new(AssetId::new(), TimelineTime::ZERO, tt(4, 1)).expect("second Clip");
        let id = canonical
            .add_media_audio_clip(track, clip, AudioSourceComponentId::primary())
            .expect("second canonical Contribution");
        canonical.audio_tracks[0]
            .clips
            .iter_mut()
            .find(|clip| clip.id == id)
            .expect("actual second Clip")
            .audio_components[0]
            .processing
            .scope_id = scope_id;
        canonical.audio_program.compact_for_tracks(&canonical.audio_tracks);
    }
    let (root, dependencies) = if nested {
        let mut root = Sequence::new("two simultaneous native child placements");
        root.settings.audio_channel_layout = AudioChannelLayout::Stereo;
        let track = root.audio_tracks[0].id;
        for name in ["first actual child", "second actual child"] {
            let clip = Clip::new_nested_sequence(
                canonical.id,
                TimelineTime::ZERO,
                tt(4, 1),
                Some(name.to_owned()),
            )
            .expect("exact nested placement");
            root.add_nested_audio_clip(track, clip, canonical.audio_program.outputs[0].id)
                .expect("canonical nested audio placement");
        }
        (root, vec![canonical.clone()])
    } else {
        (canonical.clone(), Vec::new())
    };
    let resolver = IsolatedAudioProcessorResolver::new(catalog);
    let mut runtime = AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
        &root,
        &dependencies,
        &ScopeUnityStereoResolver,
        &resolver,
        contract,
        None,
        AudioRuntimeResourceGrant::new(3, 512 * 1024 * 1024, logical_grant),
    )
    .expect("actual full retained Runtime tree");
    assert_eq!(
        runtime.resource_footprint().runtime_occurrences,
        if nested { 3 } else { 1 }
    );
    runtime
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(701),
            start_sample: 0,
        })
        .expect("one real root continuity before any GUI editing");
    let mut positive = [91.0; 8];
    runtime
        .render_into(
            AudioRenderRequest { start_sample: 0, frames: 4 },
            &mut positive,
        )
        .expect("both real native peers processed positive PCM");
    assert_eq!(
        positive, [1.0; 8],
        "two .5 native peer outputs, independently summed"
    );
    let source = mondrian_core::AudioOwnerInstallationSource::new();
    let (registration, verifier) = source.register().expect("actual slot registration");
    runtime
        .bind_native_owner_installation(verifier)
        .expect("same actual root verifier");
    let claim = source
        .begin_commit(
            source.reserve_install(&registration, None).expect("initial slot reservation"),
        )
        .expect("actual root linearization")
        .commit()
        .expect("initial exact installed claim");
    runtime
        .install_native_owner_bindings(&claim, &ExecutionCancellationToken::new())
        .expect("same actual children binding acknowledgements");
    let root_identity = runtime.resource_root_identity();
    let mut observed = ScopeActualCursorVisitor {
        instance: instance_id,
        processors: Vec::with_capacity(2),
    };
    runtime
        .visit_continuity_evidence(&mut observed)
        .expect("sole actual occurrence/epoch observation");
    assert_eq!(observed.processors.len(), 2);
    if nested {
        assert_ne!(
            observed.processors[0].identity.session_id,
            observed.processors[1].identity.session_id
        );
    } else {
        assert_eq!(
            observed.processors[0].identity.session_id,
            observed.processors[1].identity.session_id
        );
        assert_ne!(
            observed.processors[0].occurrence.owner,
            observed.processors[1].occurrence.owner
        );
    }
    let mut targets = Vec::with_capacity(2);
    let mut parents = Vec::with_capacity(2);
    let mut process_ids = Vec::with_capacity(2);
    for cursor in &observed.processors {
        let epoch = match cursor.state {
            AudioProcessorCursorState::Active { epoch, next_input_signal_sample } => {
                assert_eq!(next_input_signal_sample.sample(), 4);
                epoch
            }
            other => {
                panic!("actual first PCM must enter every lazy native occurrence: {other:?}")
            }
        };
        let lease = runtime
            .native_owner_lease(AudioNativeProcessorAddress {
                root: root_identity,
                local: cursor.identity,
                epoch: Some(epoch),
                occurrence: cursor.occurrence,
            })
            .expect("only actual current Hosted record issues this lease");
        let target = lease.editor_target();
        let title = format!("Scope peer qualification {}", target.binding);
        assert!(matches!(runtime.control_native_editor(Intent::Open {
                target, title: mondrian_core::NativeAudioEditorTitle::new(title.clone()).expect("bounded native title"),
            }, &ExecutionCancellationToken::new()).expect("actual same installed owner open"),
                Outcome::Opened { target: opened, .. } if opened == target));
        let parent = scope_real_parent(&title);
        let mut process_id = 0;
        assert_ne!(
            unsafe { scope_window_process_id(parent, &mut process_id) },
            0
        );
        assert_ne!(process_id, 0);
        targets.push(target);
        parents.push(parent);
        process_ids.push(process_id);
    }
    assert_ne!(parents[0], parents[1], "two actual owned views");
    assert_ne!(
        process_ids[0], process_ids[1],
        "two genuine isolated child instances, not one mirrored controller"
    );
    let class = scope_wide("MondrianOwnedVst3EditorFixture");
    let view = unsafe {
        scope_find_window_ex_w(
            parents[0],
            std::ptr::null_mut(),
            class.as_ptr(),
            std::ptr::null(),
        )
    };
    assert!(!view.is_null(), "real selected native child view");
    assert_ne!(
        unsafe { scope_post_message_w(view, 0x0100, b'G' as usize, 0) },
        0,
        "genuine selected main-thread group callback"
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    let proposal = loop {
        assert!(
            Instant::now() < deadline,
            "actual Scope terminal callback delivery timed out"
        );
        if let Some(batch) = runtime
            .poll_native_observations(&ExecutionCancellationToken::new())
            .expect("same serial native observation consumer")
            && let Some(proposal) = batch.author
        {
            break proposal;
        }
        std::thread::yield_now();
    };
    assert_eq!(proposal.target(), targets[0]);
    assert_eq!(proposal.sequence(), canonical.id);
    assert_eq!(proposal.open_parameter_edits(), 0);
    assert_eq!(proposal.group_depth(), 0);
    assert!(
        proposal.terminal(),
        "actual closed native state, not a parameter-only prefix"
    );
    assert_eq!(proposal.callbacks().first(), Some(&Callback::GroupStarted));
    assert_eq!(
        proposal.callbacks().last(),
        Some(&Callback::DirtyChanged(false))
    );
    let state = proposal.state().expect("real coherent terminal bytes");
    assert_eq!(state.len(), 92);
    assert_eq!(&state[..16], b"VST3HOST_STATE\0\0");
    for part in [&state[28..60], &state[60..92]] {
        assert_eq!(&part[..4], b"MEV1");
        assert_eq!(
            f64::from_le_bytes(part[8..16].try_into().expect("gain extent")),
            0.75
        );
        assert_eq!(
            f64::from_le_bytes(part[16..24].try_into().expect("balance extent")),
            0.25
        );
    }
    let mut blocked = [91.0; 8];
    assert!(matches!(
        runtime.render_into(
            AudioRenderRequest { start_sample: 4, frames: 4 },
            &mut blocked
        ),
        Err(AudioExecutionError::NativeAuthorPending(_))
    ));
    assert_eq!(
        blocked, [91.0; 8],
        "no old constants/peer PCM while native author state is unaccepted"
    );
    let processor = &mut canonical
        .audio_program
        .processing_scopes
        .iter_mut()
        .find(|scope| scope.id == scope_id)
        .expect("same canonical Scope")
        .processors
        .processors[0];
    for (id, value) in [("vst3.param.0", 0.75), ("vst3.param.1", 0.25)] {
        let mut curve = ExactAutomationCurve::new(ParameterId::new_static(id), value)
            .expect("exact canonical authored scalar");
        if id == "vst3.param.0" {
            for index in 0..9 {
                curve
                    .set_keyframe(mondrian_core::ExactAutomationKeyframe::linear(
                        tt(index, 48_000),
                        value,
                    ))
                    .expect("genuine constant canonical Curve key");
            }
        }
        processor
            .set_parameter_automation(curve)
            .expect("native schema-validated curve");
    }
    processor.opaque_state = Some(state.clone());
    let programs = [NativeAudioAuthorProgram {
        sequence: canonical.id,
        program: Arc::new(
            compile_audio_program(
                &canonical,
                AudioCompileRequest::program(canonical.audio_program.outputs[0].id),
            )
            .expect("sole canonical next Program"),
        ),
    }];
    let (_next_registration, destination) =
        source.register().expect("actual next destination registration");
    eprintln!("SCOPE_BUDGET_NATIVE_PRIMARY: two distinct actual child PIDs {process_ids:?}, positive PCM and coherent92B terminal passed; nested={nested}");
    assert!(proposal.terminal());
    assert!(
        runtime.native_author_state_entry_required(&proposal).unwrap(),
        "genuine coherent VST3 Scope terminal must reach actual StateEntry"
    );
    let frozen_grant = runtime.resource_grant();
    assert_eq!(frozen_grant.max_runtime_occurrences, 3);
    assert_eq!(frozen_grant.max_fixed_resident_bytes, 512 * 1024 * 1024);
    assert_eq!(frozen_grant.max_prepared_logical_bytes, logical_grant);
    let before = budget_observe!(&runtime);
    assert!(before.cohort.is_none());
    assert_eq!(before.held_logical_bytes, 0);
    assert!(
        runtime.state_entry_fee_witness_for_test().is_none(),
        "initial observation must leave the lazy accounting scope absent"
    );
    let mut current_sessions = BudgetCurrentSessions::default();
    runtime.visit_continuity_evidence(&mut current_sessions).unwrap();
    assert_eq!(
        current_sessions.identities.len(),
        if nested { 3 } else { 1 }
    );
    let current_weights: Vec<_> = current_sessions
        .identities
        .iter()
        .map(|identity| {
            let session = runtime.find_resource_session(*identity).expect("actual current Session");
            (*identity, session.current_resource_footprint().unwrap())
        })
        .collect();
    let old_weight =
        budget_sum(current_weights.iter().map(|(_, weight)| weight.prepared_logical_bytes));
    assert_eq!(
        old_weight, before.published.prepared_logical_bytes,
        "the real whole retained tree contains every cohort local once"
    );
    let old_plan_witnesses: Vec<_> = current_sessions
        .identities
        .iter()
        .map(|identity| {
            let session = runtime.find_resource_session(*identity).unwrap();
            (*identity, Arc::downgrade(session.shared_resource_plan()))
        })
        .collect();
    assert!(old_plan_witnesses.iter().all(|(_, weak)| weak.strong_count() > 0));
    budget_current_claim(&runtime, &source, &claim);

    for program in &programs {
        assert!(program.program.sidechain_routes.is_empty());
        assert!(program.program.transitions.is_empty());
        assert!(program.program.note_lanes.is_empty());
    }
    for identity in &current_sessions.identities {
        let program = runtime.find_resource_session(*identity).unwrap().resource_plan().program();
        assert!(program.sidechain_routes.is_empty());
        assert!(program.transitions.is_empty());
        assert!(program.note_lanes.is_empty());
    }
    // In this qualified ordinary curve family the shared requested shape is
    // exact; actual stored N is observed separately in the generous control.
    let old_locals: Vec<_> = current_weights
        .iter()
        .map(|(_, weight)| weight.prepared_logical_bytes)
        .collect();
    let cursors_before = budget_cursors(&runtime);
    assert_eq!(cursors_before.sessions.len(), if nested { 3 } else { 1 });
    assert_eq!(cursors_before.processors.len(), 2);
    let cancellation = ExecutionCancellationToken::new();

    if let Some(expected) = expected {
        assert_eq!(
            old_weight, expected.current,
            "fresh identical input shape/current charge"
        );
        assert_eq!(
            old_locals, expected.old_locals,
            "every initial local model matches real control"
        );
        let required = expected.current.checked_add(expected.next).expect("finite complete tree");
        assert_eq!(logical_grant.checked_add(1), Some(required));
        if nested {
            assert_eq!(expected.next_locals.len(), 3);
            assert!(!expected.next_locals[0].0);
            assert!(expected.next_locals[1..].iter().all(|(affected, _)| *affected));
            let first_prefix = expected
                .current
                .checked_add(expected.next_locals[0].1)
                .and_then(|value| value.checked_add(expected.next_locals[1].1))
                .expect("first two locals prefix");
            assert!(
                first_prefix <= logical_grant,
                "tight grant deliberately admits early locals but refuses complete nested tree"
            );
        }
        // Every proposal/compiler/registration/cancellation/observation DTO
        // already exists. Measure only the actual public Runtime operation.
        let (result, allocations) = measure_host_allocations_for_test(|| {
            runtime.prepare_native_author_parameter_handoff(
                &programs,
                &proposal,
                destination,
                &cancellation,
            )
        });
        eprintln!("SCOPE_WHOLE_BUDGET_DENIAL: nested={nested}; C={old_weight}; control_N={}; G={logical_grant}; allocations={allocations:?}; error={:?}",
            expected.next, result.as_ref().err());
        assert!(
            matches!(result,
                Err(NativeAudioParameterHandoffError::Resources(
                    AudioRuntimeBuildError::ResourceOverlapGrantExceeded {
                        category: crate::AudioRuntimeResourceCategory::PreparedLogicalBytes,
                        required: actual_required, granted: actual_granted,
                    }
                )) if actual_required == required && actual_granted == logical_grant
            ),
            "complete independent actual N must define the exact typed refusal"
        );
        let after = budget_observe!(&runtime);
        assert_eq!(
            after, before,
            "denial publishes no Root/footprint/grant/cohort/fee change"
        );
        assert!(after.cohort.is_none());
        assert_eq!(after.held_logical_bytes, 0);
        assert!(
            runtime.state_entry_fee_witness_for_test().is_none(),
            "early denial does not create the lazy fee scope"
        );
        assert_eq!(
            budget_cursors(&runtime),
            cursors_before,
            "all actual Session and native processor identities/heads remain unchanged"
        );
        budget_current_claim(&runtime, &source, &claim);
        assert!(
            parents.iter().all(|parent| unsafe { scope_is_window(*parent) } != 0),
            "both original actual views remain installed after denial"
        );
        assert!(runtime.native_author_state_entry_required(&proposal).unwrap());
        let mut blocked_after = [92.0; 8];
        assert!(matches!(
            runtime.render_into(
                AudioRenderRequest { start_sample: 4, frames: 4 },
                &mut blocked_after,
            ),
            Err(AudioExecutionError::NativeAuthorPending(_))
        ));
        assert_eq!(
            blocked_after, [92.0; 8],
            "unaccepted real author remains held"
        );
        assert_eq!(budget_cursors(&runtime), cursors_before);
        assert_eq!(runtime.resource_grant(), frozen_grant);
        drop(runtime);
        assert!(old_plan_witnesses.iter().all(|(_, weak)| weak.upgrade().is_none()));
        drop(source);
        // Keep the desired allocator assertion last so an old-ordering red
        // still records typed refusal, Source/cursors and actual retirement.
        assert_eq!(
            allocations, [0; 4],
            "public StateEntry budget denial must precede every local deep clone/storage"
        );
        return expected.clone();
    }

    let result = runtime.prepare_native_author_parameter_handoff(
        &programs,
        &proposal,
        destination,
        &cancellation,
    );
    assert!(matches!(result,
        Err(NativeAudioParameterHandoffError::Admission(
            NativeAudioAuthorAdmissionError::Pending {
                reason: NativeAudioAuthorHoldReason::CoherentPeerStateRequired, ..
            }
        ))
    ), "real mode2 cleanup qualifies actual retained next Sessions, not a mock resource projection");
    let first = budget_observe!(&runtime);
    let cohort = first.cohort.as_ref().expect("real Root retains partial native cohort");
    assert!(!cohort.installed);
    assert_eq!(cohort.old_root, root_identity);
    assert_eq!(cohort.locals.len(), if nested { 3 } else { 1 });
    assert_eq!(first.root, before.root);
    assert_eq!(first.published, before.published);
    assert_eq!(first.grant, frozen_grant);
    let next_locals: Vec<_> = cohort
        .locals
        .iter()
        .map(|local| {
            assert!(current_sessions.identities.contains(&local.original_identity));
            assert_ne!(
                local.currently_stored_identity.session_id,
                local.original_identity.session_id
            );
            (
                local.affected,
                local.currently_stored_weight.prepared_logical_bytes,
            )
        })
        .collect();
    let new_weight = budget_sum(next_locals.iter().map(|(_, weight)| *weight));
    assert!(
        new_weight > old_weight,
        "nine actual constant curve keys give independent N>O"
    );
    assert_eq!(first.held_logical_bytes, new_weight);
    if nested {
        assert!(!next_locals[0].0);
        assert!(next_locals[0].1 > 0);
        assert_eq!(cohort.locals[0].original_identity, root_identity);
        assert_eq!(
            cohort.locals[0].currently_stored_identity.prepared_plan_id,
            root_identity.prepared_plan_id,
            "unaffected root shares Plan but gets a complete new Session"
        );
        assert!(next_locals[1..].iter().all(|(affected, _)| *affected));
    } else {
        assert!(next_locals.iter().all(|(affected, _)| *affected));
    }
    budget_current_claim(&runtime, &source, &claim);
    let witness = runtime.state_entry_fee_witness_for_test().expect("actual enrolled owner fee");
    assert_eq!(witness.held_bytes(), new_weight);
    let weights = BudgetWeights {
        current: old_weight,
        next: new_weight,
        old_locals,
        next_locals,
    };
    eprintln!(
        "SCOPE_WHOLE_BUDGET_CONTROL: nested={nested}; actual={weights:?}; partial_native=true"
    );
    drop(runtime);
    assert_eq!(
        witness.held_bytes(),
        0,
        "actual pending owner retires before fresh tight instance"
    );
    assert!(old_plan_witnesses.iter().all(|(_, weak)| weak.upgrade().is_none()));
    drop(source);
    assert_eq!(witness.held_bytes(), 0);
    weights
}

fn run_scope_whole_tight_budget(nested: bool) {
    let control = run_scope_budget_case(nested, 512 * 1024 * 1024, None);
    let grant = control
        .current
        .checked_add(control.next)
        .and_then(|required| required.checked_sub(1))
        .expect("independent tight frozen grant");
    let denied = run_scope_budget_case(nested, grant, Some(&control));
    assert_eq!(denied, control);
}

#[test]
#[ignore = "fresh matching product helper + real Windows VST3 editor; whole Scope tight original grant"]
fn native_vst3_scope_whole_tight_budget_refuses_before_local_clone() {
    run_scope_whole_tight_budget(false);
}

#[test]
#[ignore = "fresh matching product helper + real Windows VST3 editor; complete nested tree tight original grant"]
fn native_vst3_nested_scope_whole_tight_budget_counts_unaffected_root_before_clone() {
    run_scope_whole_tight_budget(true);
}
