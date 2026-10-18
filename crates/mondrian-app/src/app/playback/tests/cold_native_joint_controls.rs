use super::*;

const GAIN_CLASS: &str = "AE4C8B1910234F819CA26D315F8702A1";

struct ColdNativeControl {
    state: AppState,
    _directory: tempfile::TempDir,
    output: Arc<parking_lot::Mutex<NativeHoldPlatformState>>,
    renderer: std::sync::Weak<TimelineAudioPcmRenderer>,
}

fn prepare_app_cold_native_control() -> ColdNativeControl {
    let helper = std::path::PathBuf::from(
        std::env::var_os("MONDRIAN_VST3_TEST_HELPER")
            .expect("set the current normal Product helper"),
    );
    let plugin = std::path::PathBuf::from(
        std::env::var_os("MONDRIAN_VST3_STATEFUL_TEST_PLUGIN")
            .expect("set the existing owned Stateful Gain DLL"),
    );
    assert!(helper.is_absolute() && helper.is_file());
    assert!(plugin.is_absolute() && plugin.is_file());
    let clap = Arc::new(
        mondrian_audio::InstalledClapAudioProcessorSpecResolver::new(helper.clone())
            .expect("actual empty installed CLAP catalog"),
    );
    let vst3 = Arc::new(
        mondrian_audio::InstalledVst3AudioProcessorSpecResolver::new(helper)
            .expect("actual installed VST3 catalog"),
    );
    // These original standalone installation/author-capture probes
    // precede Playback observation and are not custody evidence here.
    let descriptors = vst3.install_plugin(plugin).expect("install owned native binary");
    assert!(descriptors.iter().any(|descriptor| {
        descriptor.class_id == GAIN_CLASS && descriptor.name == "Mondrian Stateful Gain Fixture"
    }));
    let config = mondrian_media::AudioPlaybackConfig {
        chunk_frames: 4,
        preroll_frames: 4,
        high_watermark_frames: 8,
        max_in_flight: 1,
        ..mondrian_media::AudioPlaybackConfig::product_default()
    };
    let instance = vst3
        .create_instance(
            GAIN_CLASS,
            mondrian_audio::AudioRenderContract {
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
            },
            None,
        )
        .expect("original catalog captures actual native author state");
    assert!(matches!(
        &instance.definition,
        mondrian_timeline::audio::AudioProcessorDefinitionRef::Vst3 {
            class_id, binary_sha256: Some(_), ..
        } if class_id == GAIN_CLASS
    ));
    assert_eq!(instance.parameters.len(), 1);
    assert_eq!(
        instance.opaque_state.as_ref().map(|state| state.len()),
        Some(60)
    );

    let mut state = AppState::with_native_audio_catalogs(clap, vst3);
    // App construction leaves the default device manager dormant.
    // Never poll it; close the original workers before replacing output.
    assert!(state.shutdown_audio_playback_and_wait().all_workers_terminated());
    let mut snapshot = audio_snapshot();
    snapshot.active = false;
    snapshot.buffered_frames = 0;
    let output = Arc::new(parking_lot::Mutex::new(NativeHoldPlatformState {
        snapshot,
        revision: 0,
        acknowledged: 0,
        auto_acknowledge: true,
        queued: 0,
    }));
    state.audio_playback = AppAudioPlayback::Available(Box::new(
        AudioPlayback::with_output(config, Box::new(NativeHoldPlatformOutput(output.clone())))
            .expect("original Media workers with no-device software output"),
    ));
    let directory = tempfile::tempdir().expect("actual Project Asset Library");
    let library = AssetLibrary::open(directory.path().join("library")).unwrap();
    let mut sequence = Sequence::new("App cold owned VST3 closure");
    assert_eq!(
        sequence.settings.audio_channel_layout,
        config.channel_layout
    );
    sequence.audio_program.outputs[0].strip.pre_fader.processors.push(instance);
    state.test_set_sequence(Some(sequence));
    state.test_set_asset_library(Some(library));

    // Exactly one original App -> Timeline/Delivery/Runtime construction,
    // with its actual installed composite resolver and Playback registrar.
    let source = state
        .audio_playback_renderer("cold native joint control")
        .expect("actual App cold constructor")
        .expect("the selected native processor requires execution");
    assert!(source.renderer.native_observations_supported());
    let weak = Arc::downgrade(&source.renderer);
    state
        .commit_audio_playback("cold native joint control", audio_anchor(0), Some(source))
        .expect("one original App commit and Media prepare");
    assert!(state
        .native_audio_renderer
        .as_ref()
        .is_some_and(|installed| { std::sync::Weak::ptr_eq(installed, &weak) }));
    ColdNativeControl {
        state,
        _directory: directory,
        output,
        renderer: weak,
    }
}

fn observe_app_cold_native_retirement(retain_external: bool) {
    let ColdNativeControl { mut state, _directory, output, renderer: weak } =
        prepare_app_cold_native_control();
    let external = retain_external
        .then(|| weak.upgrade().expect("original Media source retains the actual Timeline"));
    let verifier = weak
        .upgrade()
        .expect("original Media source retains the actual Timeline")
        .owner_installation_verifier()
        .expect("original Media Source registration reached the real Runtime");
    let claim = verifier.current_claim().expect("actual installed Source claim");
    assert!(verifier.validate(&claim).is_ok());
    // prepare queues owner binding, not PCM. No poll/Enter/render loop
    // is added to force a lifecycle result or change native processing.
    let prepared = state.audio_playback_snapshot();
    assert_eq!(prepared.in_flight, 0);
    assert_eq!(prepared.next_start_sample, 0);
    assert_eq!(output.lock().queued, 0);

    let evidence = state.shutdown_audio_playback_and_wait();
    assert_eq!(evidence.render_workers_started, 1);
    assert_eq!(evidence.render_workers_terminated, 1);
    assert_eq!(evidence.render_retirement_workers_started, 1);
    assert_eq!(evidence.render_retirement_workers_terminated, 1);
    assert_eq!(evidence.foreign_owner_panics, 0);
    assert_eq!(evidence.foreign_owner_abandonments, 0);
    assert!(matches!(
        verifier.current_claim(),
        Err(mondrian_core::AudioOwnerInstallationError::NotInstalled)
    ));
    if retain_external {
        assert!(!evidence.all_workers_terminated(), "{evidence:?}");
        assert!(!evidence.shutdown_resource_facts_complete_at_deadline);
        assert!(evidence.shutdown_owner_lifetime_unresolved_at_deadline);
        assert!(weak.upgrade().is_some());
        let prior_receipt = evidence;
        drop(external);
        assert!(
            weak.upgrade().is_none(),
            "release the actual final renderer alias"
        );
        assert_eq!(
            evidence, prior_receipt,
            "late release cannot rewrite the receipt"
        );
        assert!(!evidence.all_workers_terminated());
    } else {
        assert!(evidence.all_workers_terminated(), "{evidence:?}");
        assert!(evidence.shutdown_resource_facts_complete_at_deadline);
        assert!(!evidence.shutdown_owner_lifetime_unresolved_at_deadline);
        assert!(weak.upgrade().is_none());
    }
    assert_eq!(output.lock().queued, 0);
    // Receipt flags qualify this actual App success/alias boundary, not
    // each fresh-probe registration, OS graceful100ms or all App workers.
    drop(state);
}

#[test]
#[ignore = "current normal Product and owned Stateful Gain DLL; actual App cold closure, no device/PCM"]
fn app_owned_vst3_cold_retirement_closes_after_all_aliases_release() {
    observe_app_cold_native_retirement(false);
}

#[test]
#[ignore = "current normal Product and owned Stateful Gain DLL; actual App external alias boundary"]
fn app_owned_vst3_cold_external_alias_keeps_shutdown_unresolved() {
    observe_app_cold_native_retirement(true);
}

fn poll_idle_until(
    state: &mut AppState,
    generation: u64,
    mut observe: impl FnMut(&AudioPlaybackEvent) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        // Use the original poll/handler pair, without the outer App
        // pump's independent idle-warmup dispatch. Idle can service
        // diagnostics; it returns before admitting any PCM work.
        let poll = state
            .audio_playback
            .poll(AudioPlaybackMode::Idle, audio_anchor(0))
            .expect("original serial Media control completion");
        assert_eq!(poll.snapshot.generation, generation);
        assert_eq!(poll.snapshot.in_flight, 0);
        assert_eq!(poll.snapshot.next_start_sample, 0);
        assert_eq!(
            poll.snapshot.output.expect("software output").buffered_frames,
            0
        );
        let mut completed = false;
        for event in poll.events {
            assert!(
                matches!(
                    &event,
                    AudioPlaybackEvent::NativeOwnerBindingsObserved { .. }
                        | AudioPlaybackEvent::NativeEditorObserved { .. }
                        | AudioPlaybackEvent::NativeDiagnosticsObserved { .. }
                        | AudioPlaybackEvent::NativeParameterObservationsObserved { .. }
                ),
                "unexpected control, attachment, author or PCM event: {event:?}"
            );
            completed |= observe(&event);
            state
                .handle_audio_playback_event(event, Instant::now())
                .expect("original App event consumer");
        }
        if completed {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "actual control completion was not delivered"
        );
        std::thread::yield_now();
    }
}

#[test]
#[ignore = "current normal Product and owned Stateful Gain DLL; actual App binding/editor refusal, no device/PCM"]
fn app_owned_vst3_ready_delivery_does_not_authorize_missing_editor() {
    use super::super::super::product_action::{
        AudioNativeEditorPayload, AudioProductAction, ProductAction,
    };
    use mondrian_core::{
        NativeAudioEditorControlIntent as Intent, NativeAudioEditorControlOutcome as Outcome,
        NativeAudioEditorControlRefusal as Refusal, NativeAudioEditorInsertion as Insertion,
        NativeAudioEditorLocator, NativeAudioEditorOccurrenceOwner as Owner,
    };

    let ColdNativeControl { mut state, _directory, output, renderer: weak } =
        prepare_app_cold_native_control();
    let sequence = state.active_sequence().expect("actual author Sequence");
    let native_output = &sequence.audio_program.outputs[0];
    let processor = &native_output.strip.pre_fader.processors[0];
    let output_id = native_output.id;
    let payload = AudioNativeEditorPayload {
        session_id: state.authoring_session_id().expect("actual Authoring Session"),
        sequence_id: sequence.id,
        address: mondrian_timeline::AudioProcessorRackAddress::ChannelStrip {
            owner: mondrian_timeline::AudioChannelStripOwner::ProgramOutput {
                output_id: native_output.id,
            },
            rack: mondrian_timeline::AudioChannelStripRack::PreFader,
        },
        occurrence: None,
        processor_id: processor.id,
        definition: processor.definition.clone(),
        sequence_time: mondrian_core::DomainTime::new(
            mondrian_core::AuthoringTimeDomain::Sequence(sequence.id),
            TimelineTime::ZERO,
        ),
        open: true,
    };
    let parameter_ids: Vec<_> = processor.parameters.keys().cloned().collect();
    let document_before = serde_json::to_value(sequence).expect("actual author snapshot");
    let author_before = state.authoring.as_ref().expect("author").execution_snapshot();
    let engine_before = state.playback_engine.snapshot();
    assert!(state.native_audio_editor.is_none());
    let verifier = weak
        .upgrade()
        .expect("same original Timeline")
        .owner_installation_verifier()
        .expect("actual Runtime registration");
    let claim = verifier.current_claim().expect("actual installed Source claim");
    assert!(verifier.validate(&claim).is_ok());
    let generation = state.audio_playback_snapshot().generation;

    let mut ready = None;
    poll_idle_until(&mut state, generation, |event| match event {
        AudioPlaybackEvent::NativeOwnerBindingsObserved {
            renderer_id,
            generation: observed_generation,
            status,
        } => {
            assert!(ready.is_none(), "one actual binding completion");
            assert_eq!(*observed_generation, generation);
            assert_eq!(
                *status,
                mondrian_media::AudioPcmNativeOwnerBindingStatus::Ready
            );
            ready = Some(*renderer_id);
            true
        }
        AudioPlaybackEvent::NativeEditorObserved { .. } => {
            panic!("no editor action was submitted before Ready")
        }
        _ => false,
    });
    let ready = ready.expect("actual Ready event, not queued capability metadata");
    assert!(verifier.validate(&claim).is_ok());
    match &state.audio_playback {
        AppAudioPlayback::Available(playback) => assert_eq!(
            playback.native_owner_binding_status(),
            Some(mondrian_media::AudioPcmNativeOwnerBindingStatus::Ready)
        ),
        AppAudioPlayback::Unavailable { .. } => panic!("original Media owner disappeared"),
    }
    assert!(
        state.native_audio_editor.is_none(),
        "Ready cannot create an editor context"
    );

    state
        .dispatch_action(
            ProductAction::Audio(AudioProductAction::NativeEditor(payload.clone()))
                .into_external_action(),
        )
        .expect("one original App editor action");
    assert_eq!(
        state.native_audio_editor_surface_state(&payload),
        (false, true)
    );
    let expected_locator = NativeAudioEditorLocator {
        sequence: payload.sequence_id,
        instance: payload.processor_id,
        owner: Owner::Output(output_id),
        insertion: Insertion::PreFader,
    };
    let mut no_editor = false;
    poll_idle_until(&mut state, generation, |event| match event {
        AudioPlaybackEvent::NativeEditorObserved {
            renderer_id,
            generation: observed_generation,
            intent,
            outcome,
        } => {
            assert!(
                !no_editor,
                "one completion for the one actual editor action"
            );
            assert_eq!(*renderer_id, ready);
            assert_eq!(*observed_generation, generation);
            assert!(matches!(intent, Intent::ResolveAndOpen { locator, title }
                if *locator == expected_locator && title.as_str() == "Mondrian"));
            assert_eq!(*outcome, Outcome::Refused(Refusal::NoEditor));
            no_editor = true;
            true
        }
        AudioPlaybackEvent::NativeOwnerBindingsObserved { .. } => {
            panic!("the same Source must not bind a second materialization")
        }
        _ => false,
    });
    assert!(no_editor);
    assert_eq!(
        state.native_audio_editor_surface_state(&payload),
        (false, false)
    );
    assert_eq!(
        state.status_hint().map(|(message, _)| message.id),
        Some("native-editor-no-view")
    );
    assert!(state
        .native_audio_editor
        .as_ref()
        .expect("original refused editor context")
        .retained_state_entry_source(None, payload.session_id, payload.sequence_id)
        .is_none());
    for id in parameter_ids {
        assert!(state.native_audio_parameter_presentation(&payload, &id).is_none());
    }
    assert!(
        author_before.enter_commit().is_ok(),
        "editor refusal cannot revoke author state"
    );
    assert_eq!(
        state.authoring.as_ref().expect("author").execution_snapshot().generation(),
        author_before.generation()
    );
    assert_eq!(
        serde_json::to_value(state.active_sequence().expect("same author Sequence"))
            .expect("author snapshot after refusal"),
        document_before
    );
    assert_eq!(state.playback_engine.snapshot(), engine_before);
    assert!(state.playback_engine.native_audio_control_hold_ticket().is_none());
    assert!(state.audio_playback.native_control_hold_generation().is_none());
    assert!(state.audio_playback.native_reprepare_pending_generation().is_none());
    match &state.audio_playback {
        AppAudioPlayback::Available(playback) => {
            assert!(playback.native_editor_cleanup_target().is_none());
        }
        AppAudioPlayback::Unavailable { .. } => panic!("original Media owner disappeared"),
    }
    assert_eq!(
        verifier.current_claim().expect("same Source").generation(),
        claim.generation()
    );
    assert!(verifier.validate(&claim).is_ok());
    assert!(state
        .native_audio_renderer
        .as_ref()
        .is_some_and(|renderer| { std::sync::Weak::ptr_eq(renderer, &weak) }));
    let observed = state.audio_playback_snapshot();
    assert_eq!(observed.generation, generation);
    assert_eq!(observed.in_flight, 0);
    assert_eq!(observed.next_start_sample, 0);
    assert_eq!(output.lock().queued, 0);

    let evidence = state.shutdown_audio_playback_and_wait();
    assert_eq!(evidence.render_workers_started, 1);
    assert_eq!(evidence.render_workers_terminated, 1);
    assert_eq!(evidence.render_retirement_workers_started, 1);
    assert_eq!(evidence.render_retirement_workers_terminated, 1);
    assert_eq!(evidence.foreign_owner_panics, 0);
    assert_eq!(evidence.foreign_owner_abandonments, 0);
    assert!(evidence.all_workers_terminated(), "{evidence:?}");
    assert!(evidence.shutdown_resource_facts_complete_at_deadline);
    assert!(!evidence.shutdown_owner_lifetime_unresolved_at_deadline);
    assert!(matches!(
        verifier.current_claim(),
        Err(mondrian_core::AudioOwnerInstallationError::NotInstalled)
    ));
    assert!(weak.upgrade().is_none());
    assert_eq!(output.lock().queued, 0);
    // This observes Ready and a genuine NoEditor refusal. It does not
    // directly read the private target or qualify legal author handoff,
    // PCM, GUI attachment, every probe registration or graceful100ms.
    drop(state);
}
