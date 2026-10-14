// Actual Windows VST3 Scope storage and conditional logical-role lifetime.
// Physical allocation, native Ack refusal and performance have separate gates.

use super::*;

#[derive(Default)]
struct FeeCurrentSessions {
    identities: Vec<crate::AudioSessionCursorIdentity>,
}

impl AudioContinuityEvidenceVisitor for FeeCurrentSessions {
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

fn fee_sum(mut weights: impl Iterator<Item = usize>) -> usize {
    weights
        .try_fold(0_usize, |sum, weight| sum.checked_add(weight))
        .expect("actual observed policy extent must be finite")
}

fn fee_current_claim(
    runtime: &AudioProgramRuntime,
    source: &mondrian_core::AudioOwnerInstallationSource,
    expected: &mondrian_core::AudioOwnerInstallationClaim,
) {
    let current = source.current_claim().expect("actual source").expect("installed slot");
    assert_eq!(current.generation(), expected.generation());
    let verifier = runtime
        .native_owner_installation_verifier()
        .expect("actual Runtime registration");
    verifier.validate(&current).expect("current claim belongs to this Runtime");
    verifier.validate(expected).expect("the exact expected claim remains current");
}

macro_rules! fee_observe {
    ($runtime:expr) => {{
        let observation = ($runtime)
            .observe_state_entry_fees_for_test()
            .expect("borrow the actual cohort storage and Root policy");
        let held = ($runtime)
            .check_resource_ceiling(crate::AudioRuntimeResourceCategory::PreparedLogicalBytes, 0)
            .expect("existing logical checker independently observes the same held subtotal");
        assert_eq!(observation.held_logical_bytes, held);
        assert_eq!(observation.root, ($runtime).resource_root_identity());
        assert_eq!(observation.published, ($runtime).resource_footprint());
        assert_eq!(observation.grant, ($runtime).resource_grant());
        eprintln!(
            "STATE_ENTRY_FEE_VIEW: root={:?}; C={}; H={}; G={}; cohort={:?}",
            observation.root,
            observation.published.prepared_logical_bytes,
            observation.held_logical_bytes,
            observation.grant.max_prepared_logical_bytes,
            observation.cohort
        );
        observation
    }};
}

fn run_scope_fee_contract(nested: bool, drop_pending_runtime: bool) {
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
        AudioRuntimeResourceGrant::new(3, 512 * 1024 * 1024, 512 * 1024 * 1024),
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
    let (next_registration, destination) =
        source.register().expect("actual next destination registration");
    eprintln!("SCOPE_NATIVE_PRIMARY: two distinct actual child PIDs {process_ids:?}, positive PCM and coherent92B terminal passed; nested={nested}");
    assert!(proposal.terminal());
    assert!(
        runtime.native_author_state_entry_required(&proposal).unwrap(),
        "genuine coherent VST3 Scope terminal must reach actual StateEntry"
    );
    let frozen_grant = runtime.resource_grant();
    assert_eq!(frozen_grant.max_runtime_occurrences, 3);
    assert_eq!(frozen_grant.max_fixed_resident_bytes, 512 * 1024 * 1024);
    assert_eq!(frozen_grant.max_prepared_logical_bytes, 512 * 1024 * 1024);
    let before = fee_observe!(&runtime);
    assert!(before.cohort.is_none());
    assert_eq!(before.held_logical_bytes, 0);
    assert!(
        runtime.state_entry_fee_witness_for_test().is_none(),
        "initial observation must leave the lazy accounting scope absent"
    );
    let mut current_sessions = FeeCurrentSessions::default();
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
        fee_sum(current_weights.iter().map(|(_, weight)| weight.prepared_logical_bytes));
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
    fee_current_claim(&runtime, &source, &claim);

    let first_failure = runtime.prepare_native_author_parameter_handoff(
        &programs,
        &proposal,
        destination.clone(),
        &ExecutionCancellationToken::new(),
    );
    eprintln!(
        "STATE_ENTRY_FEE_FIRST_PREPARE: nested={nested}; error={:?}",
        first_failure.as_ref().err()
    );
    assert!(
        matches!(
            first_failure,
            Err(NativeAudioParameterHandoffError::Admission(
                NativeAudioAuthorAdmissionError::Pending {
                    reason: NativeAudioAuthorHoldReason::CoherentPeerStateRequired,
                    ..
                }
            ))
        ),
        "the real first mode2 native cleanup must qualify persisted partial preparation"
    );
    let first = fee_observe!(&runtime);
    let first_cohort =
        first.cohort.as_ref().expect("actual Root persists partial native preparation");
    assert!(!first_cohort.installed);
    assert_eq!(first_cohort.old_root, root_identity);
    assert_eq!(first_cohort.locals.len(), if nested { 3 } else { 1 });
    assert_eq!(first.root, before.root);
    assert_eq!(first.published, before.published);
    assert_eq!(first.grant, frozen_grant);
    for local in &first_cohort.locals {
        assert_ne!(
            local.currently_stored_identity.session_id,
            local.original_identity.session_id
        );
        assert!(current_sessions.identities.contains(&local.original_identity));
    }
    let new_weight = fee_sum(
        first_cohort
            .locals
            .iter()
            .map(|local| local.currently_stored_weight.prepared_logical_bytes),
    );
    assert!(new_weight > old_weight,
        "nine actual constant Curve keys must qualify distinct actual N/O before interpreting transfer");
    assert_eq!(
        first.held_logical_bytes, new_weight,
        "first retained error enrolls the real N role, not zero, C+N or a receipt peak"
    );
    let witness = runtime
        .state_entry_fee_witness_for_test()
        .expect("actual persistence creates its one scope");
    assert_eq!(witness.held_bytes(), new_weight);
    fee_current_claim(&runtime, &source, &claim);
    let mut untouched = [93.0; 8];
    assert!(runtime
        .render_into(
            AudioRenderRequest { start_sample: 4, frames: 4 },
            &mut untouched
        )
        .is_err());
    assert_eq!(
        untouched, [93.0; 8],
        "partial state entry cannot publish old or substitute PCM"
    );

    let cancellation = ExecutionCancellationToken::new();
    cancellation.cancel();
    let cancelled = runtime.prepare_native_author_parameter_handoff(
        &programs,
        &proposal,
        destination.clone(),
        &cancellation,
    );
    assert!(matches!(
        cancelled,
        Err(NativeAudioParameterHandoffError::Admission(
            NativeAudioAuthorAdmissionError::Cancelled
        ))
    ));
    assert_eq!(
        fee_observe!(&runtime),
        first,
        "entry cancellation neither rebuilds partial storage nor refunds its fee"
    );
    fee_current_claim(&runtime, &source, &claim);

    let second_failure = runtime.prepare_native_author_parameter_handoff(
        &programs,
        &proposal,
        destination.clone(),
        &ExecutionCancellationToken::new(),
    );
    eprintln!(
        "STATE_ENTRY_FEE_SECOND_PREPARE: nested={nested}; error={:?}",
        second_failure.as_ref().err()
    );
    assert!(
        matches!(
            second_failure,
            Err(NativeAudioParameterHandoffError::Admission(
                NativeAudioAuthorAdmissionError::Pending {
                    reason: NativeAudioAuthorHoldReason::CoherentPeerStateRequired,
                    ..
                }
            ))
        ),
        "the real second mode2 object must independently qualify its cleanup failure"
    );
    assert_eq!(
        fee_observe!(&runtime),
        first,
        "same-cohort retry retains all exact allocation identities and N once"
    );
    fee_current_claim(&runtime, &source, &claim);

    let initial_handle = runtime
        .prepare_native_author_parameter_handoff(
            &programs,
            &proposal,
            destination.clone(),
            &ExecutionCancellationToken::new(),
        )
        .expect("all real peers now complete native state preparation");
    assert_eq!(fee_observe!(&runtime), first);
    assert!(
        parents.iter().all(|parent| unsafe { scope_is_window(*parent) } == 0),
        "every actual old view closes before completing StateEntry"
    );
    drop(initial_handle);
    assert_eq!(
        fee_observe!(&runtime),
        first,
        "external StateEntry metadata destruction cannot retire the actual Root-owned cohort"
    );
    assert_eq!(witness.held_bytes(), new_weight);
    let mut prepared = runtime
        .prepare_native_author_parameter_handoff(
            &programs,
            &proposal,
            destination,
            &ExecutionCancellationToken::new(),
        )
        .expect("the exact completed cohort remains reusable without re-enrollment");
    assert_eq!(fee_observe!(&runtime), first);
    fee_current_claim(&runtime, &source, &claim);

    if drop_pending_runtime {
        assert!(
            nested,
            "this distinct test qualifies an unaffected Root materialization"
        );
        let affected = fee_sum(
            first_cohort
                .locals
                .iter()
                .filter(|local| local.affected)
                .map(|local| local.currently_stored_weight.prepared_logical_bytes),
        );
        let unaffected: Vec<_> =
            first_cohort.locals.iter().filter(|local| !local.affected).collect();
        assert_eq!(unaffected.len(), 1);
        assert_eq!(unaffected[0].original_identity, root_identity);
        assert_eq!(
            unaffected[0].currently_stored_identity.prepared_plan_id,
            root_identity.prepared_plan_id,
            "unaffected root deliberately aliases its immutable Plan in a fresh Session"
        );
        let unaffected_weight = unaffected[0].currently_stored_weight.prepared_logical_bytes;
        assert!(unaffected_weight > 0);
        assert_eq!(affected.checked_add(unaffected_weight), Some(new_weight));
        assert_eq!(
            first_cohort.locals.iter().filter(|local| local.affected).count(),
            2
        );
        assert_eq!(
            witness.held_bytes(),
            new_weight,
            "logical extra role includes unaffected shared-Plan root exactly once"
        );
        drop(prepared);
        assert_eq!(fee_observe!(&runtime), first);
        // No Runtime/Plan strong owner escapes the test observer. The actual
        // Runtime is destroyed on its own thread; only scalar Arc/Weak remain.
        drop(runtime);
        assert_eq!(
            witness.held_bytes(),
            0,
            "actual pending cohort retires before synchronous Runtime Drop returns"
        );
        assert!(
            old_plan_witnesses.iter().all(|(_, weak)| weak.upgrade().is_none()),
            "all actual current/retained Plan owners retire, including root aliases"
        );
        drop(source);
        assert_eq!(
            witness.held_bytes(),
            0,
            "Source destruction cannot double-credit the retired cohort"
        );
        eprintln!("STATE_ENTRY_FEE_RUNTIME_DROP: N={new_weight}; unaffected={unaffected_weight}; pending_actual_runtime_retired=true; H=0; nested=true");
        return;
    }

    let unrelated = mondrian_core::AudioOwnerInstallationSource::new();
    let (foreign_registration, _) = unrelated.register().expect("unrelated registration");
    let foreign_reservation = unrelated.reserve_install(&foreign_registration, None).unwrap();
    let mut wrong_authorization =
        unrelated.authorize_runtime_installation(foreign_reservation).unwrap();
    let wrong = runtime.install_native_author_parameter_handoff(
        &mut prepared,
        &mut wrong_authorization,
        &ExecutionCancellationToken::new(),
    );
    assert!(
        matches!(
            wrong,
            Err(NativeAudioParameterHandoffError::Installation(
                mondrian_core::AudioOwnerInstallationError::WrongRegistration
            ))
        ),
        "foreign authorization is not a same-Source Superseded CAS test"
    );
    assert_eq!(fee_observe!(&runtime), first);
    fee_current_claim(&runtime, &source, &claim);

    let reservation = source.reserve_install(&next_registration, Some(&claim)).unwrap();
    let mut authorization = source.authorize_runtime_installation(reservation).unwrap();
    let receipt = runtime
        .install_native_author_parameter_handoff(
            &mut prepared,
            &mut authorization,
            &ExecutionCancellationToken::new(),
        )
        .expect("actual whole retained VST3 tree installs under its exact Source authorization");
    assert_eq!(
        receipt.continuity,
        NativeAudioParameterHandoffContinuity::StateEntryRequired
    );
    assert_ne!(receipt.target.local_plan, proposal.target().local_plan);
    assert_ne!(receipt.target.root_session, proposal.target().root_session);
    assert_ne!(
        receipt.target.local_session,
        proposal.target().local_session
    );
    assert_ne!(receipt.target.binding, proposal.target().binding);
    assert!(receipt.claim.generation() > claim.generation());
    assert_eq!(
        receipt.target.installed_generation,
        receipt.claim.generation()
    );
    let installed = fee_observe!(&runtime);
    let installed_cohort =
        installed.cohort.as_ref().expect("actual retired storage survives until Ack");
    assert!(installed_cohort.installed);
    assert_eq!(
        installed_cohort.stable_box_address,
        first_cohort.stable_box_address
    );
    assert_eq!(installed_cohort.old_root, first_cohort.old_root);
    assert_eq!(installed_cohort.locals.len(), first_cohort.locals.len());
    for (original, retired) in first_cohort.locals.iter().zip(&installed_cohort.locals) {
        assert_eq!(retired.original_identity, original.original_identity);
        assert_eq!(
            retired.currently_stored_identity, original.original_identity,
            "actual cohort slots now hold the old Sessions after the real ownership swap"
        );
        let expected_old = current_weights
            .iter()
            .find(|(identity, _)| *identity == original.original_identity)
            .unwrap();
        assert_eq!(retired.currently_stored_weight, expected_old.1);
    }
    let retired_weight = fee_sum(
        installed_cohort
            .locals
            .iter()
            .map(|local| local.currently_stored_weight.prepared_logical_bytes),
    );
    assert_eq!(retired_weight, old_weight);
    assert_eq!(
        installed.held_logical_bytes, old_weight,
        "successful install moves exact fee N to real retired O"
    );
    assert_eq!(witness.held_bytes(), old_weight);
    assert_eq!(
        installed.published.prepared_logical_bytes,
        before
            .published
            .prepared_logical_bytes
            .checked_sub(old_weight)
            .unwrap()
            .checked_add(new_weight)
            .unwrap()
    );
    assert_eq!(
        installed
            .published
            .prepared_logical_bytes
            .checked_add(installed.held_logical_bytes),
        before.published.prepared_logical_bytes.checked_add(new_weight)
    );
    assert_eq!(installed.grant, frozen_grant);
    assert_eq!(installed.root.prepared_plan_id, receipt.target.root_plan);
    assert_eq!(installed.root.session_id, receipt.target.root_session);
    fee_current_claim(&runtime, &source, &receipt.claim);
    assert!(old_plan_witnesses.iter().all(|(_, weak)| weak.strong_count() > 0));
    drop(prepared);
    assert_eq!(
        fee_observe!(&runtime),
        installed,
        "installed metadata Drop cannot release actual retired cohort/O"
    );

    let ack_cancellation = ExecutionCancellationToken::new();
    ack_cancellation.cancel();
    let cancelled_ack =
        runtime.acknowledge_native_author_parameter_handoff(&proposal, &ack_cancellation);
    assert!(
        matches!(
            cancelled_ack,
            Err(NativeAudioParameterHandoffError::Admission(
                NativeAudioAuthorAdmissionError::Cancelled
            ))
        ),
        "entry-cancelled Ack is an early public error, not native callback refusal"
    );
    assert_eq!(fee_observe!(&runtime), installed);
    fee_current_claim(&runtime, &source, &receipt.claim);
    let mut pre_ack = [93.0; 8];
    assert!(runtime
        .render_into(
            AudioRenderRequest { start_sample: 4, frames: 4 },
            &mut pre_ack
        )
        .is_err());
    assert_eq!(
        pre_ack, [93.0; 8],
        "no old or fresh PCM before exact reentered Ack"
    );

    let ack = runtime
        .acknowledge_native_author_parameter_handoff(&proposal, &ExecutionCancellationToken::new())
        .expect("actual isolated reentered object acknowledges the original retained prefix");
    assert_eq!(
        ack.continuity,
        NativeAudioParameterHandoffContinuity::StateEntryRequired
    );
    assert_eq!(ack.target, receipt.target);
    let acknowledged = fee_observe!(&runtime);
    assert!(acknowledged.cohort.is_none());
    assert_eq!(acknowledged.held_logical_bytes, 0);
    assert_eq!(witness.held_bytes(), 0);
    assert_eq!(acknowledged.root, installed.root);
    assert_eq!(acknowledged.published, installed.published);
    assert_eq!(acknowledged.grant, frozen_grant);
    fee_current_claim(&runtime, &source, &receipt.claim);
    assert!(
        first_cohort.locals.iter().all(|local| local.affected),
        "this case has one affected root local"
    );
    for (identity, weak) in &old_plan_witnesses {
        let original = first_cohort
            .locals
            .iter()
            .find(|local| local.original_identity == *identity)
            .unwrap();
        assert_ne!(
            original.currently_stored_identity.prepared_plan_id,
            identity.prepared_plan_id
        );
        assert!(
            weak.upgrade().is_none(),
            "exact cohort retirement releases each distinct old actual Plan"
        );
    }
    eprintln!("STATE_ENTRY_FEE_ACK: O={old_weight}; N={new_weight}; actual_N_to_O=true; exact_native_ack=true; actual_cohort_retired=true; H=0");
    // A fresh admitted epoch is required. Prepared child first-entry consumes
    // the restored object once rather than performing a second SDK restore.
    runtime
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(702),
            start_sample: 4,
        })
        .expect("fresh whole-tree entry");
    let mut accepted = [94.0; 8];
    runtime
        .render_into(
            AudioRenderRequest { start_sample: 4, frames: 4 },
            &mut accepted,
        )
        .expect("every actual peer processes accepted state");
    assert_eq!(
        accepted,
        [2.25, 0.75, 2.25, 0.75, 2.25, 0.75, 2.25, 0.75],
        "two real gain=.75/balance=.25 native peers independently sum"
    );
    let mut after = ScopeActualCursorVisitor {
        instance: instance_id,
        processors: Vec::with_capacity(2),
    };
    runtime.visit_continuity_evidence(&mut after).expect("actual new Session tree");
    assert_eq!(after.processors.len(), 2);
    for (previous, current) in observed.processors.iter().zip(&after.processors) {
        assert_ne!(previous.identity.session_id, current.identity.session_id);
        assert_eq!(previous.occurrence, current.occurrence);
        // Each nested occurrence owns its own monotonic continuity epoch;
        // root entry 702 invalidates its cache, not its local epoch namespace.
        let expected_epoch = if nested {
            let AudioProcessorCursorState::Active { epoch, .. } = previous.state else {
                panic!("the actual previous peer was not active");
            };
            AudioContinuityEpoch::new(epoch.get().checked_add(1).expect("fresh local epoch"))
        } else {
            AudioContinuityEpoch::new(702)
        };
        assert!(
            matches!(current.state,AudioProcessorCursorState::Active {
                epoch, next_input_signal_sample,
            } if epoch==expected_epoch && next_input_signal_sample.sample()==8),
            "the exact local continuity must advance once: previous={:?}, current={:?}",
            previous.state,
            current.state
        );
    }
    // Ordinary first entry revokes old lazy bindings; issue only from the
    // actual current Hosted records and the same current Source claim.
    let current_claim = source.current_claim().expect("source").expect("installed");
    runtime
        .install_native_owner_bindings(&current_claim, &ExecutionCancellationToken::new())
        .expect("fresh exact current epoch binding acknowledgements");
    for (index, cursor) in after.processors.iter().enumerate() {
        let lease = runtime
            .native_owner_lease(AudioNativeProcessorAddress {
                root: runtime.resource_root_identity(),
                local: cursor.identity,
                occurrence: cursor.occurrence,
                epoch: match cursor.state {
                    AudioProcessorCursorState::Active { epoch, .. } => Some(epoch),
                    _ => panic!("actual accepted peer is not active"),
                },
            })
            .expect("fresh actual retained native lease");
        let target = lease.editor_target();
        let title = format!("Scope accepted peer {}", target.binding);
        assert!(matches!(runtime.control_native_editor(Intent::Open {
                target,title:mondrian_core::NativeAudioEditorTitle::new(title.clone()).expect("title"),
            },&ExecutionCancellationToken::new()).expect("reopen actual retained child"),
                Outcome::Opened {target:opened,..} if opened==target));
        let parent = scope_real_parent(&title);
        let mut pid = 0;
        assert_ne!(unsafe { scope_window_process_id(parent, &mut pid) }, 0);
        assert_eq!(
            pid, process_ids[index],
            "same exclusive native child; no hidden replacement process"
        );
        let first_close = runtime
            .control_native_editor(Intent::Close { target }, &ExecutionCancellationToken::new())
            .expect("close fresh exact native view");
        if cleanup_retry {
            assert!(
                matches!(first_close,Outcome::ClosePending {target:actual,stage:
                    mondrian_core::NativeAudioEditorDetachStage::ClearHostFrame} if actual==target)
            );
            assert!(
                matches!(runtime.control_native_editor(Intent::Close {target},
                    &ExecutionCancellationToken::new()).expect("retry only real host-frame clear"),
                    Outcome::Closed {target:closed} if closed==target)
            );
        } else {
            assert!(matches!(first_close,Outcome::Closed {target:closed} if closed==target));
        }
    }
    eprintln!("SCOPE_NATIVE_INSTALLED: nested={nested}; full retained tree, fresh Sessions, same actual children, original-prefix Ack and independent accepted PCM passed");
    assert_eq!(runtime.resource_grant(), frozen_grant);
    assert_eq!(witness.held_bytes(), 0);
}

#[test]
#[ignore = "fresh matching product helper + actual Windows VST3 editor fixture; real Scope cohort fee lifetime"]
fn native_vst3_scope_cohort_fee_retains_real_storage_through_partial_retry_metadata_drop_and_ack() {
    run_scope_fee_contract(false, false);
}

#[test]
#[ignore = "fresh matching product helper + actual Windows VST3 editor fixture; real nested pending Runtime cohort retirement"]
fn native_vst3_nested_scope_cohort_fee_charges_unaffected_root_and_retires_on_runtime_drop() {
    run_scope_fee_contract(true, true);
}
