//! One public Project roundtrip recipe shared by real native gain/state cases.
use super::project_state_roundtrip::{unentered_owner, GAIN_SOURCE_FRAMES};
use super::*;
use mondrian_core::{AudioOwnerInstallationError, ExecutionCancellationToken, ParameterId};
use mondrian_media::{
    AudioPcmContinuity, AudioPcmRenderGeneration, AudioPcmRenderOutcome, AudioPcmRenderRequest,
    AudioPcmRenderer,
};

pub(super) struct ProjectCase {
    pub clap: Arc<mondrian_audio::InstalledClapAudioProcessorSpecResolver>,
    pub vst3: Arc<mondrian_audio::InstalledVst3AudioProcessorSpecResolver>,
    pub config: mondrian_media::AudioPlaybackConfig,
    pub instance: mondrian_timeline::audio::AudioProcessorInstance,
    pub deadline: Instant,
}

pub(super) fn observe_cold_values(
    state: &mut AppState,
    weak: &std::sync::Weak<TimelineAudioPcmRenderer>,
    instance: mondrian_core::AudioProcessorInstanceId,
    native_parameter_count: usize,
    expected: &[(ParameterId, f64)],
) {
    let renderer = weak.upgrade().expect("the original App/Media source");
    let prepared = unentered_owner(&renderer, instance);
    let generation = state.audio_playback_snapshot().generation;
    let mut ready = false;
    let mut values = false;
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
                assert_eq!(observation.facts().len(), native_parameter_count);
                assert!(observation.target().continuity_epoch.is_none());
                for (parameter, value) in expected {
                    let fact = observation
                        .facts()
                        .iter()
                        .find(|fact| &fact.parameter == parameter)
                        .expect("restored native parameter");
                    assert_eq!(
                        fact.value_bits,
                        Some(value.to_bits()),
                        "native getter before DSP"
                    );
                    assert_eq!(fact.author_value_bits, Some(value.to_bits()));
                }
                values = true;
            }
            AudioPlaybackEvent::NativeDiagnosticsObserved { .. } => {}
            other => panic!("no editor, author transaction or PCM before cold proof: {other:?}"),
        }
        ready && values
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

pub(super) fn run(
    case: ProjectCase,
    control: ColdNativeControl,
    observe: impl Fn(
        &mut AppState,
        &std::sync::Weak<TimelineAudioPcmRenderer>,
        mondrian_core::AudioProcessorInstanceId,
    ),
    expected_sample: impl Fn(usize, usize) -> f64,
) {
    let ProjectCase { clap, vst3, config, instance, deadline } = case;
    let instance_id = instance.id;
    let ColdNativeControl { mut state, _directory, output, renderer: old_weak } = control;
    let expected = state.active_sequence().unwrap().clone();
    let old_source = old_weak.upgrade().unwrap().owner_installation_verifier().unwrap();
    let old_session = state.authoring.as_ref().unwrap().session_id();
    let project = state.authoring.as_ref().unwrap().project_id();
    let files = tempfile::tempdir().unwrap();
    let archive = files.path().join("原生状态与声道布局.mdp");
    let check_layout = |renderer: &TimelineAudioPcmRenderer| {
        let evidence = renderer.delivery_evidence();
        assert_eq!(
            evidence.program_layout,
            expected.settings.audio_channel_layout
        );
        assert_eq!(evidence.target_layout, config.channel_layout);
        assert_eq!(
            evidence.mapping_kind,
            if evidence.program_layout == evidence.target_layout {
                mondrian_audio::AudioDeliveryMappingKind::Identity
            } else {
                mondrian_audio::AudioDeliveryMappingKind::Standard
            }
        );
    };
    let mut reopened: Option<AppState> = None;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        check_layout(&old_weak.upgrade().unwrap());
        observe(&mut state, &old_weak, instance_id);
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
        let source = fresh.audio_playback_renderer("native Project roundtrip").unwrap().unwrap();
        check_layout(&source.renderer);
        let prepared = unentered_owner(&source.renderer, instance_id);
        let weak = Arc::downgrade(&source.renderer);
        assert!(!std::sync::Weak::ptr_eq(&weak, &old_weak));
        fresh
            .commit_audio_playback("native Project roundtrip", audio_anchor(0), Some(source))
            .unwrap();
        let erased: std::sync::Weak<dyn AudioPcmRenderer> = weak.clone();
        assert!(fresh.audio_playback.matches_source_weak(&erased));
        observe(fresh, &weak, instance_id);
        let renderer = weak.upgrade().unwrap();
        assert_eq!(unentered_owner(&renderer, instance_id), prepared);
        let new_source = renderer.owner_installation_verifier().unwrap();
        assert!(!new_source.same_source(&old_source));
        assert_eq!(fresh_output.lock().queued, 0);
        let generation = AudioPcmRenderGeneration::new(fresh.audio_playback_snapshot().generation);
        let cancel = ExecutionCancellationToken::new();
        // Original predeclared Float32 arithmetic bound; no warmup samples skipped.
        let tolerance = 2.0 * f64::from(f32::EPSILON);
        let total_frames = usize::try_from(GAIN_SOURCE_FRAMES).unwrap();
        let channels = config.channel_layout.channel_count();
        for start in (0..total_frames).step_by(config.chunk_frames) {
            let frames = config.chunk_frames.min(total_frames - start);
            let AudioPcmRenderOutcome::Rendered(buffer) = renderer
                .render(
                    AudioPcmRenderRequest {
                        transport_activity: None,
                        start_sample: i64::try_from(start).unwrap(),
                        frame_count: frames,
                        sample_rate: config.sample_rate,
                        channel_layout: config.channel_layout,
                        continuity: if start == 0 {
                            AudioPcmContinuity::Enter(generation)
                        } else {
                            AudioPcmContinuity::Continue(generation)
                        },
                    },
                    &cancel,
                )
                .expect("the same installed fresh Timeline/Delivery source")
            else {
                panic!("complete native-state PCM required");
            };
            assert_eq!(buffer.channel_layout, config.channel_layout);
            assert_eq!(buffer.sample_rate, config.sample_rate);
            assert_eq!(buffer.samples.len(), frames * channels);
            for (index, actual) in buffer.samples.iter().enumerate() {
                let expected = expected_sample(start + index / channels, index % channels);
                assert!(
                    actual.is_finite() && (f64::from(*actual) - expected).abs() <= tolerance,
                    "frame{} channel{}: actual={actual}, expected={expected}",
                    start + index / channels,
                    index % channels
                );
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
