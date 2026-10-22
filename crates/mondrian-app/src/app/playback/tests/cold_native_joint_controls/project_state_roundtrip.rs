//! Real owned CLAP GUI author -> durable Project -> fresh native state -> PCM.
//! Idle native observations precede every host signal entry in the new owner.
use super::*;
use mondrian_audio::{
    AudioContinuityEvidenceVisitor, AudioExecutionError, AudioProcessorCursorEvidence,
    AudioProcessorCursorState, AudioSessionCursorEvidence, AudioSessionCursorState,
};
use mondrian_core::{
    AudioOwnerInstallationError, ExecutionCancellationToken,
    NativeAudioEditorControlIntent as Intent, NativeAudioEditorControlOutcome as Outcome,
    NativeAudioEditorControlRefusal as Refusal,
};
use mondrian_media::{
    AudioPcmContinuity, AudioPcmRenderGeneration, AudioPcmRenderOutcome, AudioPcmRenderRequest,
    AudioPcmRenderer,
};

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct UnenteredOwner {
    session: Option<AudioSessionCursorEvidence>,
    processor: Option<AudioProcessorCursorEvidence>,
}

impl AudioContinuityEvidenceVisitor for UnenteredOwner {
    fn session(&mut self, evidence: AudioSessionCursorEvidence) -> Result<(), AudioExecutionError> {
        assert_eq!(evidence.state, AudioSessionCursorState::Unentered);
        assert!(
            self.session.replace(evidence).is_none(),
            "one actual root Session"
        );
        Ok(())
    }

    fn processor(
        &mut self,
        evidence: AudioProcessorCursorEvidence,
    ) -> Result<(), AudioExecutionError> {
        assert_eq!(evidence.state, AudioProcessorCursorState::Unentered);
        assert!(
            self.processor.replace(evidence).is_none(),
            "one actual Hosted occurrence"
        );
        Ok(())
    }
}

pub(super) fn unentered_owner(
    renderer: &TimelineAudioPcmRenderer,
    instance: mondrian_core::AudioProcessorInstanceId,
) -> UnenteredOwner {
    let mut facts = UnenteredOwner::default();
    renderer
        .visit_unentered_continuity_for_test(&mut facts)
        .expect("read only the original still-unentered owner");
    let session = facts.session.expect("actual root Session");
    let processor = facts.processor.expect("actual Hosted occurrence");
    assert_eq!(processor.identity, session.identity);
    assert_eq!(processor.occurrence.instance_id, instance);
    facts
}

pub(super) fn initialize_nonzero_audio(
    sequence: &mut Sequence,
    library: &Arc<AssetLibrary>,
    root: &std::path::Path,
) {
    use std::io::Write;
    const FRAMES: u32 = 32;
    let path = root.join("author-roundtrip-stereo.wav");
    let mut wave = std::io::BufWriter::new(std::fs::File::create(&path).expect("create WAV"));
    for bytes in [
        b"RIFF".as_slice(),
        &(36 + FRAMES * 8).to_le_bytes(),
        b"WAVEfmt ",
        &16_u32.to_le_bytes(),
        &3_u16.to_le_bytes(),
        &2_u16.to_le_bytes(),
        &48_000_u32.to_le_bytes(),
        &384_000_u32.to_le_bytes(),
        &8_u16.to_le_bytes(),
        &32_u16.to_le_bytes(),
        b"data",
        &(FRAMES * 8).to_le_bytes(),
    ] {
        wave.write_all(bytes).expect("WAV header");
    }
    for _ in 0..FRAMES {
        wave.write_all(&0.25_f32.to_le_bytes()).expect("left source");
        wave.write_all(&(-0.25_f32).to_le_bytes()).expect("right source");
    }
    wave.flush().expect("complete bounded source");
    drop(wave);
    let path = std::fs::canonicalize(path).expect("canonical WAV source");
    let asset = library
        .commit_media_probe(
            mondrian_assets::AssetMediaProbeCandidate::new(
                path.clone(),
                mondrian_core::MediaFileFingerprint::capture(&path),
                mondrian_media::probe_media_info(&path).expect("actual WAV probe"),
            )
            .expect("actual source candidate"),
            None,
        )
        .expect("register source in the same Project library");
    sequence
        .add_media_audio_clip(
            sequence.audio_tracks[0].id,
            mondrian_timeline::Clip::new(
                asset,
                TimelineTime::ZERO,
                TimelineTime::new(i64::from(FRAMES), 48_000).unwrap(),
            )
            .expect("actual Clip"),
            mondrian_core::AudioSourceComponentId::primary(),
        )
        .expect("initial source assembly before native owner or GUI authoring");
}

#[test]
#[ignore = "current normal Product and owned CLAP Gain editor DLL; GUI author/Project fresh state before first signal, software PCM only"]
fn app_owned_clap_author_state_project_roundtrip_restores_before_first_dsp() {
    use super::super::super::super::product_action::{AudioProductAction, ProductAction};
    let deadline = Instant::now() + Duration::from_secs(30);
    let files = tempfile::tempdir().expect("bounded archive controls");
    let pending_path = files.path().join("pending-author.mdp");
    let archive = files.path().join("原生作者状态重开🙂.mdp");
    let mut refused_saves = 0;
    let ClosedOwnedClapAuthorControl {
        control:
            ColdNativeControl {
                mut state,
                _directory,
                output,
                renderer: old_renderer,
            },
        payload,
        verifier,
        installed_verifier,
        installed_claim,
        target: old_target,
    } = prepare_app_clap_editor_author_closed_with_hooks(initialize_nonzero_audio, |state| {
        assert!(state.authoring.as_ref().unwrap().has_pending_native_authoring());
        assert_eq!(refused_saves, 0, "one actual proposal before its exact Ack");
        let pending = state.project_persistence.pending_requests();
        assert_eq!(pending, 0);
        let document = serde_json::to_value(state.authoring.as_ref().unwrap().document()).unwrap();
        let history = state.authoring.as_ref().unwrap().history().diagnostics();
        let error = state
            .save_project_file_as(pending_path.clone())
            .expect_err("public Save cannot persist a native gesture before its actual Ack");
        assert!(
            error.to_string().contains("native author state is incomplete"),
            "{error:#}"
        );
        assert!(!pending_path.exists());
        assert_eq!(state.project_persistence.pending_requests(), pending);
        assert_eq!(
            serde_json::to_value(state.authoring.as_ref().unwrap().document()).unwrap(),
            document
        );
        assert_eq!(
            state.authoring.as_ref().unwrap().history().diagnostics(),
            history
        );
        refused_saves += 1;
    });
    let mut reopened: Option<AppState> = None;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert_eq!(refused_saves, 1);
        let author = state.authoring.as_ref().unwrap();
        assert!(!author.has_pending_native_authoring());
        let project = author.project_id();
        let old_session = author.session_id();
        let expected_sequence = state.active_sequence().unwrap().clone();
        let volume = mondrian_core::ParameterId::new("clap.param.1").unwrap();
        let instance = &expected_sequence.audio_program.outputs[0].strip.pre_fader.processors[0];
        let curve = instance.parameters[&volume].automation.as_ref().unwrap();
        assert_eq!(curve.default_value.to_bits(), 0.75_f64.to_bits());
        assert!(curve.keyframes.is_empty());
        assert_eq!(
            instance.opaque_state.as_ref().unwrap().as_slice(),
            &0.75_f32.to_le_bytes()
        );
        let clap = state.clap_catalog.as_ref().unwrap().clone();
        let vst3 = state.vst3_catalog.as_ref().unwrap().clone();
        assert!(installed_verifier.validate(&installed_claim).is_ok());
        state
            .save_project_file_as(archive.clone())
            .expect("public durable closed-author Save As");
        assert!(archive.is_file());
        state
            .close_project()
            .expect("public ProjectClose consumes the exact old Source");
        let closed = state.shutdown_audio_playback_and_wait();
        assert!(
            closed.all_workers_terminated()
                && closed.shutdown_resource_facts_complete_at_deadline
                && !closed.shutdown_owner_lifetime_unresolved_at_deadline
                && closed.foreign_owner_panics == 0
                && closed.foreign_owner_abandonments == 0,
            "{closed:?}"
        );
        assert!(
            old_renderer.upgrade().is_none(),
            "old native owner is gone before reopen"
        );
        assert!(state.authoring.is_none() && state.native_audio_renderer.is_none());
        assert!(!state.audio_playback.native_editor_source_available());
        assert_eq!(output.lock().queued, 0);
        for old in [&verifier, &installed_verifier] {
            assert!(matches!(
                old.current_claim(),
                Err(AudioOwnerInstallationError::NotInstalled)
            ));
        }
        assert!(
            Instant::now() < deadline,
            "original close/save exceeds the 30s control budget"
        );

        let mut fresh = AppState::with_native_audio_catalogs(clap, vst3);
        assert!(fresh.shutdown_audio_playback_and_wait().all_workers_terminated());
        let mut snapshot = audio_snapshot();
        snapshot.active = false;
        snapshot.buffered_frames = 0;
        let fresh_output = Arc::new(parking_lot::Mutex::new(NativeHoldPlatformState {
            snapshot,
            revision: 0,
            acknowledged: 0,
            auto_acknowledge: true,
            queued: 0,
        }));
        let config = mondrian_media::AudioPlaybackConfig {
            chunk_frames: 4,
            preroll_frames: 4,
            high_watermark_frames: 8,
            max_in_flight: 1,
            ..mondrian_media::AudioPlaybackConfig::product_default()
        };
        fresh.audio_playback = AppAudioPlayback::Available(Box::new(
            AudioPlayback::with_output(
                config,
                Box::new(NativeHoldPlatformOutput(fresh_output.clone())),
            )
            .unwrap(),
        ));
        reopened = Some(fresh);
        let fresh = reopened.as_mut().unwrap();
        fresh.open_project_file(archive.clone()).expect("public fresh Project reopen");
        let new_author = fresh.authoring.as_ref().unwrap();
        assert_eq!(
            new_author.project_id(),
            project,
            "persistent Project identity survives"
        );
        assert_ne!(
            new_author.session_id(),
            old_session,
            "authority is a new Authoring Session"
        );
        assert!(fresh
            .active_sequence()
            .unwrap()
            .author_state_eq_ignoring_revision(&expected_sequence));
        assert!(!fresh.can_undo_action() && !fresh.can_redo_action());
        assert!(fresh.native_audio_renderer.is_none());
        // Reject the original real UI capability before constructing any new Source.
        let before = fresh.playback_engine.snapshot();
        let history = fresh.authoring.as_ref().unwrap().history().diagnostics();
        let error = fresh
            .dispatch_action(
                ProductAction::Audio(AudioProductAction::NativeEditor(payload.clone()))
                    .into_external_action(),
            )
            .expect_err("old Authoring Session payload is not authority in the reopened Project");
        assert!(
            error.to_string().contains("stale author Session"),
            "{error:?}"
        );
        assert_eq!(fresh.playback_engine.snapshot(), before);
        assert_eq!(
            fresh.authoring.as_ref().unwrap().history().diagnostics(),
            history
        );
        assert!(fresh.native_audio_renderer.is_none());

        fresh
            .synchronize_playback_authoring_binding()
            .expect("bind original stopped Engine");
        let source = fresh
            .audio_playback_renderer("native state Project roundtrip")
            .unwrap()
            .unwrap();
        // Preparation initializes/loads/activates the plugin, but does not call
        // its signal process. These canonical cursor facts prove host NoEnter;
        // they do not claim the plugin itself computes nothing during init.
        let prepared = unentered_owner(&source.renderer, instance.id);
        let weak = Arc::downgrade(&source.renderer);
        assert!(!std::sync::Weak::ptr_eq(&weak, &old_renderer));
        fresh
            .commit_audio_playback(
                "native state Project roundtrip",
                audio_anchor(0),
                Some(source),
            )
            .unwrap();
        let erased: std::sync::Weak<dyn AudioPcmRenderer> = weak.clone();
        assert!(fresh.audio_playback.matches_source_weak(&erased));
        let renderer = weak.upgrade().expect("the exact installed fresh Timeline");
        let new_verifier = renderer.owner_installation_verifier().unwrap();
        let new_claim = new_verifier.current_claim().unwrap();
        assert!(!new_verifier.same_source(&installed_verifier));
        assert!(!new_verifier.same_registration(&installed_verifier));
        assert!(new_verifier.validate(&new_claim).is_ok());
        assert_eq!(unentered_owner(&renderer, instance.id), prepared);
        let generation = fresh.audio_playback_snapshot().generation;
        let poll_deadline = Instant::now() + Duration::from_secs(2);
        let mut ready = false;
        let mut observed = None;
        let mut renderer_id = None;
        while !ready || observed.is_none() {
            // No warmup, reprepare, author, Source replacement or reset can hide
            // an earlier Enter. This exact Plan/Session stays Unentered across
            // the original Idle control path and the real native value query.
            assert!(fresh.audio_playback.matches_source_weak(&erased));
            assert_eq!(unentered_owner(&renderer, instance.id), prepared);
            assert!(new_verifier.validate(&new_claim).is_ok());
            let poll = fresh.audio_playback.poll(AudioPlaybackMode::Idle, audio_anchor(0)).unwrap();
            assert_eq!(poll.snapshot.generation, generation);
            assert_eq!(poll.snapshot.in_flight, 0);
            assert_eq!(poll.snapshot.next_start_sample, 0);
            assert_eq!(poll.snapshot.output.unwrap().buffered_frames, 0);
            for event in poll.events {
                let (actual_renderer, actual_generation) = match &event {
                    AudioPlaybackEvent::NativeOwnerBindingsObserved {
                        renderer_id,
                        generation,
                        ..
                    }
                    | AudioPlaybackEvent::NativeParameterObservationsObserved {
                        renderer_id,
                        generation,
                        ..
                    }
                    | AudioPlaybackEvent::NativeDiagnosticsObserved {
                        renderer_id,
                        generation,
                        ..
                    } => (*renderer_id, *generation),
                    other => panic!("unexpected fresh-source control: {other:?}"),
                };
                assert_eq!(actual_generation, generation);
                if let Some(prior) = renderer_id {
                    assert_eq!(actual_renderer, prior);
                }
                renderer_id = Some(actual_renderer);
                match &event {
                    AudioPlaybackEvent::NativeOwnerBindingsObserved {
                        generation: actual,
                        status,
                        ..
                    } => {
                        assert_eq!(*actual, generation);
                        assert_eq!(
                            *status,
                            mondrian_media::AudioPcmNativeOwnerBindingStatus::Ready
                        );
                        assert!(!ready);
                        ready = true;
                    }
                    AudioPlaybackEvent::NativeParameterObservationsObserved {
                        generation: actual,
                        observation,
                        ..
                    } => {
                        assert_eq!(*actual, generation);
                        assert!(
                            observed.is_none(),
                            "one current-source native value receipt"
                        );
                        let target = observation.target();
                        assert_eq!(target.instance, instance.id);
                        assert_eq!(
                            target.root_plan,
                            prepared.session.unwrap().identity.prepared_plan_id
                        );
                        assert_eq!(
                            target.local_plan,
                            prepared.processor.unwrap().identity.prepared_plan_id
                        );
                        assert_ne!(target.binding, old_target.binding);
                        assert_eq!(target.installed_generation, new_claim.generation());
                        assert!(target.continuity_epoch.is_none());
                        assert_ne!(target.root_session, old_target.root_session);
                        assert_ne!(target.local_session, old_target.local_session);
                        let value = observation
                            .facts()
                            .iter()
                            .rev()
                            .find(|f| f.parameter == volume)
                            .unwrap();
                        assert_eq!(
                            value.value_bits,
                            Some(0.75_f64.to_bits()),
                            "real native getter before any host DSP"
                        );
                        assert_ne!(
                            value.value_bits,
                            Some(1.0_f64.to_bits()),
                            "factory is an independent discriminator"
                        );
                        assert_eq!(value.display.as_deref(), Some("75.00 %"));
                        observed = Some(target);
                    }
                    AudioPlaybackEvent::NativeDiagnosticsObserved {
                        generation: actual, ..
                    } => assert_eq!(*actual, generation),
                    other => panic!(
                        "only unchanged Idle binding/query is admitted before first DSP: {other:?}"
                    ),
                }
                fresh.handle_audio_playback_event(event, Instant::now()).unwrap();
            }
            assert_eq!(unentered_owner(&renderer, instance.id), prepared);
            assert!(new_verifier.validate(&new_claim).is_ok());
            assert_eq!(fresh_output.lock().queued, 0);
            assert!(
                Instant::now() < poll_deadline && Instant::now() < deadline,
                "fresh pre-DSP native receipt did not settle"
            );
            if !ready || observed.is_none() {
                std::thread::yield_now();
            }
        }
        assert_eq!(
            prepared.session.unwrap().identity.session_id,
            observed.unwrap().root_session
        );
        assert_eq!(
            prepared.processor.unwrap().identity.session_id,
            observed.unwrap().local_session
        );
        assert_eq!(
            renderer
                .control_native_editor(
                    Intent::Close { target: old_target },
                    &ExecutionCancellationToken::new()
                )
                .unwrap(),
            Outcome::Refused(Refusal::NotInstalled),
            "the old native target cannot control the fresh owner"
        );
        assert_eq!(unentered_owner(&renderer, instance.id), prepared);
        assert!(new_verifier.validate(&new_claim).is_ok());
        // Only now may this software PCM adapter enter the same fresh source.
        // Every sample is nonzero, so restoring factory unity is observable.
        let pcm_generation = AudioPcmRenderGeneration::new(generation);
        let cancel = ExecutionCancellationToken::new();
        for (start_sample, continuity) in [
            (0, AudioPcmContinuity::Enter(pcm_generation)),
            (4, AudioPcmContinuity::Continue(pcm_generation)),
        ] {
            let AudioPcmRenderOutcome::Rendered(buffer) = renderer
                .render(
                    AudioPcmRenderRequest {
                        start_sample,
                        frame_count: 4,
                        sample_rate: 48_000,
                        channel_layout: AudioChannelLayout::Stereo,
                        continuity,
                    },
                    &cancel,
                )
                .expect("actual fresh source PCM")
            else {
                panic!("complete PCM is required");
            };
            assert_eq!(buffer.samples, [0.1875_f32, -0.1875].repeat(4));
        }
        assert!(new_verifier.validate(&new_claim).is_ok());
        assert!(matches!(
            installed_verifier.current_claim(),
            Err(AudioOwnerInstallationError::NotInstalled)
        ));
        drop(renderer);
        fresh
            .close_project()
            .expect("normal close after the fresh software PCM windows");
        let fresh_closed = fresh.shutdown_audio_playback_and_wait();
        assert!(
            fresh_closed.all_workers_terminated()
                && fresh_closed.shutdown_resource_facts_complete_at_deadline
                && !fresh_closed.shutdown_owner_lifetime_unresolved_at_deadline
                && fresh_closed.foreign_owner_panics == 0
                && fresh_closed.foreign_owner_abandonments == 0,
            "{fresh_closed:?}"
        );
        assert!(weak.upgrade().is_none());
        assert!(matches!(
            new_verifier.current_claim(),
            Err(AudioOwnerInstallationError::NotInstalled)
        ));
        assert!(
            Instant::now() < deadline,
            "complete workflow exceeds its original 30s budget"
        );
    }));
    let original_cleanup = state.shutdown_audio_playback_and_wait();
    let fresh_cleanup = reopened.as_mut().map(AppState::shutdown_audio_playback_and_wait);
    let clean = original_cleanup.all_workers_terminated()
        && original_cleanup.shutdown_resource_facts_complete_at_deadline
        && !original_cleanup.shutdown_owner_lifetime_unresolved_at_deadline
        && original_cleanup.foreign_owner_panics == 0
        && original_cleanup.foreign_owner_abandonments == 0
        && fresh_cleanup.as_ref().is_none_or(|e| {
            e.all_workers_terminated()
                && e.shutdown_resource_facts_complete_at_deadline
                && !e.shutdown_owner_lifetime_unresolved_at_deadline
                && e.foreign_owner_panics == 0
                && e.foreign_owner_abandonments == 0
        });
    if let Err(payload) = result {
        if !clean {
            eprintln!("native author Project cleanup incomplete: old={original_cleanup:?}, new={fresh_cleanup:?}");
        }
        std::panic::resume_unwind(payload);
    }
    assert!(clean, "old={original_cleanup:?}, new={fresh_cleanup:?}");
}
