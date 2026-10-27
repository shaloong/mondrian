//! Positive topology-change qualification. A refusal is a failure of this gate.
use super::*;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, WaitForSingleObject};

#[test]
#[ignore = "current production helper and CLAP mode19; dynamic native I/O migration is not implemented"]
fn installed_clap_runtime_restart_rebinds_mono_detector_without_replacing_native_owner() {
    let catalog = Arc::new(
        DiscoveredClapAudioProcessorSpecResolver::discover(
            path("MONDRIAN_CLAP_TEST_HELPER"),
            [path("MONDRIAN_CLAP_SIDECHAIN_TEST_PLUGIN")],
        )
        .expect("actual native catalog"),
    );
    let render = AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Stereo,
        max_block_frames: 4,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes: 32 * 1024 * 1024,
        public_output_lookahead_budget_frames: 1024,
        compensation_delay_scratch_budget_bytes: 1024 * 1024,
    };
    let mut state = vec![19];
    state.extend_from_slice(&0.5_f32.to_le_bytes());
    let instance = catalog
        .create_instance("org.mondrian.fixture.sidechain", render, Some(state))
        .expect("initial stereo Main / mono detector activation");
    assert!(instance.parameters.is_empty());
    let mut sequence = sequence_with_audio_clip();
    sequence.settings.audio_channel_layout = AudioChannelLayout::Stereo;
    sequence.settings.audio_sample_rate = render.sample_rate;
    let occurrence = AudioProcessorOccurrence {
        instance_id: instance.id,
        owner: AudioProcessorOccurrenceOwner::Output(sequence.audio_program.outputs[0].id),
        insertion: AudioProcessorInsertionPoint::PreFader,
    };
    sequence.audio_program.sidechain_routes.push(AudioProcessorSidechainRoute::new(
        AudioRouteSource::Track {
            track_id: sequence.audio_tracks[0].id,
            port: AudioChannelStripOutputPort::PostMute,
        },
        instance.id,
        "clap.audio-input.11",
    ));
    sequence.audio_program.outputs[0].strip.pre_fader.processors.push(instance);
    let author_before = serde_json::to_value(&sequence).unwrap();
    let mut runtime = AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
        &sequence,
        &[],
        &StereoRampResolver,
        &IsolatedAudioProcessorResolver::new(catalog),
        render,
        None,
        AudioRuntimeResourceGrant::new(1, 64 * 1024 * 1024, 32 * 1024 * 1024),
    )
    .expect("initial canonical DAG");
    let source = AudioOwnerInstallationSource::new();
    let (registration, verifier) = source.register().unwrap();
    runtime.bind_native_owner_installation(verifier.clone()).unwrap();
    let claim = source
        .reserve_install(&registration, None)
        .and_then(|reservation| source.begin_commit(reservation))
        .unwrap()
        .commit()
        .unwrap();
    let cancellation = ExecutionCancellationToken::new();
    runtime.install_native_owner_bindings(&claim, &cancellation).unwrap();
    runtime
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(1),
            start_sample: 0,
        })
        .unwrap();
    let initial = cursors(&runtime);
    let address = AudioNativeProcessorAddress {
        root: initial.session.unwrap().identity,
        local: initial.processor.unwrap().identity,
        occurrence,
        epoch: Some(AudioContinuityEpoch::new(1)),
    };
    let lease = runtime.native_owner_lease(address).unwrap();
    let object = runtime.observe_native_owner_object(lease).unwrap();
    let pid = object.process_id().get();
    assert_ne!(pid, std::process::id());
    // SAFETY: the authenticated original owner supplies this PID. This handle
    // can query/wait only and is closed by OwnedHandle after all assertions.
    let raw = unsafe { OpenProcess(0x0010_0000 | 0x1000, 0, pid) };
    assert!(!raw.is_null());
    let handle = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert_eq!(runtime.public_output_lookahead_frames(), 3);
        for start in [0, 4] {
            let mut pcm = [99.0; 8];
            runtime
                .render_into(
                    AudioRenderRequest {
                        transport_activity: None,
                        start_sample: start,
                        frames: 4,
                    },
                    &mut pcm,
                )
                .expect("complete old mono-detector window");
            // [r,-r] folds to zero in the mono detector; Main weight2 * gain.5.
            assert_pcm(&pcm, start, 1.0);
        }
        let mut pcm = [99.0; 8];
        let ticket = match runtime.render_into(
            AudioRenderRequest {
                transport_activity: None,
                start_sample: 8,
                frames: 4,
            },
            &mut pcm,
        ) {
            Err(AudioExecutionError::ReprepareRequired(ticket)) => ticket,
            other => panic!("actual native restart must hold publication: {other:?}"),
        };
        assert_eq!(pcm, [99.0; 8]);
        let held = cursors(&runtime);
        let operation = runtime
            .service_native_reprepare(ticket, &cancellation)
            .expect("capture changed native detector contract after deactivation");
        let mut expected_state = vec![0];
        expected_state.extend_from_slice(&0.5_f32.to_le_bytes());
        assert_eq!(
            runtime.native_reprepare_configuration(&operation).unwrap(),
            expected_state
        );
        runtime
            .prepare_native_reprepare_plan(&operation, verifier.clone(), &cancellation)
            .expect("prepare changed routing and buffer geometry under original Root grant");
        assert_eq!(cursors(&runtime), held);
        assert_eq!(runtime.observe_native_owner_object(lease).unwrap(), object);
        let mut authorization = source
            .authorize_runtime_installation(
                source.reserve_install(&registration, Some(&claim)).unwrap(),
            )
            .unwrap();
        let receipt = runtime
            .install_native_reprepare_plan(&operation, &mut authorization, &cancellation)
            .expect("same-owner native mapping and canonical plan installation");
        assert_ne!(receipt.claim().generation(), claim.generation());
        assert_eq!(runtime.public_output_lookahead_frames(), 3);
        assert_eq!(
            runtime.render_into(
                AudioRenderRequest {
                    transport_activity: None,
                    start_sample: 8,
                    frames: 4,
                },
                &mut pcm
            ),
            Err(AudioExecutionError::StateEntryRequired)
        );
        assert_eq!(pcm, [99.0; 8]);
        runtime
            .enter_state(AudioStateEntry {
                epoch: AudioContinuityEpoch::new(2),
                start_sample: 8,
            })
            .unwrap();
        let next = cursors(&runtime);
        let new_lease = runtime
            .native_owner_lease(AudioNativeProcessorAddress {
                root: next.session.unwrap().identity,
                local: next.processor.unwrap().identity,
                occurrence,
                epoch: Some(AudioContinuityEpoch::new(2)),
            })
            .unwrap();
        assert_eq!(
            runtime.observe_native_owner_object(new_lease).unwrap(),
            object
        );
        for (start, frames) in [(8, 1), (9, 3), (12, 4)] {
            let mut pcm = vec![99.0; frames * 2];
            runtime
                .render_into(
                    AudioRenderRequest {
                        transport_activity: None,
                        start_sample: start,
                        frames,
                    },
                    &mut pcm,
                )
                .expect("new stereo detector PCM");
            // Native port 11 keeps its identity and now receives both channels.
            assert_pcm(&pcm, start, 2.5);
        }
        runtime.finish_native_reprepare_retirement(receipt.retire()).unwrap();
        assert_eq!(serde_json::to_value(&sequence).unwrap(), author_before);
    }));
    // Keep failure reporting after original-owner cleanup, including RED gates.
    drop(runtime);
    let settled = unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) };
    let mut exit = 0;
    let queried = unsafe { GetExitCodeProcess(handle.as_raw_handle().cast(), &mut exit) };
    println!(
        "{}",
        serde_json::json!({"case":"clap-dynamic-detector-width",
        "native_pid":pid,"post_drop_wait_status":settled,
        "exit_query_succeeded":queried!=0,"exit_code":exit})
    );
    assert_eq!(
        settled, 0,
        "original child must settle even when qualification fails"
    );
    assert_ne!(queried, 0);
    assert_eq!(exit, 0);
    if let Err(payload) = outcome {
        std::panic::resume_unwind(payload);
    }
}

fn assert_pcm(pcm: &[f32], start: i64, gain: f32) {
    for (offset, pair) in pcm.chunks_exact(2).enumerate() {
        let expected = (start + offset as i64 + 1) as f32 * gain;
        assert_eq!(pair, [expected, -expected]);
    }
}
