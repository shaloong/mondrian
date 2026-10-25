//! Actual App/Media consumption of Playing, not a direct renderer invocation.
use super::*;

#[test]
#[ignore = "current Product, owned CLAP sidechain fixture and FFmpeg; software preroll only"]
fn app_owned_clap_media_preroll_enqueues_transport_pcm() {
    qualify(true, false);
}

#[test]
#[ignore = "current Product, owned VST3 sidechain fixture and FFmpeg; software preroll only"]
fn app_owned_vst3_media_preroll_enqueues_transport_pcm() {
    qualify(false, false);
}

#[test]
#[ignore = "current Product, owned CLAP meter fixture and FFmpeg; software preroll crosses 7/8 bar"]
fn app_owned_clap_media_preroll_enqueues_meter_pcm() {
    qualify(true, true);
}

#[test]
#[ignore = "current Product, owned VST3 meter fixture and FFmpeg; software preroll crosses 7/8 bar"]
fn app_owned_vst3_media_preroll_enqueues_meter_pcm() {
    qualify(false, true);
}

fn qualify(clap_format: bool, meter: bool) {
    let seek_frame = if meter { 63 } else { 1 };
    let start_sample = seek_frame * 2_000;
    let material = |name| {
        let path = std::path::PathBuf::from(std::env::var_os(name).expect("native input"));
        assert!(path.is_absolute() && path.is_file(), "{name}");
        path
    };
    let helper = material(if clap_format {
        "MONDRIAN_CLAP_TEST_HELPER"
    } else {
        "MONDRIAN_VST3_TEST_HELPER"
    });
    let clap = Arc::new(
        mondrian_audio::InstalledClapAudioProcessorSpecResolver::new(helper.clone()).unwrap(),
    );
    let vst3 =
        Arc::new(mondrian_audio::InstalledVst3AudioProcessorSpecResolver::new(helper).unwrap());
    let layout = AudioChannelLayout::Stereo;
    assert_eq!(crate::app::AUDIO_OUTPUT_LAYOUT, layout);
    let config = mondrian_media::AudioPlaybackConfig {
        sample_rate: 48_000,
        channel_layout: layout,
        chunk_frames: 4,
        preroll_frames: 4,
        high_watermark_frames: 4,
        max_in_flight: 1,
        ..mondrian_media::AudioPlaybackConfig::product_default()
    };
    let render = mondrian_audio::AudioRenderContract {
        sample_rate: config.sample_rate,
        channel_layout: layout,
        max_block_frames: config.chunk_frames,
        processing_mode: mondrian_audio::AudioProcessingMode::Realtime,
        processor_session_scratch_budget_bytes:
            mondrian_audio::AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
        public_output_lookahead_budget_frames:
            mondrian_audio::AudioRenderContract::DEFAULT_PUBLIC_OUTPUT_LOOKAHEAD_BUDGET_FRAMES,
        compensation_delay_scratch_budget_bytes:
            mondrian_audio::AudioRenderContract::DEFAULT_COMPENSATION_DELAY_SCRATCH_BUDGET_BYTES,
    };
    let mut instance = if clap_format {
        let descriptors =
            clap.install_library(material("MONDRIAN_CLAP_SIDECHAIN_TEST_PLUGIN")).unwrap();
        assert_eq!(descriptors.len(), 1);
        assert_eq!(descriptors[0].plugin_id, "org.mondrian.fixture.sidechain");
        let mut seed = vec![if meter { 16 } else { 6 }];
        seed.extend_from_slice(&0.5_f32.to_le_bytes());
        clap.create_instance(&descriptors[0].plugin_id, render, Some(seed)).unwrap()
    } else {
        let descriptors =
            vst3.install_plugin(material("MONDRIAN_VST3_SIDECHAIN_TEST_PLUGIN")).unwrap();
        assert_eq!(descriptors.len(), 1);
        // Fixture component state; the existing SDK captures the canonical snapshot.
        let mut seed = b"MSS1".to_vec();
        seed.extend_from_slice(&1_u32.to_le_bytes());
        seed.extend_from_slice(&0.5_f64.to_le_bytes());
        seed.extend_from_slice(&(if meter { 9_u32 } else { 3 }).to_le_bytes());
        vst3.create_instance(&descriptors[0].class_id, render, Some(seed)).unwrap()
    };
    instance.parameters.clear();
    let directory = tempfile::tempdir().unwrap();
    let library = AssetLibrary::open(directory.path().join("library")).unwrap();
    let mut sequence = Sequence::new("actual Media transport PCM");
    sequence.settings.frame_rate = mondrian_core::Rational::FPS_24;
    sequence.settings.audio_sample_rate = 48_000;
    sequence.settings.audio_channel_layout = layout;
    let reference = meter.then(|| {
        mondrian_core::MusicalTimeReference::new(
            TimelineTime::new(2, 48_000).unwrap(),
            TimelineTime::new(3, 4).unwrap(),
        )
        .unwrap()
        .with_time_signature(Some(
            mondrian_core::MusicalTimeSignature::new(7, 8).unwrap(),
        ))
    });
    super::super::project_state_roundtrip::initialize_nonzero_audio(
        &mut sequence,
        &library,
        directory.path(),
    );
    // Reuse the original32-frame WAV. Meter enters at S126000 and crosses the
    // bar at126002 inside the first actual four-frame Media request.
    sequence.audio_tracks[0].clips[0].position = TimelineTime::new(seek_frame, 24).unwrap();
    sequence.audio_program.outputs[0].strip.pre_fader.processors.push(instance);
    let mut state = AppState::with_native_audio_catalogs(clap, vst3);
    assert!(state.shutdown_audio_playback_and_wait().all_workers_terminated());
    let mut snapshot = audio_snapshot();
    snapshot.active = false;
    snapshot.buffered_frames = 0;
    snapshot.callback_consumed_frames = 0;
    snapshot.active_callback_consumed_frames = 0;
    snapshot.active_duration = None;
    snapshot.callback_count = 0;
    snapshot.last_callback_frames = 0;
    snapshot.last_callback_playback_delay = Some(Duration::ZERO);
    snapshot.last_callback_playback_delay_uncertainty = Some(Duration::ZERO);
    snapshot.last_callback_age = Some(Duration::ZERO);
    let output = Arc::new(parking_lot::Mutex::new(NativeHoldPlatformState {
        snapshot,
        revision: 0,
        acknowledged: 0,
        auto_acknowledge: true,
        queued: 0,
    }));
    let buffers = Arc::new(parking_lot::Mutex::new(Vec::with_capacity(2)));
    state.audio_playback = AppAudioPlayback::Available(Box::new(
        AudioPlayback::with_output(
            config,
            Box::new(RecordingOutput {
                original: NativeHoldPlatformOutput(output.clone()),
                buffers: buffers.clone(),
                window_limit: 2,
            }),
        )
        .unwrap(),
    ));
    state.test_set_sequence(Some(sequence));
    state.test_set_asset_library(Some(library.clone()));
    state.synchronize_playback_authoring_binding().unwrap();
    if meter {
        let generation = state.project_author_generation();
        state
            .dispatch_action(
                crate::app::ui_actions::sequence_set_musical_time_reference_action(
                    crate::app::product_action::SequenceSetMusicalTimeReferencePayload {
                        sequence_id: state.active_sequence_id().unwrap(),
                        reference,
                    },
                ),
            )
            .expect("author meter through the public transaction, not fixture seeding");
        assert_eq!(state.project_author_generation(), generation + 1);
        assert_eq!(
            state.active_sequence().unwrap().audio_program.musical_time_reference,
            reference
        );
    }
    let document = serde_json::to_value(state.authoring.as_ref().unwrap().document()).unwrap();
    let revision = library.database_revision().unwrap();
    let mut weak = std::sync::Weak::<TimelineAudioPcmRenderer>::new();
    let mut verifier = None;
    let mut handle: Option<OwnedHandle> = None;
    let target = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        state.seek(seek_frame).unwrap();
        assert_eq!(state.current_frame(), seek_frame);
        assert_eq!(
            state.audio_playback_snapshot().next_start_sample,
            start_sample
        );
        assert!(state.native_audio_renderer.is_none());
        let source = state.audio_playback_renderer("Media transport PCM").unwrap().unwrap();
        weak = Arc::downgrade(&source.renderer);
        assert!(source.renderer.owner_installation_verifier().is_none());
        let before = media_cursors(&source.renderer);
        assert_eq!(before.sessions[0].state, AudioSessionCursorState::Unentered);
        assert_eq!(
            before.processors[0].state,
            AudioProcessorCursorState::Unentered
        );
        state
            .commit_audio_playback(
                "Media transport PCM",
                audio_anchor(start_sample),
                Some(source),
            )
            .unwrap();
        let generation = state.audio_playback_snapshot().generation;
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut ready = false;
        while !ready {
            let poll = state
                .audio_playback
                .poll(AudioPlaybackMode::Idle, audio_anchor(start_sample))
                .unwrap();
            assert_eq!(poll.snapshot.generation, generation);
            assert_eq!(poll.snapshot.in_flight, 0);
            assert_eq!(poll.snapshot.next_start_sample, start_sample);
            for event in poll.events {
                if let AudioPlaybackEvent::NativeOwnerBindingsObserved { status, .. } = &event {
                    assert_eq!(
                        *status,
                        mondrian_media::AudioPcmNativeOwnerBindingStatus::Ready
                    );
                    ready = true;
                }
                handle_observation(&mut state, event);
            }
            assert!(
                Instant::now() < deadline,
                "native Source binding did not settle"
            );
            std::thread::yield_now();
        }
        let renderer = weak.upgrade().unwrap();
        assert_eq!(media_cursors(&renderer), before);
        assert!(buffers.lock().is_empty());
        let installed = renderer.owner_installation_verifier().unwrap();
        let claim = installed.current_claim().unwrap();
        installed.validate(&claim).unwrap();
        verifier = Some(installed.clone());
        let cold = media_owner(&renderer);
        let cold_object = renderer.observe_native_object_for_test(cold).unwrap();
        let pid = cold_object.process_id().get();
        assert_ne!(pid, std::process::id());
        // SAFETY: query/synchronize rights on the actual installed child's PID.
        let raw = unsafe { OpenProcess(0x0010_0000 | 0x1000, 0, pid) };
        assert!(!raw.is_null());
        handle = Some(unsafe { OwnedHandle::from_raw_handle(raw.cast()) });
        assert_eq!(
            unsafe { WaitForSingleObject(handle.as_ref().unwrap().as_raw_handle().cast(), 0) },
            258
        );
        drop(renderer);
        let mut entered_object = None;
        for (window, anchor) in [start_sample, start_sample + 4].into_iter().enumerate() {
            // Actual Media requests supply Playing. No direct positive render,
            // second Enter, physical callback or output-clock advance is injected.
            while buffers.lock().len() < window + 1
                || state.audio_playback_snapshot().in_flight != 0
            {
                let poll = state
                    .audio_playback
                    .poll(AudioPlaybackMode::Preroll, audio_anchor(anchor))
                    .unwrap();
                assert_eq!(poll.snapshot.generation, generation);
                assert!(poll.snapshot.next_start_sample <= anchor + 4);
                assert!(!poll.snapshot.output.unwrap().active);
                for event in poll.events {
                    handle_observation(&mut state, event);
                }
                assert!(
                    Instant::now() < deadline,
                    "actual PCM windows did not enqueue"
                );
                std::thread::yield_now();
            }
            assert_eq!(buffers.lock().len(), window + 1);
            assert_eq!(output.lock().queued, (window + 1) * 4);
            let renderer = weak.upgrade().unwrap();
            let lease = media_owner(&renderer);
            assert_eq!(lease.editor_target().continuity_epoch, Some(generation));
            assert_ne!(lease.binding(), cold.binding());
            assert!(renderer.observe_native_object_for_test(cold).is_err());
            let current = renderer.observe_native_object_for_test(lease).unwrap();
            assert_eq!(current.process_id(), cold_object.process_id());
            if let Some(first) = entered_object {
                assert_eq!(
                    current, first,
                    "Continue must retain the entered native object"
                );
            } else {
                // VST3 may reconstruct its component at the first nonzero entry.
                // Its standard-specific entry is distinct from steady Continue.
                entered_object = Some(current);
            }
            assert_eq!(
                installed.current_claim().unwrap().generation(),
                claim.generation()
            );
            installed.validate(&claim).unwrap();
            let facts = media_cursors(&renderer);
            assert!(matches!(facts.sessions[0].state,
                AudioSessionCursorState::Active { epoch, next_public_sample, next_execution_sample, .. }
                if epoch == mondrian_audio::AudioContinuityEpoch::new(generation)
                    && next_public_sample.sample() == anchor + 4
                    && next_execution_sample.sample() == anchor + 7));
            assert!(matches!(facts.processors[0].state,
                AudioProcessorCursorState::Active { epoch, next_input_signal_sample }
                if epoch == mondrian_audio::AudioContinuityEpoch::new(generation)
                    && next_input_signal_sample.sample() == anchor + 7));
            renderer
                .validate_native_author_source_closed(&ExecutionCancellationToken::new())
                .unwrap();
        }
        let recorded = buffers.lock();
        assert_eq!(recorded.len(), 2);
        for (window, buffer) in recorded.iter().enumerate() {
            assert_eq!(buffer.sample_rate, 48_000);
            assert_eq!(buffer.channel_layout, layout);
            for (offset, frame) in buffer.samples.chunks_exact(2).enumerate() {
                let sample = start_sample as usize + window * 4 + offset;
                // Independent public-sample oracle; no DLL/adapter helper.
                let meter_gain = if meter {
                    80.0 / 1024.0
                        + if sample < 126_002 {
                            1.0 / 16.0
                        } else {
                            1.0 / 8.0
                        }
                } else {
                    0.0
                };
                let expected = 0.5 * (0.625 + (sample % 251) as f32 / 1024.0 + meter_gain);
                assert_eq!(frame, [expected, -expected], "public sample {sample}");
            }
        }
        assert_eq!(
            state.audio_playback_snapshot().next_start_sample,
            start_sample + 8
        );
        assert_eq!(library.database_revision().unwrap(), revision);
        assert_eq!(
            serde_json::to_value(state.authoring.as_ref().unwrap().document()).unwrap(),
            document
        );
        assert!(!state.authoring.as_ref().unwrap().has_pending_native_authoring());
        assert!(state.native_audio_editor.is_none());
    }));
    let cleanup = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let evidence = state.shutdown_audio_playback_and_wait();
        assert!(evidence.all_workers_terminated(), "{evidence:?}");
        assert!(evidence.shutdown_resource_facts_complete_at_deadline);
        assert!(!evidence.shutdown_owner_lifetime_unresolved_at_deadline);
        assert_eq!(evidence.foreign_owner_panics, 0);
        assert_eq!(evidence.foreign_owner_abandonments, 0);
        assert!(weak.upgrade().is_none());
        if let Some(verifier) = verifier {
            assert!(matches!(
                verifier.current_claim(),
                Err(mondrian_core::AudioOwnerInstallationError::NotInstalled)
            ));
        }
        if let Some(handle) = handle {
            // SAFETY: the query handle is kept open from before the first Enter.
            assert_eq!(
                unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) },
                0
            );
            let mut exit = u32::MAX;
            assert_ne!(
                unsafe { GetExitCodeProcess(handle.as_raw_handle().cast(), &mut exit) },
                0
            );
            assert_eq!(exit, 0);
        }
    }));
    if let Err(primary) = target {
        std::panic::resume_unwind(primary);
    }
    if let Err(error) = cleanup {
        std::panic::resume_unwind(error);
    }
}

fn handle_observation(state: &mut AppState, event: AudioPlaybackEvent) {
    assert!(
        matches!(
            event,
            AudioPlaybackEvent::NativeDiagnosticsObserved { .. }
                | AudioPlaybackEvent::NativeParameterObservationsObserved { .. }
                | AudioPlaybackEvent::NativeOwnerBindingsObserved { .. }
        ),
        "unexpected event: {event:?}"
    );
    state.handle_audio_playback_event(event, Instant::now()).unwrap();
}

fn media_cursors(renderer: &TimelineAudioPcmRenderer) -> Cursors {
    renderer.with_delivery_observation_for_test(|delivery| {
        let mut facts = Cursors::default();
        delivery.visit_continuity_evidence(&mut facts).unwrap();
        assert_eq!((facts.sessions.len(), facts.processors.len()), (1, 1));
        assert!(delivery.requires_state_entry());
        assert_eq!(facts.sessions[0].public_output_lookahead_frames, 3);
        assert!(delivery.resource_footprint().media_window_bytes > 0);
        facts
    })
}

fn media_owner(renderer: &TimelineAudioPcmRenderer) -> AudioNativeProcessorOwnerLease {
    let facts = media_cursors(renderer);
    let processor = facts.processors[0];
    let epoch = match processor.state {
        AudioProcessorCursorState::Unentered => None,
        AudioProcessorCursorState::Active { epoch, .. } => Some(epoch),
        other => panic!("unexpected native Media cursor: {other:?}"),
    };
    renderer.with_delivery_observation_for_test(|delivery| {
        delivery
            .native_owner_lease(AudioNativeProcessorAddress {
                root: facts.sessions[0].identity,
                local: processor.identity,
                occurrence: processor.occurrence,
                epoch,
            })
            .unwrap()
    })
}
