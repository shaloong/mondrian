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
        // The functional software driver has an explicit observation clock.
        // Resource waits retain that instant; only an actual FIFO consumption
        // advances it. Real process/deadline/cleanup clocks remain untouched.
        state.play().expect("one original public play and native Source");
        let software_origin = state.playback_observation_instant_anchor;
        let mut tick = software_origin;
        let mut consumed_frames = 0_u64;
        let old_generation = state.audio_playback_snapshot().generation;
        let weak = state.native_audio_renderer.as_ref().unwrap().clone();
        let renderer = weak.upgrade().expect("actual sole Timeline renderer");
        let verifier = renderer.owner_installation_verifier().unwrap();
        let old_claim = verifier.current_claim().unwrap();
        let old_identity = cursor_facts(&state).session.unwrap().identity;
        {
            let mut capture = output.lock();
            let silent = capture.callback(INACTIVE_CALLBACK_FRAMES, tick);
            assert_eq!(silent, vec![[0.0; 2]; INACTIVE_CALLBACK_FRAMES]);
            assert!(capture.queue.is_empty());
            assert_eq!(capture.snapshot.active_callback_consumed_frames, 0);
        }
        crate::app::playback::tests::current_playback_ready_at(&mut state, tick);
        let original_playback_epoch = state.playback_engine.snapshot().epoch;
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
        let mut fresh_video_ready = false;
        loop {
            state
                .pump_audio_output_clocked_for_test(&mut || tick, &mut |state, event| match event {
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
                    AudioPlaybackEvent::NativeParameterObservationsObserved { .. } => {
                        panic!("mode7 has no parameter lanes or parameter-observation DTOs");
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
                        let lease = actual_native_lease(&renderer, &facts);
                        old_lease = Some(lease);
                        // One real non-RT Preserve RPC under the original Runtime mutex.
                        // The actual Required boundary precedes capture/installation.
                        let object = renderer
                            .state
                            .lock()
                            .delivery
                            .observe_native_owner_object(lease)
                            .expect("authenticated old object while the original restart is held");
                        assert_ne!(object.process_id().get(), std::process::id());
                        old_object = Some((object.process_id(), object.object_serial()));
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
                    other => {
                        panic!("no editor/author/substitution/recovery side path: {other:?}")
                    }
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
                    consumed_frames += 4;
                    // Derive cumulative time once, avoiding per-block rounding drift.
                    tick = software_origin
                        + Duration::from_nanos(
                            (consumed_frames * 1_000_000_000).div_ceil(u64::from(RATE)),
                        );
                    let consumed = capture.consume(4, tick);
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
                {
                    if !fresh_video_ready {
                        assert_eq!(
                            state.playback_engine.snapshot().state,
                            mondrian_playback::TransportState::Priming
                        );
                        assert_ne!(
                            state.playback_engine.snapshot().epoch,
                            original_playback_epoch
                        );
                        assert!(
                            !snapshot.output.unwrap().active,
                            "Fresh PCM cannot activate before its current video demand is ready"
                        );
                        crate::app::playback::tests::current_playback_ready_at(&mut state, tick);
                        fresh_video_ready = true;
                    } else {
                        assert!(
                            snapshot.activation_preroll_satisfied
                                && snapshot.output.unwrap().active
                        );
                        assert_eq!(snapshot.media_anchor.unwrap().sample(), anchor);
                        {
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
                        }
                        let facts = cursor_facts(&state);
                        let lease = actual_native_lease(&renderer, &facts);
                        // A second real non-RT RPC, only after actual Fresh Ready,
                        // activation and both complete new PCM windows. No output lock.
                        let object = renderer
                            .state
                            .lock()
                            .delivery
                            .observe_native_owner_object(lease)
                            .expect("authenticated new object after the original Fresh entry");
                        new_object = Some((object.process_id(), object.object_serial()));
                        assert!(
                            Instant::now() < deadline,
                            "authenticated identity RPC exceeded the original restart deadline"
                        );
                        break;
                    }
                }
            }
            assert!(
                Instant::now() < deadline,
                "owned App/Media restart did not complete: {snapshot:?}; old_ready={old_ready} old_object={old_object:?} new_object={new_object:?} pending={pending:?} quiesced={quiesced} installed={installed:?} fresh={fresh:?} fresh_video_ready={fresh_video_ready}"
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

#[test]
#[ignore = "current normal Product and owned CLAP mode13; two original App/Media restart cycles, software FIFO only"]
fn native_clap_repeated_restart_preserves_same_source_and_fresh_pdc_pcm() {
    let scenario = Scenario { mode: 13, capture_windows: 16, ..RESTART };
    let mut configuration = vec![13];
    configuration.extend_from_slice(&0.5_f32.to_le_bytes());
    configuration.push(0);
    let (_directory, mut state, output, _) =
        native_fader_fixture_with_configuration(scenario, Some(configuration));
    let target = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let document = state.authoring.as_ref().unwrap().document().clone();
        let author_generation = state.project_author_generation();
        let history = state.authoring_history().unwrap().diagnostics();
        let deadline = Instant::now() + Duration::from_secs(30);
        state.play().expect("one public play for both real restart cycles");
        let origin = state.playback_observation_instant_anchor;
        let mut tick = origin;
        let mut consumed_frames = 0_u64;
        let weak = state.native_audio_renderer.as_ref().unwrap().clone();
        let erased: std::sync::Weak<dyn mondrian_media::AudioPcmRenderer> = weak.clone();
        let AppAudioPlayback::Available(playback) = &state.audio_playback else {
            panic!("original Media Source");
        };
        assert!(playback.matches_source_weak(&erased));
        let renderer = weak.upgrade().expect("actual original Timeline renderer");
        let verifier = renderer.owner_installation_verifier().unwrap();
        let mut claim = verifier.current_claim().unwrap();
        let mut identity = cursor_facts(&state).session.unwrap().identity;
        let mut generation = state.audio_playback_snapshot().generation;
        let mut renderer_id = None;
        let mut object = None;
        let mut binding_ready = false;
        let mut phase_first = 0;
        let mut phase_anchor = 0_i64;
        let mut phase_consumed = 0;
        // Each group uses its actual Fresh anchor and actual appended extent.
        // Mode13's real callbacks, not a chosen window count, delimit the groups.
        let assert_pcm = |capture: &CaptureState, first: usize, anchor: i64| {
            let windows = &capture.windows[first..];
            for (block, window) in windows.iter().enumerate() {
                assert_eq!(window.clear_serial, windows[0].clear_serial);
                assert_eq!(window.samples.len(), scenario.chunk * 2);
                for (frame, pair) in window.samples.chunks_exact(2).enumerate() {
                    let sample = usize::try_from(anchor).unwrap() + block * scenario.chunk + frame;
                    assert!(sample < SOURCE_FRAMES, "finite original WAV coverage");
                    for (channel, expected) in input_sample(sample).into_iter().enumerate() {
                        assert!(
                            (pair[channel] - expected).abs() <= 1e-6,
                            "repeat public{sample} channel{channel}: {} != {expected}",
                            pair[channel]
                        );
                    }
                }
            }
            assert!(!capture.overflow);
            assert_eq!(capture.activation_discarded, 0);
            windows.len() * scenario.chunk
        };
        {
            let mut capture = output.lock();
            assert_eq!(
                capture.callback(INACTIVE_CALLBACK_FRAMES, tick),
                vec![[0.0; 2]; INACTIVE_CALLBACK_FRAMES]
            );
            assert_eq!(capture.snapshot.active_callback_consumed_frames, 0);
            assert!(capture.queue.is_empty());
        }
        crate::app::playback::tests::current_playback_ready_at(&mut state, tick);
        for cycle in 0..2 {
            let old_claim = claim.clone();
            let old_identity = identity;
            let old_epoch = state.playback_engine.snapshot().epoch;
            let next_lookahead = 7 + cycle * 4;
            let mut pending = None;
            let mut held_windows = None;
            let mut quiesced = false;
            let mut installed = None;
            let mut settled_claim = None;
            let mut next_identity = None;
            let mut old_lease = None;
            let mut fresh = None;
            let mut video_ready = false;
            loop {
                state.pump_audio_output_clocked_for_test(&mut || tick, &mut |state, event| {
                    match event {
                        AudioPlaybackEvent::NativeOwnerBindingsObserved {
                            renderer_id: actual, generation: observed, status,
                        } => {
                            assert_eq!(*status, AudioPcmNativeOwnerBindingStatus::Ready);
                            if let Some(expected) = renderer_id {
                                assert_eq!(*actual, expected);
                            } else {
                                renderer_id = Some(*actual);
                            }
                            if *observed == generation {
                                binding_ready = true;
                            } else {
                                assert!(installed.is_some(), "new bindings follow real settlement");
                            }
                        }
                        AudioPlaybackEvent::NativeReprepareRequired {
                            previous_generation, pending_generation, start_sample, notice, ..
                        } => {
                            assert!(binding_ready && pending.is_none());
                            assert_eq!(*previous_generation, generation);
                            assert_ne!(*pending_generation, generation);
                            assert_eq!(notice.request,
                                mondrian_core::NativeAudioReprepareRequest::ClapRestart);
                            if cycle == 1 {
                                assert!(phase_consumed > 0,
                                    "second request follows real Playing/FIFO consumption after first Fresh");
                            }
                            assert!(verifier.validate(&old_claim).is_ok());
                            let facts = cursor_facts(state);
                            assert_eq!(facts.session.unwrap().identity, old_identity);
                            assert_heads(&facts, generation, *start_sample,
                                Scenario { lookahead: 3 + cycle as i64 * 4, ..scenario });
                            assert_eq!(notice.instance_id,
                                facts.processor.unwrap().occurrence.instance_id);
                            let lease = actual_native_lease(&renderer, &facts);
                            let observed = renderer.state.lock().delivery
                                .observe_native_owner_object(lease)
                                .expect("authenticated held object before each real capture");
                            let observed = (observed.process_id(), observed.object_serial());
                            assert_ne!(observed.0.get(), std::process::id());
                            if let Some(expected) = object {
                                assert_eq!(observed, expected, "same native child and load incarnation");
                            } else {
                                object = Some(observed);
                            }
                            old_lease = Some(lease);
                            let capture = output.lock();
                            let published = assert_pcm(&capture, phase_first, phase_anchor);
                            assert!(published > 0);
                            assert_eq!(*start_sample, phase_anchor + published as i64);
                            let serial = capture.windows[phase_first].clear_serial;
                            let discarded: usize = capture.cleared.iter()
                                .filter(|discard| discard.clear_serial == serial)
                                .map(|discard| discard.frames).sum();
                            assert_eq!(published, phase_consumed + discarded,
                                "every old FIFO frame was really consumed or retired by original clear");
                            assert!(capture.queue.is_empty() && !capture.snapshot.active);
                            held_windows = Some(capture.windows.len());
                            pending = Some(*pending_generation);
                        }
                        AudioPlaybackEvent::NativeReprepareOutputQuiesced {
                            pending_generation, ..
                        } => {
                            assert_eq!(Some(*pending_generation), pending);
                            assert!(!quiesced);
                            let capture = output.lock();
                            assert!(capture.queue.is_empty() && !capture.snapshot.active);
                            assert_eq!(Some(capture.windows.len()), held_windows);
                            quiesced = true;
                        }
                        AudioPlaybackEvent::NativeReprepareObserved {
                            renderer_id: actual, generation: observed, status,
                        } => {
                            assert_eq!(Some(*actual), renderer_id);
                            assert_eq!(Some(*observed), pending);
                            assert!(quiesced && installed.is_none());
                            let AudioPcmNativeReprepareStatus::Settled { installed_generation } = status
                            else { panic!("both original work/retire/Settle chains must succeed: {status:?}"); };
                            let next_claim = verifier.current_claim().unwrap();
                            assert_eq!(next_claim.generation(), *installed_generation);
                            assert_ne!(next_claim.generation(), old_claim.generation());
                            assert!(verifier.validate(&old_claim).is_err());
                            let facts = cursor_facts(state);
                            let next = facts.session.unwrap().identity;
                            assert_ne!(next.session_id, old_identity.session_id);
                            assert_ne!(next.prepared_plan_id, old_identity.prepared_plan_id);
                            assert_eq!(facts.session.unwrap().state, AudioSessionCursorState::Unentered);
                            assert_eq!(facts.processor.unwrap().state, AudioProcessorCursorState::Unentered);
                            assert_eq!(facts.session.unwrap().public_output_lookahead_frames, next_lookahead);
                            let runtime = renderer.state.lock();
                            assert!(runtime.generation.is_none());
                            assert!(runtime.delivery.native_owner_lease(old_lease.unwrap().address()).is_err());
                            assert!(runtime.delivery.native_owner_lease(mondrian_audio::AudioNativeProcessorAddress {
                                root: next, local: next, occurrence: facts.processor.unwrap().occurrence,
                                epoch: None,
                            }).is_err());
                            drop(runtime);
                            let AppAudioPlayback::Available(playback) = &state.audio_playback else {
                                panic!("original Media");
                            };
                            assert_eq!(playback.native_owner_binding_status(), None);
                            let capture = output.lock();
                            assert!(capture.queue.is_empty() && !capture.snapshot.active);
                            assert_eq!(Some(capture.windows.len()), held_windows,
                                "install and real retirement grant no PCM while Unentered");
                            installed = Some(*installed_generation);
                            settled_claim = Some(next_claim);
                            next_identity = Some(next);
                        }
                        AudioPlaybackEvent::NativeDiagnosticsObserved { .. } => {}
                        other => panic!("no parameter/editor/author/recovery side path: {other:?}"),
                    }
                }).expect("original App poll/handler/provider and retirement queues");
                let snapshot = state.audio_playback_snapshot();
                assert_eq!(snapshot.render_substitution_count, 0);
                assert_eq!(snapshot.render_generation_recovery_count, 0);
                assert_eq!(snapshot.underrun_recovery_count, 0);
                assert_eq!(snapshot.active_interval_underrun_frames, 0);
                assert_eq!(snapshot.output.unwrap().underrun_frames, 0);
                assert!(!snapshot.output.unwrap().stream_failed);
                assert!(state.native_audio_renderer.as_ref().unwrap().ptr_eq(&weak));
                let AppAudioPlayback::Available(playback) = &state.audio_playback else {
                    panic!("original Media remains available");
                };
                assert!(
                    playback.matches_source_weak(&erased),
                    "same actual Source Arc in both cycles"
                );
                if installed.is_some() && fresh.is_none() {
                    assert!(snapshot.generation > pending.unwrap());
                    let anchor = state
                        .authoritative_audio_anchor(
                            &state.playback_engine,
                            state.playback_engine.monotonic_high_water(),
                            "repeat actual Fresh anchor",
                        )
                        .unwrap();
                    assert_eq!(snapshot.next_start_sample, anchor.sample());
                    assert!(
                        anchor.sample() >= 0 && (anchor.sample() as usize + 15) < SOURCE_FRAMES
                    );
                    fresh = Some((snapshot.generation, anchor.sample()));
                }
                if let Some((new_generation, anchor)) = fresh {
                    if snapshot.generation == new_generation
                        && snapshot.in_flight == 0
                        && snapshot.output.unwrap().buffered_frames == scenario.watermark
                    {
                        if !video_ready {
                            assert_eq!(
                                state.playback_engine.snapshot().state,
                                mondrian_playback::TransportState::Priming
                            );
                            assert_ne!(state.playback_engine.snapshot().epoch, old_epoch);
                            assert!(
                                !snapshot.output.unwrap().active,
                                "each Fresh needs its own current video Ready"
                            );
                            crate::app::playback::tests::current_playback_ready_at(
                                &mut state, tick,
                            );
                            video_ready = true;
                        } else {
                            assert!(
                                snapshot.activation_preroll_satisfied
                                    && snapshot.output.unwrap().active
                            );
                            assert_eq!(
                                state.playback_engine.snapshot().state,
                                mondrian_playback::TransportState::Playing
                            );
                            assert_eq!(snapshot.media_anchor.unwrap().sample(), anchor);
                            let capture = output.lock();
                            let published = assert_pcm(&capture, held_windows.unwrap(), anchor);
                            assert!(
                                published >= scenario.watermark,
                                "two complete Fresh public windows"
                            );
                            assert_eq!(published, capture.queue.len());
                            assert_restart_windows(&capture, held_windows.unwrap(), anchor);
                            drop(capture);
                            let facts = cursor_facts(&state);
                            assert_eq!(facts.session.unwrap().identity, next_identity.unwrap());
                            assert_heads(
                                &facts,
                                new_generation,
                                anchor + published as i64,
                                Scenario { lookahead: next_lookahead as i64, ..scenario },
                            );
                            let lease = actual_native_lease(&renderer, &facts);
                            assert_ne!(lease.binding(), old_lease.unwrap().binding());
                            // Settled identifies the Unentered installation. The original
                            // Media Fresh reprime publishes a newer claim on the same Source.
                            let fresh_claim = verifier.current_claim().unwrap();
                            assert_eq!(verifier.validate(&fresh_claim), Ok(()));
                            assert_eq!(lease.installed_root_serial(), fresh_claim.generation());
                            assert!(fresh_claim.generation() > installed.unwrap());
                            assert_eq!(
                                verifier.validate(settled_claim.as_ref().unwrap()),
                                Err(mondrian_core::AudioOwnerInstallationError::Superseded)
                            );
                            let observed = renderer
                                .state
                                .lock()
                                .delivery
                                .observe_native_owner_object(lease)
                                .expect(
                                    "authenticated object after each activated Fresh and full PCM",
                                );
                            assert_eq!(
                                Some((observed.process_id(), observed.object_serial())),
                                object
                            );
                            assert_eq!(
                                playback.native_owner_binding_status(),
                                Some(AudioPcmNativeOwnerBindingStatus::Ready)
                            );
                            assert!(playback.native_reprepare_pending_generation().is_none());
                            claim = fresh_claim;
                            identity = next_identity.unwrap();
                            generation = new_generation;
                            phase_first = held_windows.unwrap();
                            phase_anchor = anchor;
                            phase_consumed = 0;
                            binding_ready = true;
                            break;
                        }
                    }
                } else if pending.is_none() {
                    let mut capture = output.lock();
                    if capture.snapshot.active
                        && capture.queue.len() == scenario.watermark
                        && snapshot.in_flight == 0
                    {
                        assert_eq!(
                            state.playback_engine.snapshot().state,
                            mondrian_playback::TransportState::Playing
                        );
                        assert_pcm(&capture, phase_first, phase_anchor);
                        // Only the actual active callback moves this explicit software clock.
                        // Filling/pumping/waiting never fabricates consumed device frames.
                        consumed_frames += scenario.chunk as u64;
                        tick = origin
                            + Duration::from_nanos(
                                (consumed_frames * 1_000_000_000).div_ceil(u64::from(RATE)),
                            );
                        for (index, pair) in
                            capture.consume(scenario.chunk, tick).into_iter().enumerate()
                        {
                            assert_eq!(
                                pair,
                                input_sample(
                                    usize::try_from(phase_anchor).unwrap() + phase_consumed + index
                                )
                            );
                        }
                        phase_consumed += scenario.chunk;
                    }
                }
                assert_eq!(state.authoring.as_ref().unwrap().document(), &document);
                assert_eq!(state.project_author_generation(), author_generation);
                assert_eq!(state.authoring_history().unwrap().diagnostics(), history);
                assert!(Instant::now() < deadline,
                    "two-cycle restart incomplete: cycle={cycle}, {snapshot:?}; pending={pending:?} quiesced={quiesced} installed={installed:?} fresh={fresh:?} video_ready={video_ready}");
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(
                Instant::now() < deadline,
                "original total two-cycle work deadline"
            );
        }
        assert_eq!(
            cursor_facts(&state).session.unwrap().public_output_lookahead_frames,
            11
        );
        assert_eq!(state.authoring.as_ref().unwrap().document(), &document);
        assert_eq!(state.project_author_generation(), author_generation);
        assert_eq!(state.authoring_history().unwrap().diagnostics(), history);
        assert!(state.native_audio_editor.is_none());
        drop(renderer);
    }));
    let deadline = Instant::now() + Duration::from_secs(2);
    let pause = state.pause();
    let audio = state.shutdown_audio_playback_until(deadline);
    let cleanup = state.shutdown_for_endurance(deadline);
    assert!(
        pause.is_ok() && audio.all_workers_terminated() && cleanup.all_resources_released(),
        "complete repeated restart cleanup: pause={pause:?}, audio={audio:?}, app={cleanup:?}"
    );
    if let Err(payload) = target {
        std::panic::resume_unwind(payload);
    }
}
