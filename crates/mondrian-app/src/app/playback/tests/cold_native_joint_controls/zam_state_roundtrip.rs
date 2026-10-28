//! Real nondefault opaque state through public Project save/reopen and the
//! same App-created Timeline/Delivery source, with native-owned values and a
//! conflicting authored constant. No plugin GUI or alternate DSP executor.
use super::project_state_roundtrip::{initialize_nonzero_audio, unentered_owner};
use super::*;
use mondrian_core::{AudioOwnerInstallationError, ExecutionCancellationToken, ParameterId};
use mondrian_media::{
    AudioPcmContinuity, AudioPcmRenderGeneration, AudioPcmRenderOutcome, AudioPcmRenderRequest,
    AudioPcmRenderer,
};

const PLUGIN: &str = "com.zamaudio.ZamCompX2";

fn observe_restored_cold_state(
    state: &mut AppState,
    weak: &std::sync::Weak<TimelineAudioPcmRenderer>,
    instance: mondrian_core::AudioProcessorInstanceId,
) {
    let renderer = weak.upgrade().expect("the original App/Media source");
    let prepared = unentered_owner(&renderer, instance);
    let generation = state.audio_playback_snapshot().generation;
    let mut ready = false;
    let mut value = false;
    poll_idle_until(state, generation, |event| {
        assert_eq!(unentered_owner(&renderer, instance), prepared);
        match event {
            AudioPlaybackEvent::NativeOwnerBindingsObserved { status, .. } => {
                assert_eq!(
                    *status,
                    mondrian_media::AudioPcmNativeOwnerBindingStatus::Ready
                );
                ready = true;
            }
            AudioPlaybackEvent::NativeParameterObservationsObserved { observation, .. } => {
                assert_eq!(observation.target().instance, instance);
                assert_eq!(
                    observation.facts().len(),
                    11,
                    "native metadata includes both readonly meters"
                );
                assert!(observation.target().continuity_epoch.is_none());
                let makeup = ParameterId::new("clap.param.5").unwrap();
                let fact =
                    observation.facts().iter().find(|fact| fact.parameter == makeup).unwrap();
                assert_eq!(
                    fact.value_bits,
                    Some(12.0_f64.to_bits()),
                    "native getter before host DSP"
                );
                assert_eq!(fact.author_value_bits, Some(12.0_f64.to_bits()));
                value = true;
            }
            AudioPlaybackEvent::NativeDiagnosticsObserved { .. } => {}
            other => {
                panic!("no editor, author transaction or PCM before cold state proof: {other:?}")
            }
        }
        ready && value
    });
    assert_eq!(unentered_owner(&renderer, instance), prepared);
    renderer
        .observe_native_author_source_closed_for_test(&ExecutionCancellationToken::new())
        .expect("exclusive source observation")
        .expect("actual cold notifications completed without authoring");
}

fn assert_closed(evidence: mondrian_media::AudioPlaybackShutdownEvidence) {
    assert!(evidence.all_workers_terminated(), "{evidence:?}");
    assert!(
        evidence.shutdown_resource_facts_complete_at_deadline,
        "{evidence:?}"
    );
    assert!(
        !evidence.shutdown_owner_lifetime_unresolved_at_deadline,
        "{evidence:?}"
    );
    assert_eq!(evidence.foreign_owner_panics, 0);
    assert_eq!(evidence.foreign_owner_abandonments, 0);
}

#[test]
#[ignore = "official Zam4.5 CLAP, actual captured Makeup12dB state and matching normal Product; public Project and same-source software PCM"]
fn app_zam_clap_nondefault_opaque_project_reload_preserves_native_and_host_authority() {
    for host_override in [None, Some(6.0)] {
        project_roundtrip(host_override);
    }
}

fn project_roundtrip(host_override: Option<f64>) {
    let deadline = Instant::now() + Duration::from_secs(30);
    let path = |key| std::path::PathBuf::from(std::env::var_os(key).expect("native test input"));
    let helper = path("MONDRIAN_CLAP_TEST_HELPER");
    let seed = std::fs::read(path("MONDRIAN_ZAM_NONDEFAULT_STATE")).unwrap();
    assert!(!seed.is_empty() && seed.len() <= 64 * 1024);
    let clap = Arc::new(
        mondrian_audio::InstalledClapAudioProcessorSpecResolver::new(helper.clone()).unwrap(),
    );
    let vst3 =
        Arc::new(mondrian_audio::InstalledVst3AudioProcessorSpecResolver::new(helper).unwrap());
    let installed = clap.install_library(path("MONDRIAN_CLAP_MIXED_VENDOR_TEST_PLUGIN")).unwrap();
    assert!(installed.iter().any(|descriptor| descriptor.plugin_id == PLUGIN));
    let config = mondrian_media::AudioPlaybackConfig {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Stereo,
        chunk_frames: 4,
        preroll_frames: 4,
        high_watermark_frames: 8,
        max_in_flight: 1,
        ..mondrian_media::AudioPlaybackConfig::product_default()
    };
    let contract = mondrian_audio::AudioRenderContract {
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
    let factory = clap.create_instance(PLUGIN, contract, None).unwrap();
    assert_ne!(
        factory.opaque_state.as_ref().map(|state| state.as_slice()),
        Some(seed.as_slice()),
        "actual nondefault discriminator"
    );
    let mut instance = clap.create_instance(PLUGIN, contract, Some(seed.clone())).unwrap();
    assert_eq!(
        instance.opaque_state.as_ref().map(|state| state.as_slice()),
        Some(seed.as_slice())
    );
    // Native meters9/10 are presentation only; the existing author factory
    // intentionally excludes readonly descriptors from the canonical map.
    assert_eq!(instance.parameters.len(), 9);
    for id in 0..9 {
        assert!(instance
            .parameters
            .contains_key(&ParameterId::new(format!("clap.param.{id}")).unwrap()));
    }
    assert!(instance.parameters.values().all(|parameter| parameter.automation.is_none()));
    if let Some(value) = host_override {
        let id = ParameterId::new("clap.param.5").unwrap();
        instance
            .parameters
            .get_mut(&id)
            .unwrap()
            .set_automation(mondrian_core::ExactAutomationCurve::new(id, value).unwrap())
            .unwrap();
    }
    let instance_id = instance.id;
    let ColdNativeControl { mut state, _directory, output, renderer: old_weak } =
        prepare_app_native_control_with_instance(
            clap.clone(),
            vst3.clone(),
            config,
            instance.clone(),
            initialize_nonzero_audio,
        );
    let expected = state.active_sequence().unwrap().clone();
    let old_source = old_weak.upgrade().unwrap().owner_installation_verifier().unwrap();
    let old_session = state.authoring.as_ref().unwrap().session_id();
    let project = state.authoring.as_ref().unwrap().project_id();
    let files = tempfile::tempdir().unwrap();
    let archive = files.path().join("Zam非默认状态.mdp");
    let mut reopened: Option<AppState> = None;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        observe_restored_cold_state(&mut state, &old_weak, instance_id);
        assert!(state.active_sequence().unwrap().author_state_eq_ignoring_revision(&expected));
        assert_eq!(
            state.authoring.as_ref().unwrap().history().diagnostics().undo_entries,
            0
        );
        state.save_project_file_as(archive.clone()).expect("public Project Save As");
        assert!(archive.is_file());
        state.close_project().expect("public old ProjectClose");
        assert_closed(state.shutdown_audio_playback_and_wait());
        assert!(old_weak.upgrade().is_none());
        assert!(matches!(
            old_source.current_claim(),
            Err(AudioOwnerInstallationError::NotInstalled)
        ));
        assert_eq!(output.lock().queued, 0);

        let mut fresh = AppState::with_native_audio_catalogs(clap, vst3);
        assert_closed(fresh.shutdown_audio_playback_and_wait());
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
        fresh.audio_playback = AppAudioPlayback::Available(Box::new(
            AudioPlayback::with_output(
                config,
                Box::new(NativeHoldPlatformOutput(fresh_output.clone())),
            )
            .unwrap(),
        ));
        reopened = Some(fresh);
        let fresh = reopened.as_mut().unwrap();
        fresh.open_project_file(archive.clone()).expect("public fresh App ProjectOpen");
        let author = fresh.authoring.as_ref().unwrap();
        assert_eq!(author.project_id(), project);
        assert_ne!(author.session_id(), old_session);
        assert!(fresh.active_sequence().unwrap().author_state_eq_ignoring_revision(&expected));
        assert!(!fresh.can_undo_action() && !fresh.can_redo_action());
        assert!(fresh.native_audio_renderer.is_none());
        fresh.synchronize_playback_authoring_binding().unwrap();
        let source =
            fresh.audio_playback_renderer("Zam opaque Project roundtrip").unwrap().unwrap();
        let prepared = unentered_owner(&source.renderer, instance_id);
        let weak = Arc::downgrade(&source.renderer);
        assert!(!std::sync::Weak::ptr_eq(&weak, &old_weak));
        fresh
            .commit_audio_playback(
                "Zam opaque Project roundtrip",
                audio_anchor(0),
                Some(source),
            )
            .unwrap();
        let erased: std::sync::Weak<dyn AudioPcmRenderer> = weak.clone();
        assert!(fresh.audio_playback.matches_source_weak(&erased));
        observe_restored_cold_state(fresh, &weak, instance_id);
        let renderer = weak.upgrade().unwrap();
        assert_eq!(unentered_owner(&renderer, instance_id), prepared);
        let new_source = renderer.owner_installation_verifier().unwrap();
        assert!(!new_source.same_source(&old_source));
        assert_eq!(fresh_output.lock().queued, 0);
        let generation = AudioPcmRenderGeneration::new(fresh.audio_playback_snapshot().generation);
        let cancel = ExecutionCancellationToken::new();
        let expected_sample = 0.25_f64 * 10.0_f64.powf(host_override.unwrap_or(12.0) / 20.0);
        // Predeclared Float32 arithmetic bound. Restoring factory unity instead
        // would differ by >0.24. The no-curve case depends on native state alone;
        // the Some(6dB) case must override the conflicting opaque12dB value.
        let tolerance = 2.0 * f64::from(f32::EPSILON);
        for start_sample in (0..32).step_by(4) {
            let AudioPcmRenderOutcome::Rendered(buffer) = renderer
                .render(
                    AudioPcmRenderRequest {
                        transport_activity: None,
                        start_sample,
                        frame_count: 4,
                        sample_rate: 48_000,
                        channel_layout: AudioChannelLayout::Stereo,
                        continuity: if start_sample == 0 {
                            AudioPcmContinuity::Enter(generation)
                        } else {
                            AudioPcmContinuity::Continue(generation)
                        },
                    },
                    &cancel,
                )
                .expect("the same installed fresh Timeline/Delivery source")
            else {
                panic!("complete nondefault PCM required");
            };
            assert_eq!(buffer.samples.len(), 8);
            for frame in buffer.samples.chunks_exact(2) {
                for (actual, expected) in frame.iter().zip([expected_sample, -expected_sample]) {
                    assert!(
                        actual.is_finite() && (f64::from(*actual) - expected).abs() <= tolerance,
                        "frame{start_sample}: actual={actual}, expected={expected}"
                    );
                }
            }
        }
        assert!(fresh.active_sequence().unwrap().author_state_eq_ignoring_revision(&expected));
        assert_eq!(
            fresh.active_sequence().unwrap().audio_program.outputs[0]
                .strip
                .pre_fader
                .processors[0],
            instance
        );
        drop(renderer);
        fresh.close_project().expect("public fresh ProjectClose after PCM");
        assert_closed(fresh.shutdown_audio_playback_and_wait());
        assert!(weak.upgrade().is_none());
        assert!(matches!(
            new_source.current_claim(),
            Err(AudioOwnerInstallationError::NotInstalled)
        ));
        assert!(
            Instant::now() < deadline,
            "original30s public roundtrip budget"
        );
    }));
    let old_cleanup = state.shutdown_audio_playback_and_wait();
    let fresh_cleanup = reopened.as_mut().map(AppState::shutdown_audio_playback_and_wait);
    assert_closed(old_cleanup);
    if let Some(evidence) = fresh_cleanup {
        assert_closed(evidence);
    }
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}
