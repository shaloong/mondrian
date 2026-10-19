//! Real owned mode7 through the original App/Media producer and retirement queue.
//! Finite software-output/PDC qualification; no physical device or realtime clock claim.
use super::*;
use mondrian_media::{
    AudioPcmNativeOwnerBindingStatus, AudioPcmNativeReprepareStatus, AudioPlaybackEvent,
};

const RESTART: Scenario = Scenario {
    mode: 7,
    chunk: 4,
    watermark: 8,
    capture_windows: 8,
    output_capacity: 16,
    lookahead: 3,
};

fn assert_restart_windows(output: &CaptureState, first: usize, anchor: i64) {
    let windows = &output.windows[first..first + 2];
    assert_eq!(windows[0].clear_serial, windows[1].clear_serial);
    for (block, window) in windows.iter().enumerate() {
        assert_eq!(window.samples.len(), 8);
        for (offset, pair) in window.samples.chunks_exact(2).enumerate() {
            let absolute = usize::try_from(anchor).unwrap() + block * 4 + offset;
            assert!(absolute < SOURCE_FRAMES, "finite real WAV coverage");
            let expected = input_sample(absolute);
            for channel in 0..2 {
                assert!(
                    (pair[channel] - expected[channel]).abs() <= 1e-6,
                    "PDC public{absolute} channel{channel}: {} != {}",
                    pair[channel],
                    expected[channel]
                );
            }
        }
    }
}

#[test]
#[ignore = "current normal Product and owned CLAP sidechain mode7; actual App/Media queues and software output, no physical DAC"]
fn native_clap_restart_installs_same_source_and_enters_fresh_pdc_pcm() {
    let mut configuration = vec![7];
    configuration.extend_from_slice(&0.5_f32.to_le_bytes());
    configuration.push(0);
    let (_directory, mut state, output, _) =
        native_fader_fixture_with_configuration(RESTART, Some(configuration));
    let target = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let document = state.authoring.as_ref().unwrap().document().clone();
        let author_generation = state.project_author_generation();
        let history = state.authoring_history().unwrap().diagnostics();
        let deadline = Instant::now() + Duration::from_secs(30);
        // This original headless Ready/preroll seam calls public play exactly
        // once. Preparing a separate cold Source before play would create an
        // unrelated native object and would invalidate a same-child comparison.
        crate::app::playback::tests::play_ready(&mut state);
        let old_generation = state.audio_playback_snapshot().generation;
        let weak = state.native_audio_renderer.as_ref().unwrap().clone();
        let renderer = weak.upgrade().expect("actual sole Timeline renderer");
        let verifier = renderer.owner_installation_verifier().unwrap();
        let old_claim = verifier.current_claim().unwrap();
        let old_identity = cursor_facts(&state).session.unwrap().identity;
        {
            let mut capture = output.lock();
            let at = state.playback_observation_instant_anchor;
            let silent = capture.callback(INACTIVE_CALLBACK_FRAMES, at);
            assert_eq!(silent, vec![[0.0; 2]; INACTIVE_CALLBACK_FRAMES]);
            assert!(capture.queue.is_empty());
            assert_eq!(capture.snapshot.active_callback_consumed_frames, 0);
        }
        let mut old_ready = false;
        let mut renderer_id = None;
        let mut old_object = None;
        let mut new_object = None;
        let mut pending = None;
        let mut quiesced = false;
        let mut installed = None;
        let mut new_identity = None;
        let mut old_lease = None;
        let mut consumed_old = false;
        let mut fresh = None;
        loop {
            state
                .pump_audio_output_observed_for_test(&mut |state, event| match event {
                    AudioPlaybackEvent::NativeOwnerBindingsObserved {
                        renderer_id: actual,
                        generation,
                        status,
                    } => {
                        assert_eq!(*status, AudioPcmNativeOwnerBindingStatus::Ready);
                        if *generation == old_generation {
                            assert!(!old_ready, "one original Media binding completion");
                            old_ready = true;
                            renderer_id = Some(*actual);
                        } else {
                            assert_eq!(
                                Some(*actual),
                                renderer_id,
                                "same installed renderer materialization"
                            );
                            assert!(installed.is_some(), "new binding follows actual settlement");
                        }
                    }
                    AudioPlaybackEvent::NativeParameterObservationsObserved {
                        renderer_id: actual,
                        observation,
                        ..
                    } => {
                        assert_eq!(Some(*actual), renderer_id);
                        assert!(
                            observation.facts().is_empty(),
                            "actual mode7 has no parameter lanes"
                        );
                        let object = (observation.process_id(), observation.object_serial());
                        if observation.target().installed_generation == old_claim.generation() {
                            if let Some(previous) = old_object {
                                assert_eq!(previous, object);
                            }
                            old_object = Some(object);
                        } else {
                            assert!(
                                installed.is_some(),
                                "no alternate native object/Source before installation"
                            );
                            if let Some(previous) = new_object {
                                assert_eq!(previous, object);
                            }
                            new_object = Some(object);
                        }
                    }
                    AudioPlaybackEvent::NativeReprepareRequired {
                        previous_generation,
                        pending_generation,
                        start_sample,
                        notice,
                        ..
                    } => {
                        assert!(old_ready && pending.is_none());
                        assert_eq!(*previous_generation, old_generation);
                        assert_ne!(*pending_generation, old_generation);
                        assert_eq!(*start_sample, 8);
                        assert_eq!(
                            notice.request,
                            mondrian_core::NativeAudioReprepareRequest::ClapRestart
                        );
                        let facts = cursor_facts(state);
                        assert_eq!(facts.session.unwrap().identity, old_identity);
                        assert_heads(&facts, old_generation, 8, RESTART);
                        old_lease = Some(actual_native_lease(&renderer, &facts));
                        assert_eq!(
                            notice.instance_id,
                            facts.processor.unwrap().occurrence.instance_id
                        );
                        let capture = output.lock();
                        assert_eq!(capture.windows.len(), 2);
                        assert_restart_windows(&capture, 0, 0);
                        assert!(capture.queue.is_empty() && !capture.snapshot.active);
                        pending = Some(*pending_generation);
                    }
                    AudioPlaybackEvent::NativeReprepareOutputQuiesced {
                        pending_generation,
                        ..
                    } => {
                        assert_eq!(Some(*pending_generation), pending);
                        assert!(!quiesced);
                        let capture = output.lock();
                        assert!(capture.queue.is_empty() && !capture.snapshot.active);
                        assert_eq!(
                            capture.windows.len(),
                            2,
                            "no PCM while original output retires"
                        );
                        quiesced = true;
                    }
                    AudioPlaybackEvent::NativeReprepareObserved {
                        renderer_id: actual,
                        generation,
                        status,
                    } => {
                        assert_eq!(Some(*actual), renderer_id);
                        assert_eq!(Some(*generation), pending);
                        assert!(quiesced && installed.is_none());
                        let AudioPcmNativeReprepareStatus::Settled { installed_generation } =
                            status
                        else {
                            panic!("real retained restart must settle: {status:?}");
                        };
                        let current = verifier.current_claim().unwrap();
                        assert_eq!(current.generation(), *installed_generation);
                        assert!(verifier.validate(&old_claim).is_err());
                        assert_ne!(*installed_generation, old_claim.generation());
                        let facts = cursor_facts(state);
                        let next = facts.session.unwrap().identity;
                        assert_ne!(next.session_id, old_identity.session_id);
                        assert_ne!(next.prepared_plan_id, old_identity.prepared_plan_id);
                        assert_eq!(
                            facts.session.unwrap().state,
                            AudioSessionCursorState::Unentered
                        );
                        assert_eq!(
                            facts.processor.unwrap().state,
                            AudioProcessorCursorState::Unentered
                        );
                        assert_eq!(facts.session.unwrap().public_output_lookahead_frames, 7);
                        let runtime = renderer.state.lock();
                        assert!(runtime.generation.is_none());
                        assert!(runtime
                            .delivery
                            .native_owner_lease(old_lease.unwrap().address())
                            .is_err());
                        drop(runtime);
                        let AppAudioPlayback::Available(playback) = &state.audio_playback else {
                            panic!("same Media");
                        };
                        assert_eq!(
                            playback.native_owner_binding_status(),
                            None,
                            "old Ready is revoked before Fresh Enter"
                        );
                        let capture = output.lock();
                        assert!(capture.queue.is_empty() && !capture.snapshot.active);
                        assert_eq!(
                            capture.windows.len(),
                            2,
                            "retirement/settlement grants no PCM"
                        );
                        installed = Some(*installed_generation);
                        new_identity = Some(next);
                    }
                    AudioPlaybackEvent::NativeDiagnosticsObserved { .. } => {}
                    other => panic!("no editor/author/substitution/recovery side path: {other:?}"),
                })
                .expect("original App poll/handler/provider/retirement queue");
            let snapshot = state.audio_playback_snapshot();
            assert_eq!(snapshot.render_substitution_count, 0);
            assert_eq!(snapshot.render_generation_recovery_count, 0);
            assert_eq!(snapshot.underrun_recovery_count, 0);
            assert!(!snapshot.output.unwrap().stream_failed);
            assert!(state.native_audio_renderer.as_ref().unwrap().ptr_eq(&weak));
            if installed.is_some() && fresh.is_none() {
                assert!(
                    snapshot.generation > pending.unwrap(),
                    "actual distinct Fresh render generation"
                );
                let anchor = state
                    .authoritative_audio_anchor(
                        &state.playback_engine,
                        state.playback_engine.monotonic_high_water(),
                        "owned restart E2E actual Fresh anchor",
                    )
                    .unwrap();
                assert_eq!(snapshot.next_start_sample, anchor.sample());
                assert!(anchor.sample() >= 0 && (anchor.sample() as usize + 15) < SOURCE_FRAMES);
                fresh = Some((snapshot.generation, anchor.sample()));
            }
            // Do not assume which original consumer first reports pending. If
            // a full old FIFO still needs another render request, consume exactly
            // one real software block; otherwise leave the actual hold alone.
            if pending.is_none() && !consumed_old {
                let mut capture = output.lock();
                if capture.windows.len() == 2 && capture.snapshot.active && capture.queue.len() == 8
                {
                    let consumed = capture.consume(4, Instant::now());
                    for (frame, sample) in consumed.iter().enumerate() {
                        assert_eq!(*sample, input_sample(frame));
                    }
                    consumed_old = true;
                }
            }
            if let Some((generation, anchor)) = fresh {
                if snapshot.generation == generation
                    && snapshot.in_flight == 0
                    && snapshot.output.unwrap().buffered_frames == 8
                    && new_object.is_some()
                {
                    assert!(
                        snapshot.activation_preroll_satisfied && snapshot.output.unwrap().active
                    );
                    assert_eq!(snapshot.media_anchor.unwrap().sample(), anchor);
                    let capture = output.lock();
                    assert_eq!(
                        capture.windows.len(),
                        4,
                        "two complete old and two complete new windows"
                    );
                    assert_restart_windows(&capture, 0, 0);
                    assert_restart_windows(&capture, 2, anchor);
                    assert!(!capture.overflow);
                    assert_eq!(capture.activation_discarded, 0);
                    break;
                }
            }
            assert!(
                Instant::now() < deadline,
                "owned App/Media restart did not complete: {snapshot:?}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        let old_object = old_object
            .expect("NOT_COVERED_OBJECT: original authenticated native observation required");
        assert_eq!(
            new_object,
            Some(old_object),
            "same actual child PID and native load incarnation"
        );
        let (generation, anchor) = fresh.unwrap();
        let facts = cursor_facts(&state);
        assert_eq!(facts.session.unwrap().identity, new_identity.unwrap());
        let final_scenario = Scenario { lookahead: 7, ..RESTART };
        assert_heads(&facts, generation, anchor + 8, final_scenario);
        let new_lease = actual_native_lease(&renderer, &facts);
        assert_ne!(new_lease.binding(), old_lease.unwrap().binding());
        assert_eq!(
            new_lease.installed_root_serial(),
            verifier.current_claim().unwrap().generation()
        );
        assert!(renderer
            .state
            .lock()
            .delivery
            .native_owner_lease(old_lease.unwrap().address())
            .is_err());
        assert_eq!(state.authoring.as_ref().unwrap().document(), &document);
        assert_eq!(state.project_author_generation(), author_generation);
        assert_eq!(state.authoring_history().unwrap().diagnostics(), history);
        assert!(state.native_audio_editor.is_none());
        let AppAudioPlayback::Available(playback) = &state.audio_playback else {
            panic!("the original Media remains available");
        };
        assert!(playback.native_reprepare_pending_generation().is_none());
        println!("owned restart E2E: old_generation={old_generation} pending={:?} fresh={generation} A={anchor} PDC=3->7 same_child={old_object:?}", pending);
        drop(renderer);
    }));
    let deadline = Instant::now() + Duration::from_secs(2);
    let pause = state.pause();
    let audio = state.shutdown_audio_playback_until(deadline);
    let cleanup = state.shutdown_for_endurance(deadline);
    assert!(
        pause.is_ok() && audio.all_workers_terminated() && cleanup.all_resources_released(),
        "complete owned restart cleanup: pause={pause:?}, audio={audio:?}, app={cleanup:?}"
    );
    if let Err(payload) = target {
        std::panic::resume_unwind(payload);
    }
}
