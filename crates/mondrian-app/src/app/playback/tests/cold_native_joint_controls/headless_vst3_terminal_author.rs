//! Two real owned VST3 prefixes share one App author group and Media Source.
//! The first four-frame Preroll window triggers mode13; output never activates.
//! This qualifies joint author custody, not PCM samples or a physical device.
#![cfg(windows)]
use super::*;
use mondrian_core::{
    ExactAutomationCurve, ExecutionCancellationToken, NativeAudioAuthorCallback,
    NativeAudioAuthorCompletion, NativeAudioAuthorProposal,
    NativeAudioEditorControlIntent as Intent, NativeAudioEditorControlOutcome as Outcome,
    NativeAudioEditorControlRefusal as EditorRefusal, NativeAudioEditorTarget,
    NativeAudioParameterRescanKind, ParameterId,
};
use mondrian_media::{
    AudioPcmNativeAuthorHandoffRefusal as AuthorRefusal,
    AudioPcmNativeAuthorTerminalBatchStatus as Status, AudioPcmRenderer,
};
use std::collections::BTreeMap;

const CLASS: &str = "61921CF43A2B4A68AE5AE08241D85E22";

// The existing initial-values fixture state, identical to Audio's mode13 input.
// This constructs only owned native bytes; it never constructs a proposal/lease.
fn initial_mode13() -> Vec<u8> {
    let mut native = b"MIV1".to_vec();
    native.extend_from_slice(&1_u32.to_le_bytes());
    native.extend_from_slice(&0.375_f64.to_le_bytes());
    native.extend_from_slice(&13_u32.to_le_bytes());
    let mut state = b"VST3HOST_STATE\0\0".to_vec();
    for word in [1_u32, 20, 20] {
        state.extend_from_slice(&word.to_le_bytes());
    }
    state.extend_from_slice(&native);
    state.extend_from_slice(&native);
    state
}

fn actual_rescan(proposal: &NativeAudioAuthorProposal, gain: &ParameterId) {
    assert!(proposal.terminal());
    assert_eq!(
        proposal.completion(),
        NativeAudioAuthorCompletion::CoherentConfiguration
    );
    assert_eq!(
        (proposal.open_parameter_edits(), proposal.group_depth()),
        (0, 0)
    );
    let [NativeAudioAuthorCallback::ParameterValuesRescanned { kind, values }] =
        proposal.callbacks()
    else {
        panic!("one actual controller rescan, not a fabricated author update");
    };
    assert_eq!(*kind, NativeAudioParameterRescanKind::Values);
    assert_eq!(values.as_slice(), [(gain.clone(), 0.75_f64.to_bits())]);
    let state = proposal
        .state()
        .expect("actual same-instance component/controller save")
        .as_slice();
    assert_eq!(state.len(), 28 + 24 + 44);
    assert_eq!(&state[..16], b"VST3HOST_STATE\0\0");
    for (at, expected) in [(16, 1_u32), (20, 24), (24, 44)] {
        assert_eq!(&state[at..at + 4], &expected.to_le_bytes());
    }
    let mut native = b"MIV1".to_vec();
    native.extend_from_slice(&1_u32.to_le_bytes());
    native.extend_from_slice(&0.75_f64.to_le_bytes());
    native.extend_from_slice(&13_u32.to_le_bytes());
    assert_eq!(&state[28..48], native.as_slice());
    assert_eq!(&state[52..72], native.as_slice());
    assert!(
        u32::from_le_bytes(state[48..52].try_into().unwrap()) >= 2,
        "actual PCM plus the original zero-sample author bridge"
    );
    assert_eq!(
        &state[92..96],
        &0_u32.to_le_bytes(),
        "no wrong-thread controller operation"
    );
}

#[test]
#[ignore = "current normal Product and owned initial-values mode13 DLL; two real App terminal owners, software Preroll only, no PCM-sample/GUI/device qualification"]
fn app_vst3_peer_terminal_author_publishes_one_frontier_and_closes_same_source() {
    let deadline = Instant::now() + Duration::from_secs(30);
    let material = |name| {
        let path = std::path::PathBuf::from(
            std::env::var_os(name).expect("explicit current native material"),
        );
        assert!(path.is_absolute() && path.is_file());
        path
    };
    let helper = material("MONDRIAN_VST3_TEST_HELPER");
    let library = material("MONDRIAN_VST3_INITIAL_VALUES_TEST_PLUGIN");
    let clap = Arc::new(
        mondrian_audio::InstalledClapAudioProcessorSpecResolver::new(helper.clone()).unwrap(),
    );
    let vst3 =
        Arc::new(mondrian_audio::InstalledVst3AudioProcessorSpecResolver::new(helper).unwrap());
    assert!(vst3
        .install_plugin(library)
        .expect("actual owned catalog")
        .iter()
        .any(|descriptor| descriptor.class_id == CLASS));
    let config = mondrian_media::AudioPlaybackConfig {
        chunk_frames: 4,
        preroll_frames: 4,
        high_watermark_frames: 4,
        max_in_flight: 1,
        ..mondrian_media::AudioPlaybackConfig::product_default()
    };
    let render = mondrian_audio::AudioRenderContract {
        sample_rate: config.sample_rate,
        channel_layout: config.channel_layout,
        max_block_frames: config.chunk_frames,
        processing_mode: mondrian_audio::AudioProcessingMode::Realtime,
        processor_session_scratch_budget_bytes:
            mondrian_audio::AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
        public_output_lookahead_budget_frames:
            mondrian_audio::AudioRenderContract::DEFAULT_PUBLIC_OUTPUT_LOOKAHEAD_BUDGET_FRAMES,
        compensation_delay_scratch_budget_bytes:
            mondrian_audio::AudioRenderContract::DEFAULT_COMPENSATION_DELAY_SCRATCH_BUDGET_BYTES,
    };
    let gain = ParameterId::new_static("vst3.param.0");
    let mut instances = Vec::with_capacity(2);
    for _ in 0..2 {
        let seed = initial_mode13();
        let mut instance = vst3
            .create_instance(CLASS, render, Some(seed.clone()))
            .expect("independent real initial-values instance");
        assert_eq!(instance.parameters.len(), 1);
        assert_eq!(
            instance.opaque_state.as_ref().unwrap().as_slice(),
            seed.as_slice()
        );
        assert!(!instance.bypassed);
        instance
            .set_parameter_automation(ExactAutomationCurve::new(gain.clone(), 0.75).unwrap())
            .unwrap();
        instances.push(instance);
    }
    let second = instances.pop().unwrap();
    let first = instances.pop().unwrap();
    let ids = [first.id, second.id];
    assert_ne!(
        ids[0], ids[1],
        "two actual catalog creations, not cloned author IDs"
    );
    let ColdNativeControl { mut state, _directory, output, renderer: weak } =
        prepare_app_native_control_with_instance(
            clap,
            vst3,
            config,
            first,
            move |sequence, assets, root| {
                sequence.audio_program.outputs[0].strip.pre_fader.processors.push(second);
                super::project_state_roundtrip::initialize_nonzero_audio(sequence, assets, root);
            },
        );
    let mut installed_verifier = None;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert!(state.native_audio_editor.is_none());
        assert!(state.active_sequence().unwrap().audio_program.note_lanes.is_empty());
        let erased: std::sync::Weak<dyn AudioPcmRenderer> = weak.clone();
        assert!(state.audio_playback.matches_source_weak(&erased));
        let renderer = weak.upgrade().expect("actual installed same Timeline Source");
        let original_verifier = renderer.owner_installation_verifier().unwrap();
        let original_claim = original_verifier.current_claim().unwrap();
        original_verifier.validate(&original_claim).unwrap();
        installed_verifier = Some(original_verifier.clone());
        let generation = state.audio_playback_snapshot().generation;
        let author_session = state.authoring.as_ref().unwrap().session_id();
        let author_generation = state.authoring.as_ref().unwrap().author_generation();
        let original_document =
            serde_json::to_value(state.authoring.as_ref().unwrap().document()).unwrap();
        let initial_history = state.authoring.as_ref().unwrap().history().diagnostics();
        let sequence = state.active_sequence_id().unwrap();
        let mut renderer_id = None;
        let mut objects = BTreeMap::new();
        let mut parameter_targets = BTreeMap::new();
        let mut members: BTreeMap<_, NativeAudioAuthorProposal> = BTreeMap::new();
        let mut new_targets: BTreeMap<_, NativeAudioEditorTarget> = BTreeMap::new();
        let mut new_generation = None;
        let mut prepared_count = 0_usize;
        let mut acknowledged_count = 0_usize;
        let mut finished_count = 0_usize;
        let mut priming = false;
        loop {
            assert!(
                Instant::now() < deadline,
                "joint App terminal author did not close within 30s"
            );
            assert!(state.native_audio_editor.is_none());
            assert!(state.audio_playback.matches_source_weak(&erased));
            let mode = if priming {
                AudioPlaybackMode::Preroll
            } else {
                AudioPlaybackMode::Idle
            };
            let poll = state
                .audio_playback
                .poll(mode, audio_anchor(0))
                .expect("original Media queue poll");
            assert_eq!(poll.snapshot.generation, generation);
            assert!(poll.snapshot.in_flight <= 1);
            assert!(
                poll.snapshot.next_start_sample <= 4,
                "no second PCM request"
            );
            assert!(
                !poll.snapshot.output.unwrap().active,
                "no device activation"
            );
            assert!(output.lock().queued <= 4);
            assert_eq!(poll.snapshot.render_generation_recovery_count, 0);
            assert_eq!(poll.snapshot.render_substitution_count, 0);
            for event in poll.events {
                let mut prepared = false;
                let mut acknowledged = false;
                match &event {
                    AudioPlaybackEvent::NativeOwnerBindingsObserved {
                        renderer_id: actual,
                        generation: observed,
                        status,
                    } => {
                        assert_eq!(*observed, generation);
                        assert_eq!(
                            *status,
                            mondrian_media::AudioPcmNativeOwnerBindingStatus::Ready
                        );
                        assert!(renderer_id.is_none_or(|prior| prior == *actual));
                        renderer_id = Some(*actual);
                    }
                    AudioPlaybackEvent::NativeParameterObservationsObserved {
                        renderer_id: actual,
                        generation: observed,
                        observation,
                    } => {
                        assert_eq!(Some(*actual), renderer_id);
                        assert_eq!(*observed, generation);
                        let target = observation.target();
                        assert!(ids.contains(&target.instance));
                        assert_eq!(target.root_plan, target.local_plan);
                        assert_eq!(target.root_session, target.local_session);
                        let object = (
                            observation.process_id().get(),
                            observation.object_serial().get(),
                        );
                        assert_ne!(object.0, std::process::id());
                        if let Some(previous) = objects.insert(target.instance, object) {
                            assert_eq!(
                                previous, object,
                                "same native incarnation across joint installation"
                            );
                        }
                        let fact = observation
                            .facts()
                            .iter()
                            .rev()
                            .find(|f| f.parameter == gain)
                            .expect("actual Gain query");
                        if target.continuity_epoch.is_some() {
                            assert_eq!(fact.value_bits, Some(0.75_f64.to_bits()));
                        }
                        if prepared_count == 0 {
                            original_verifier.validate(&original_claim).unwrap();
                            assert_eq!(target.installed_generation, original_claim.generation());
                        } else if target.installed_generation != original_claim.generation() {
                            let actual = renderer.owner_installation_verifier().unwrap();
                            let claim = actual.current_claim().unwrap();
                            actual.validate(&claim).unwrap();
                            assert_eq!(target.installed_generation, claim.generation());
                            assert!(new_generation.is_none_or(|prior| prior == claim.generation()));
                            new_generation = Some(claim.generation());
                            let original = members
                                .get(&target.instance)
                                .expect("actual original peer")
                                .target();
                            assert_eq!(target.root_session, original.root_session);
                            assert_eq!(target.local_session, original.local_session);
                            assert_eq!(target.continuity_epoch, original.continuity_epoch);
                            assert_ne!(target.root_plan, original.root_plan);
                            assert_ne!(target.binding, original.binding);
                            if let Some(previous) = new_targets.insert(target.instance, target) {
                                assert_eq!(previous, target);
                            }
                            installed_verifier = Some(actual);
                        }
                        parameter_targets.insert(target.instance, target);
                    }
                    AudioPlaybackEvent::NativeAuthorProposalObserved {
                        renderer_id: actual,
                        generation: observed,
                        proposal,
                    } => {
                        assert!(
                            priming,
                            "mode13 must not be replaced with a fabricated startup callback"
                        );
                        assert_eq!(Some(*actual), renderer_id);
                        assert_eq!(*observed, generation);
                        assert_eq!(proposal.sequence(), sequence);
                        let target = proposal.target();
                        assert!(ids.contains(&target.instance));
                        assert!(objects.contains_key(&target.instance));
                        assert_eq!(target.root_plan, target.local_plan);
                        assert_eq!(target.root_session, target.local_session);
                        assert_eq!(target.installed_generation, original_claim.generation());
                        assert!(target.continuity_epoch.is_some());
                        actual_rescan(proposal, &gain);
                        if let Some(previous) = members.get(&target.instance) {
                            assert!(previous.same_retained_proposal(proposal));
                        } else {
                            assert_eq!(
                                prepared_count, 0,
                                "no late peer silently joins a published frontier"
                            );
                            // Author delivery suppresses optional presentation
                            // queries. The cold DTO authenticates the object, not
                            // a future Enter binding/epoch. The actual proposal
                            // is projected from that Hosted owner after PCM.
                            let cold = parameter_targets.get(&target.instance).unwrap();
                            assert_eq!(target.root_plan, cold.root_plan);
                            assert_eq!(target.root_session, cold.root_session);
                            assert_eq!(target.local_session, cold.local_session);
                            assert_eq!(target.installed_generation, cold.installed_generation);
                            members.insert(target.instance, proposal.as_ref().clone());
                        }
                    }
                    AudioPlaybackEvent::NativeAuthorTerminalBatchPrepared {
                        renderer_id: actual,
                        generation: observed,
                        proposal,
                        ..
                    } => {
                        assert_eq!(Some(*actual), renderer_id);
                        assert_eq!(*observed, generation);
                        assert_eq!(
                            members.len(),
                            2,
                            "actual preparation covers both retained native peers"
                        );
                        assert!(members.values().any(|p| p.same_retained_proposal(proposal)));
                        assert!(state.authoring.as_ref().unwrap().has_pending_native_authoring());
                        assert_eq!(
                            serde_json::to_value(state.authoring.as_ref().unwrap().document())
                                .unwrap(),
                            original_document
                        );
                        assert_eq!(
                            state.authoring.as_ref().unwrap().author_generation(),
                            author_generation
                        );
                        prepared_count += 1;
                        assert_eq!(prepared_count, 1);
                        prepared = true;
                    }
                    AudioPlaybackEvent::NativeAuthorTerminalBatchObserved {
                        renderer_id: actual,
                        generation: observed,
                        proposal,
                        status,
                        ..
                    } => {
                        assert_eq!(Some(*actual), renderer_id);
                        assert_eq!(*observed, generation);
                        assert!(members.values().any(|p| p.same_retained_proposal(proposal)));
                        match status {
                            Status::Refused(AuthorRefusal::AuthorStatePending)
                                if prepared_count == 0 =>
                            {
                                // Missing/unpolled peer is an expected failed attempt.
                                // It must not publish one member or erase the real group.
                                assert_eq!(
                                    serde_json::to_value(
                                        state.authoring.as_ref().unwrap().document()
                                    )
                                    .unwrap(),
                                    original_document
                                );
                                assert_eq!(
                                    state.authoring.as_ref().unwrap().author_generation(),
                                    author_generation
                                );
                            }
                            Status::Acknowledged { target }
                            | Status::InstalledAwaitingAcknowledgement { target } => {
                                assert_eq!(prepared_count, 1);
                                let original = members.get(&target.instance).unwrap().target();
                                assert_eq!(target.root_session, original.root_session);
                                assert_eq!(target.local_session, original.local_session);
                                assert_eq!(target.continuity_epoch, original.continuity_epoch);
                                assert_ne!(target.root_plan, original.root_plan);
                                assert_eq!(target.root_plan, target.local_plan);
                                assert_ne!(target.binding, original.binding);
                                let actual = renderer.owner_installation_verifier().unwrap();
                                let claim = actual.current_claim().unwrap();
                                actual.validate(&claim).unwrap();
                                assert_ne!(claim.generation(), original_claim.generation());
                                assert_eq!(target.installed_generation, claim.generation());
                                assert!(
                                    new_generation.is_none_or(|prior| prior == claim.generation())
                                );
                                new_generation = Some(claim.generation());
                                installed_verifier = Some(actual);
                                if matches!(status, Status::Acknowledged { .. }) {
                                    acknowledged_count += 1;
                                    assert_eq!(acknowledged_count, 1, "one all-member Ack outcome");
                                    acknowledged = true;
                                }
                            }
                            other => panic!("joint author operation refused: {other:?}"),
                        }
                    }
                    AudioPlaybackEvent::NativeDiagnosticsObserved {
                        renderer_id: actual,
                        generation: observed,
                        batch,
                    } => {
                        assert_eq!(Some(*actual), renderer_id);
                        assert_eq!(*observed, generation);
                        assert!(!batch.loss().incomplete());
                    }
                    other => panic!("joint NoGUI control admitted unexpected event: {other:?}"),
                }
                state
                    .handle_audio_playback_event(event, Instant::now())
                    .expect("original App event dispatcher");
                let author = state.authoring.as_ref().unwrap();
                assert_eq!(author.session_id(), author_session);
                assert!(state.native_audio_editor.is_none());
                if prepared {
                    assert_eq!(
                        author.author_generation().get(),
                        author_generation.get().checked_add(1).unwrap()
                    );
                    assert_eq!(
                        author.history().diagnostics().undo_entries,
                        initial_history.undo_entries + 1
                    );
                    assert!(
                        author.has_pending_native_authoring(),
                        "accepted Document still waits for native Acks"
                    );
                    for id in ids {
                        let current = state.active_sequence().unwrap().audio_program.outputs[0]
                            .strip
                            .pre_fader
                            .processors
                            .iter()
                            .find(|p| p.id == id)
                            .unwrap();
                        assert_eq!(
                            current.parameters[&gain]
                                .automation
                                .as_ref()
                                .unwrap()
                                .default_value
                                .to_bits(),
                            0.75_f64.to_bits()
                        );
                        assert_eq!(
                            current.opaque_state.as_ref().unwrap().as_slice(),
                            members[&id].state().unwrap().as_slice()
                        );
                    }
                } else if prepared_count == 0 {
                    assert_eq!(author.author_generation(), author_generation);
                    assert_eq!(author.history().diagnostics(), initial_history);
                } else {
                    assert_eq!(
                        author.author_generation().get(),
                        author_generation.get().checked_add(1).unwrap()
                    );
                    assert_eq!(
                        author.history().diagnostics().undo_entries,
                        initial_history.undo_entries + 1
                    );
                }
                if acknowledged {
                    assert!(!author.has_pending_native_authoring());
                    finished_count += 1;
                    assert_eq!(finished_count, 1);
                }
                assert!(
                    Instant::now() < deadline,
                    "original event handler exceeded total work budget"
                );
            }
            // This is the existing shared pump's retry tail, not a second
            // candidate/installation driver. Busy retirement custody may persist.
            state
                .retry_native_audio_terminal_author()
                .expect("original retained-work retry");
            if !priming && renderer_id.is_some() && objects.len() == 2 {
                assert_ne!(
                    objects[&ids[0]].0, objects[&ids[1]].0,
                    "independent native children"
                );
                renderer
                    .validate_native_author_source_closed(&ExecutionCancellationToken::new())
                    .expect("actual cold frontier is closed before its first PCM");
                assert_eq!(output.lock().queued, 0);
                priming = true;
            }
            if acknowledged_count == 1 && new_targets.len() == 2 {
                assert_eq!(prepared_count, 1);
                assert_eq!(finished_count, 1);
                assert_eq!(members.len(), 2);
                assert_eq!(output.lock().queued, 4, "one real complete Preroll window");
                let snapshot = state.audio_playback_snapshot();
                assert_eq!(snapshot.in_flight, 0);
                assert_eq!(snapshot.next_start_sample, 4);
                assert!(!snapshot.output.unwrap().active);
                assert_eq!(
                    new_targets[&ids[0]].root_plan,
                    new_targets[&ids[1]].root_plan
                );
                assert_eq!(
                    new_targets[&ids[0]].installed_generation,
                    new_targets[&ids[1]].installed_generation
                );
                assert_ne!(new_generation, Some(original_claim.generation()));
                let author = state.authoring.as_ref().unwrap();
                assert!(!author.has_pending_native_authoring());
                assert_eq!(
                    author.history().diagnostics().pending_entries,
                    initial_history.pending_entries
                );
                renderer
                    .validate_native_author_source_closed(&ExecutionCancellationToken::new())
                    .expect("all actual retained prefixes acknowledged on the complete Root");
                for original in members.values() {
                    assert!(matches!(
                        renderer
                            .control_native_editor(
                                Intent::Close { target: original.target() },
                                &ExecutionCancellationToken::new()
                            )
                            .unwrap(),
                        Outcome::Refused(EditorRefusal::NotInstalled)
                    ));
                }
                assert!(Instant::now() < deadline);
                break;
            }
            std::thread::yield_now();
        }
    }));
    let cleanup = state.shutdown_for_endurance(Instant::now() + Duration::from_secs(2));
    let clean = cleanup.all_resources_released()
        && weak.upgrade().is_none()
        && installed_verifier.as_ref().is_none_or(|v| {
            matches!(
                v.current_claim(),
                Err(mondrian_core::AudioOwnerInstallationError::NotInstalled)
            )
        });
    if let Err(payload) = result {
        if !clean {
            eprintln!("joint VST3 author cleanup incomplete: {cleanup:?}");
        }
        std::panic::resume_unwind(payload);
    }
    assert!(clean, "JOINT_VST3_AUTHOR_CLEANUP_HOLD: {cleanup:?}");
}
