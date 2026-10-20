//! Owned VST3 startup/priming restart through the original App/Media queues.
//! The first Process can request latency reprepare before any public PCM exists.
//! Finite software output only, without physical-device or realtime qualification.
use super::*;
use mondrian_audio::DiscoveredVst3AudioProcessorSpecResolver;
use mondrian_media::{
    AudioPcmNativeOwnerBindingStatus, AudioPcmNativeReprepareStatus, AudioPlaybackEvent,
};

const RESTART: Scenario = Scenario {
    mode: 5,
    chunk: 4,
    watermark: 8,
    capture_windows: 8,
    output_capacity: 16,
    lookahead: 3,
};

fn fixture() -> (
    tempfile::TempDir,
    AppState,
    Arc<Mutex<CaptureState>>,
    mondrian_core::ProgramOutputId,
    mondrian_core::ParameterId,
) {
    let path = |name| {
        let path = std::env::var_os(name).map(std::path::PathBuf::from).expect(name);
        assert!(path.is_absolute() && path.is_file(), "{name}");
        path
    };
    let catalog = Arc::new(
        DiscoveredVst3AudioProcessorSpecResolver::discover(
            path("MONDRIAN_VST3_TEST_HELPER"),
            [path("MONDRIAN_VST3_SIDECHAIN_TEST_PLUGIN")],
        )
        .expect("owned VST3 discovery"),
    );
    // Owned fixture MSS1 v1 component/controller envelope, gain .5 and mode5.
    // This is test input to the original concrete catalog, not a second codec.
    let mut component = Vec::from(*b"MSS1");
    component.extend_from_slice(&1_u32.to_le_bytes());
    component.extend_from_slice(&0.5_f64.to_le_bytes());
    component.extend_from_slice(&5_u32.to_le_bytes());
    let mut configuration = Vec::from(*b"VST3HOST_STATE\0\0");
    configuration.extend_from_slice(&1_u32.to_le_bytes());
    configuration.extend_from_slice(&(component.len() as u32).to_le_bytes());
    configuration.extend_from_slice(&(component.len() as u32).to_le_bytes());
    configuration.extend_from_slice(&component);
    configuration.extend_from_slice(&component);
    let instance = catalog
        .create_instance(
            &catalog.descriptors()[0].class_id,
            reference_render_contract(),
            Some(configuration.clone()),
        )
        .expect("actual owned mode5 component/controller state");
    assert_eq!(
        instance.opaque_state.as_ref().map(|state| state.as_slice()),
        Some(configuration.as_slice())
    );
    assert_eq!(instance.parameters.len(), 1);
    let parameter = mondrian_core::ParameterId::new("vst3.param.0").unwrap();
    assert_eq!(instance.parameters.keys().next(), Some(&parameter));
    let resolver = Arc::new(IsolatedAudioProcessorResolver::new(catalog));
    let (directory, state, output, output_id) =
        native_fader_fixture_with_processor(RESTART, instance, resolver);
    (directory, state, output, output_id, parameter)
}

fn assert_windows(output: &CaptureState, first: usize, count: usize, anchor: i64) {
    let windows = &output.windows[first..first + count];
    for (block, window) in windows.iter().enumerate() {
        assert_eq!(window.samples.len(), 8);
        assert_eq!(window.clear_serial, windows[0].clear_serial);
        for (offset, pair) in window.samples.chunks_exact(2).enumerate() {
            let sample = usize::try_from(anchor).unwrap() + block * 4 + offset;
            assert!(sample < SOURCE_FRAMES, "finite actual WAV coverage");
            // Actual main weight2 times gain.5; auxiliary buses are unconnected.
            for (channel, expected) in input_sample(sample).into_iter().enumerate() {
                assert!(
                    (pair[channel] - expected).abs() <= 1e-6,
                    "PDC public{sample} channel{channel}: {} != {expected}",
                    pair[channel]
                );
            }
        }
    }
}

fn assert_gain(
    observation: &mondrian_core::NativeAudioParameterObservation,
    parameter: &mondrian_core::ParameterId,
) {
    assert_eq!(observation.facts().len(), 1);
    let fact = &observation.facts()[0];
    assert_eq!(&fact.parameter, parameter);
    assert_eq!(fact.name.as_deref(), Some("Gain"));
    assert_eq!(fact.value_bits, Some(0.5_f64.to_bits()));
    assert_eq!(fact.info_flags, Some(1), "actual VST3 kCanAutomate");
    assert_eq!(fact.event_flags, 0);
    assert_eq!(fact.hidden, Some(false));
    assert_eq!(fact.display.as_deref(), Some("0.5"));
    assert_eq!(fact.periodic, None);
    assert_eq!(fact.module, None);
    assert!(fact.revision <= observation.source_revision());
    assert_eq!(
        fact.origin,
        mondrian_core::NativeAudioParameterObservationOrigin::MainQuery { rescan_flags: 0 }
    );
}

#[test]
#[ignore = "current normal Product and owned VST3 mode5/6; App/Media priming restart and software output, no physical DAC"]
fn native_vst3_priming_restart_installs_same_source_and_enters_fresh_pdc_pcm() {
    let (_directory, mut state, output, _output_id, parameter) = fixture();
    let target = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let document = state.authoring.as_ref().unwrap().document().clone();
        let author_generation = state.project_author_generation();
        let history = state.authoring_history().unwrap().diagnostics();
        let deadline = Instant::now() + Duration::from_secs(30);
        state.play().expect("original public play and actual VST3 Source");
        // Only real FIFO consumption advances this functional software clock.
        // Native deadlines and the work/cleanup bounds retain real wall time.
        let origin = state.playback_observation_instant_anchor;
        let mut tick = origin;
        let mut consumed_frames = 0_u64;
        let old_generation = state.audio_playback_snapshot().generation;
        let weak = state.native_audio_renderer.as_ref().unwrap().clone();
        let erased: std::sync::Weak<dyn AudioPcmRenderer> = weak.clone();
        let renderer = weak.upgrade().expect("original actual Timeline renderer");
        let verifier = renderer.owner_installation_verifier().unwrap();
        let old_claim = verifier.current_claim().unwrap();
        let old_identity = cursor_facts(&state).session.unwrap().identity;
        {
            let mut capture = output.lock();
            assert_eq!(
                capture.callback(INACTIVE_CALLBACK_FRAMES, tick),
                vec![[0.0; 2]; INACTIVE_CALLBACK_FRAMES]
            );
            assert!(capture.queue.is_empty());
            assert_eq!(capture.snapshot.active_callback_consumed_frames, 0);
        }
        crate::app::playback::tests::current_playback_ready_at(&mut state, tick);
        let original_playback_epoch = state.playback_engine.snapshot().epoch;
        let mut renderer_id = None;
        let mut old_lease = None;
        let mut old_object = None;
        let mut pending = None;
        let mut old_windows = None;
        let mut quiesced = false;
        let mut installed = None;
        let mut next_identity = None;
        let mut fresh = None;
        let mut fresh_ready = false;
        let mut fresh_object = None;
        let mut fresh_parameter = None;
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
                            assert!(renderer_id.replace(*actual).is_none());
                        } else {
                            assert_eq!(Some(*actual), renderer_id);
                            assert!(installed.is_some(), "new Ready follows actual settlement");
                        }
                    }
                    AudioPlaybackEvent::NativeParameterObservationsObserved {
                        renderer_id: actual,
                        generation,
                        observation,
                    } => {
                        assert_eq!(Some(*actual), renderer_id);
                        assert_gain(observation, &parameter);
                        if observation.target().root_session != old_identity.session_id {
                            assert!(installed.is_some());
                            fresh_parameter = Some((*generation, (**observation).clone()));
                        }
                    }
                    AudioPlaybackEvent::NativeReprepareRequired {
                        previous_generation,
                        pending_generation,
                        start_sample,
                        notice,
                        ..
                    } => {
                        assert!(renderer_id.is_some() && pending.is_none());
                        assert_eq!(*previous_generation, old_generation);
                        assert_ne!(*pending_generation, old_generation);
                        assert_eq!(
                            notice.request,
                            mondrian_core::NativeAudioReprepareRequest::Vst3Restart { flags: 8 }
                        );
                        let facts = cursor_facts(state);
                        assert_eq!(facts.session.unwrap().identity, old_identity);
                        assert_eq!(facts.session.unwrap().public_output_lookahead_frames, 3);
                        let lease = actual_native_lease(&renderer, &facts);
                        old_lease = Some(lease);
                        let object = renderer
                            .state
                            .lock()
                            .delivery
                            .observe_native_owner_object(lease)
                            .expect("actual held native object before capture");
                        assert_ne!(object.process_id().get(), std::process::id());
                        old_object = Some(object);
                        assert_eq!(
                            notice.instance_id,
                            facts.processor.unwrap().occurrence.instance_id
                        );
                        let capture = output.lock();
                        let count = capture.windows.len();
                        // Priming can trigger before any complete public block;
                        // freeze the real published prefix instead of forcing8.
                        assert_eq!(*start_sample, (count * RESTART.chunk) as i64);
                        assert_windows(&capture, 0, count, 0);
                        assert!(capture.queue.is_empty() && !capture.snapshot.active);
                        old_windows = Some(count);
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
                        assert_eq!(Some(capture.windows.len()), old_windows);
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
                            panic!("actual retained VST3 restart must settle: {status:?}");
                        };
                        let claim = verifier.current_claim().unwrap();
                        assert_eq!(claim.generation(), *installed_generation);
                        assert_ne!(claim.generation(), old_claim.generation());
                        assert!(verifier.validate(&old_claim).is_err());
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
                        assert!(runtime
                            .delivery
                            .native_owner_lease(mondrian_audio::AudioNativeProcessorAddress {
                                root: next,
                                local: next,
                                occurrence: facts.processor.unwrap().occurrence,
                                epoch: None,
                            })
                            .is_err());
                        drop(runtime);
                        let AppAudioPlayback::Available(playback) = &state.audio_playback else {
                            panic!("same Media");
                        };
                        assert_eq!(
                            playback.native_owner_binding_status(),
                            None,
                            "old Ready revoked"
                        );
                        let capture = output.lock();
                        assert!(capture.queue.is_empty() && !capture.snapshot.active);
                        assert_eq!(
                            Some(capture.windows.len()),
                            old_windows,
                            "no PCM from install or retirement"
                        );
                        next_identity = Some(next);
                        installed = Some(*installed_generation);
                    }
                    AudioPlaybackEvent::NativeDiagnosticsObserved { .. } => {}
                    other => panic!("no editor/author/substitution/recovery side path: {other:?}"),
                })
                .expect("original App poll/handler/provider/retirement pipeline");
            let snapshot = state.audio_playback_snapshot();
            assert_eq!(snapshot.render_substitution_count, 0);
            assert_eq!(snapshot.render_generation_recovery_count, 0);
            assert_eq!(snapshot.underrun_recovery_count, 0);
            assert!(!snapshot.output.unwrap().stream_failed);
            assert!(state.native_audio_renderer.as_ref().unwrap().ptr_eq(&weak));
            let AppAudioPlayback::Available(playback) = &state.audio_playback else {
                panic!("same Media");
            };
            assert!(
                playback.matches_source_weak(&erased),
                "same actual Media Source Arc"
            );
            if installed.is_some() && fresh.is_none() {
                assert!(snapshot.generation > pending.unwrap());
                let anchor = state
                    .authoritative_audio_anchor(
                        &state.playback_engine,
                        state.playback_engine.monotonic_high_water(),
                        "owned VST3 priming restart Fresh anchor",
                    )
                    .unwrap();
                assert_eq!(snapshot.next_start_sample, anchor.sample());
                assert!(anchor.sample() >= 0 && (anchor.sample() as usize + 15) < SOURCE_FRAMES);
                fresh = Some((snapshot.generation, anchor.sample()));
            }
            if pending.is_none() && consumed_frames == 0 {
                let mut capture = output.lock();
                if capture.snapshot.active && capture.queue.len() == RESTART.watermark {
                    consumed_frames += RESTART.chunk as u64;
                    tick = origin
                        + Duration::from_nanos(
                            (consumed_frames * 1_000_000_000).div_ceil(u64::from(RATE)),
                        );
                    let consumed = capture.consume(RESTART.chunk, tick);
                    for (index, pair) in consumed.into_iter().enumerate() {
                        assert_eq!(pair, input_sample(index));
                    }
                }
            }
            if let Some((generation, anchor)) = fresh {
                if snapshot.generation == generation
                    && snapshot.in_flight == 0
                    && snapshot.output.unwrap().buffered_frames == RESTART.watermark
                {
                    if !fresh_ready {
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
                            "PCM does not grant current video Ready"
                        );
                        crate::app::playback::tests::current_playback_ready_at(&mut state, tick);
                        fresh_ready = true;
                    } else {
                        assert!(
                            snapshot.activation_preroll_satisfied
                                && snapshot.output.unwrap().active
                        );
                        assert_eq!(snapshot.media_anchor.unwrap().sample(), anchor);
                        let capture = output.lock();
                        assert_eq!(capture.windows.len(), old_windows.unwrap() + 2);
                        assert_windows(&capture, 0, old_windows.unwrap(), 0);
                        assert_windows(&capture, old_windows.unwrap(), 2, anchor);
                        assert!(!capture.overflow);
                        assert_eq!(capture.activation_discarded, 0);
                        drop(capture);
                        if fresh_object.is_none() {
                            let facts = cursor_facts(&state);
                            let lease = actual_native_lease(&renderer, &facts);
                            let object = renderer
                                .state
                                .lock()
                                .delivery
                                .observe_native_owner_object(lease)
                                .expect("actual object after Fresh native reload");
                            assert_eq!(object.process_id(), old_object.unwrap().process_id());
                            assert!(object.object_serial() > old_object.unwrap().object_serial());
                            fresh_object = Some(object);
                        }
                        assert!(
                            Instant::now() < deadline,
                            "native identity observation exceeded the original restart deadline"
                        );
                        // Presentation is a later natural Media DrainAuthor DTO,
                        // never the prerequisite for submitting current Ready.
                        if fresh_parameter
                            .as_ref()
                            .is_some_and(|(observed, _)| *observed == generation)
                        {
                            break;
                        }
                    }
                }
            }
            assert!(Instant::now() < deadline,
                "VST3 priming restart incomplete: {snapshot:?}; pending={pending:?} old_windows={old_windows:?} quiesced={quiesced} installed={installed:?} fresh={fresh:?} Ready={fresh_ready} object={fresh_object:?} parameter={}",fresh_parameter.is_some());
            std::thread::sleep(Duration::from_millis(1));
        }
        let (generation, anchor) = fresh.unwrap();
        let facts = cursor_facts(&state);
        assert_eq!(facts.session.unwrap().identity, next_identity.unwrap());
        assert_heads(
            &facts,
            generation,
            anchor + 8,
            Scenario { lookahead: 7, ..RESTART },
        );
        let lease = actual_native_lease(&renderer, &facts);
        assert_ne!(lease.binding(), old_lease.unwrap().binding());
        assert_eq!(
            lease.installed_root_serial(),
            verifier.current_claim().unwrap().generation()
        );
        assert!(renderer
            .state
            .lock()
            .delivery
            .native_owner_lease(old_lease.unwrap().address())
            .is_err());
        let (_, observation) = fresh_parameter.unwrap();
        assert_eq!(observation.target(), lease.editor_target());
        assert_eq!(observation.process_id(), fresh_object.unwrap().process_id());
        assert_eq!(
            observation.object_serial(),
            fresh_object.unwrap().object_serial()
        );
        assert_gain(&observation, &parameter);
        assert_eq!(state.authoring.as_ref().unwrap().document(), &document);
        assert_eq!(state.project_author_generation(), author_generation);
        assert_eq!(state.authoring_history().unwrap().diagnostics(), history);
        assert!(state.native_audio_editor.is_none());
        let AppAudioPlayback::Available(playback) = &state.audio_playback else {
            panic!("same Media");
        };
        assert!(playback.native_reprepare_pending_generation().is_none());
        assert_eq!(
            state.playback_engine.snapshot().state,
            mondrian_playback::TransportState::Playing
        );
        println!("owned VST3 priming restart: old={old_generation} pending={pending:?} fresh={generation} A={anchor} old_windows={old_windows:?} PDC=3->7 old={:?} fresh={fresh_object:?}",old_object.unwrap());
        drop(renderer);
    }));
    let deadline = Instant::now() + Duration::from_secs(2);
    let pause = state.pause();
    let audio = state.shutdown_audio_playback_until(deadline);
    let cleanup = state.shutdown_for_endurance(deadline);
    assert!(
        pause.is_ok() && audio.all_workers_terminated() && cleanup.all_resources_released(),
        "complete owned VST3 restart cleanup: pause={pause:?}, audio={audio:?}, app={cleanup:?}"
    );
    if let Err(payload) = target {
        std::panic::resume_unwind(payload);
    }
}
