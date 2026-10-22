use super::*;

const GAIN_CLASS: &str = "AE4C8B1910234F819CA26D315F8702A1";

struct ColdNativeControl {
    state: AppState,
    _directory: tempfile::TempDir,
    output: Arc<parking_lot::Mutex<NativeHoldPlatformState>>,
    renderer: std::sync::Weak<TimelineAudioPcmRenderer>,
}

fn prepare_app_cold_native_control() -> ColdNativeControl {
    prepare_app_native_control(
        "MONDRIAN_VST3_STATEFUL_TEST_PLUGIN",
        GAIN_CLASS,
        "Mondrian Stateful Gain Fixture",
        1,
        60,
    )
}

fn prepare_app_native_control(
    plugin_environment: &str,
    expected_class: &str,
    expected_name: &str,
    expected_parameters: usize,
    expected_state_bytes: usize,
) -> ColdNativeControl {
    prepare_app_native_control_with_initializer(
        plugin_environment,
        expected_class,
        expected_name,
        expected_parameters,
        expected_state_bytes,
        |_, _, _| {},
    )
}

fn prepare_app_native_control_with_initializer(
    plugin_environment: &str,
    expected_class: &str,
    expected_name: &str,
    expected_parameters: usize,
    expected_state_bytes: usize,
    initialize: impl FnOnce(&mut Sequence, &Arc<AssetLibrary>, &std::path::Path),
) -> ColdNativeControl {
    let helper = std::path::PathBuf::from(
        std::env::var_os("MONDRIAN_VST3_TEST_HELPER")
            .expect("set the current normal Product helper"),
    );
    let plugin = std::path::PathBuf::from(
        std::env::var_os(plugin_environment).expect("set the selected current owned VST3 DLL"),
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
        descriptor.class_id == expected_class && descriptor.name == expected_name
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
            expected_class,
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
        } if class_id == expected_class
    ));
    assert_eq!(instance.parameters.len(), expected_parameters);
    assert_eq!(
        instance.opaque_state.as_ref().map(|state| state.len()),
        Some(expected_state_bytes)
    );

    prepare_app_native_control_with_instance(clap, vst3, config, instance, initialize)
}

fn prepare_app_clap_editor_control() -> ColdNativeControl {
    prepare_app_clap_editor_control_with_initializer(|_, _, _| {})
}

fn prepare_app_clap_editor_control_with_initializer(
    initialize: impl FnOnce(&mut Sequence, &Arc<AssetLibrary>, &std::path::Path),
) -> ColdNativeControl {
    const PLUGIN: &str = "org.mondrian.fixture.sample-gain";
    let helper = std::path::PathBuf::from(
        std::env::var_os("MONDRIAN_CLAP_TEST_HELPER").expect("current normal Product helper"),
    );
    let library = std::path::PathBuf::from(
        std::env::var_os("MONDRIAN_CLAP_SAMPLE_TEST_PLUGIN").expect("current owned CLAP Gain DLL"),
    );
    assert!(helper.is_absolute() && helper.is_file());
    assert!(library.is_absolute() && library.is_file());
    let clap = Arc::new(
        mondrian_audio::InstalledClapAudioProcessorSpecResolver::new(helper.clone())
            .expect("actual installed CLAP catalog"),
    );
    let vst3 = Arc::new(
        mondrian_audio::InstalledVst3AudioProcessorSpecResolver::new(helper)
            .expect("actual empty installed VST3 catalog"),
    );
    let descriptors = clap.install_library(library).expect("install actual CLAP library");
    assert_eq!(descriptors.len(), 1);
    assert_eq!(descriptors[0].plugin_id, PLUGIN);
    assert_eq!(descriptors[0].name, "Mondrian Sample Gain Fixture");
    let config = mondrian_media::AudioPlaybackConfig {
        chunk_frames: 4,
        preroll_frames: 4,
        high_watermark_frames: 8,
        max_in_flight: 1,
        ..mondrian_media::AudioPlaybackConfig::product_default()
    };
    let instance = clap
        .create_instance(
            PLUGIN,
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
        .expect("original CLAP catalog captures real schemas and author state");
    assert!(matches!(
        &instance.definition,
        mondrian_timeline::audio::AudioProcessorDefinitionRef::Clap {
            plugin_id, binary_sha256: Some(_), ..
        } if plugin_id == PLUGIN
    ));
    assert_eq!(instance.parameters.len(), 1);
    assert_eq!(
        instance.opaque_state.as_ref().expect("actual four-byte CLAP state").as_slice(),
        &1.0_f32.to_le_bytes()
    );
    prepare_app_native_control_with_instance(clap, vst3, config, instance, initialize)
}

fn prepare_app_native_control_with_instance(
    clap: Arc<mondrian_audio::InstalledClapAudioProcessorSpecResolver>,
    vst3: Arc<mondrian_audio::InstalledVst3AudioProcessorSpecResolver>,
    config: mondrian_media::AudioPlaybackConfig,
    instance: mondrian_timeline::audio::AudioProcessorInstance,
    initialize: impl FnOnce(&mut Sequence, &Arc<AssetLibrary>, &std::path::Path),
) -> ColdNativeControl {
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
    initialize(&mut sequence, &library, directory.path());
    state.test_set_sequence(Some(sequence));
    state.test_set_asset_library(Some(library));

    // The fixture setters install Project data only. Bind the original
    // stopped Engine before native construction, as production authoring
    // does, so the first editor commit need not perform an initial seek.
    assert_eq!(
        state.playback_engine.snapshot().state,
        mondrian_playback::TransportState::Stopped
    );
    assert!(state.native_audio_renderer.is_none());
    state
        .synchronize_playback_authoring_binding()
        .expect("bind the actual Sequence before cold native construction");
    assert_eq!(
        state.playback_engine.sequence_id(),
        state.active_sequence_id()
    );
    let bound = state.playback_engine.snapshot();
    assert_eq!(bound.state, mondrian_playback::TransportState::Paused);
    assert_eq!(bound.position.frame, 0);
    assert!(state.native_audio_renderer.is_none());
    let prepared = state.audio_playback_snapshot();
    assert_eq!(prepared.in_flight, 0);
    assert_eq!(prepared.next_start_sample, 0);
    assert_eq!(prepared.output.expect("software output").buffered_frames, 0);
    assert_eq!(output.lock().queued, 0);
    let history = state.authoring.as_ref().unwrap().history().diagnostics();
    assert_eq!(history.undo_entries, 0);
    assert_eq!(history.pending_entries, 0);

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
    assert!(
        state.native_audio_editor.is_some(),
        "original refused editor context"
    );
    assert!(state
        .native_audio_author_work
        .as_ref()
        .and_then(|work| work.retained_state_entry_source(
            None,
            payload.session_id,
            payload.sequence_id,
            &state.audio_playback
        ))
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

// These are test-side Win32 observations of the actual owned view, never an
// editor parent implementation or a PID inferred from a generic window title.
#[link(name = "user32")]
unsafe extern "system" {
    #[link_name = "EnumWindows"]
    fn enumerate_windows(
        callback: unsafe extern "system" fn(*mut std::ffi::c_void, isize) -> i32,
        data: isize,
    ) -> i32;
    #[link_name = "EnumChildWindows"]
    fn enumerate_child_windows(
        parent: *mut std::ffi::c_void,
        callback: unsafe extern "system" fn(*mut std::ffi::c_void, isize) -> i32,
        data: isize,
    ) -> i32;
    #[link_name = "GetWindowThreadProcessId"]
    fn window_process(window: *mut std::ffi::c_void, process: *mut u32) -> u32;
    #[link_name = "GetClassNameW"]
    fn window_class(window: *mut std::ffi::c_void, class: *mut u16, capacity: i32) -> i32;
    #[link_name = "GetWindowTextW"]
    fn window_text(window: *mut std::ffi::c_void, text: *mut u16, capacity: i32) -> i32;
    #[link_name = "GetAncestor"]
    fn window_ancestor(window: *mut std::ffi::c_void, flags: u32) -> *mut std::ffi::c_void;
    #[link_name = "PostMessageW"]
    fn post_window_message(
        window: *mut std::ffi::c_void,
        message: u32,
        word: usize,
        long: isize,
    ) -> i32;
    #[link_name = "IsWindow"]
    fn is_window(window: *mut std::ffi::c_void) -> i32;
}

fn owned_editor_view(process: std::num::NonZeroU32) -> *mut std::ffi::c_void {
    owned_editor_view_matching(process, b"MondrianOwnedVst3EditorFixture", true, None)
}

fn owned_editor_view_matching(
    process: std::num::NonZeroU32,
    class: &'static [u8],
    exact_class: bool,
    title: Option<&'static [u8]>,
) -> *mut std::ffi::c_void {
    struct Search {
        process: u32,
        class: &'static [u8],
        exact_class: bool,
        title: Option<&'static [u8]>,
        matches: usize,
        view: *mut std::ffi::c_void,
    }
    unsafe extern "system" fn child(window: *mut std::ffi::c_void, data: isize) -> i32 {
        let Some(search) = (unsafe { (data as *mut Search).as_mut() }) else {
            return 0;
        };
        let mut process = 0;
        unsafe {
            window_process(window, &mut process);
        }
        let mut class = [0_u16; 128];
        let length = unsafe { window_class(window, class.as_mut_ptr(), class.len() as i32) };
        if process != search.process
            || length < search.class.len() as i32
            || (search.exact_class && length != search.class.len() as i32)
            || !class[..search.class.len()]
                .iter()
                .copied()
                .eq(search.class.iter().map(|byte| u16::from(*byte)))
        {
            return 1;
        }
        if let Some(expected) = search.title {
            let mut text = [0_u16; 128];
            let length = unsafe { window_text(window, text.as_mut_ptr(), text.len() as i32) };
            if length != expected.len() as i32
                || !text[..expected.len()]
                    .iter()
                    .copied()
                    .eq(expected.iter().map(|byte| u16::from(*byte)))
            {
                return 1;
            }
        }
        search.matches = search.matches.saturating_add(1);
        search.view = window;
        1
    }
    unsafe extern "system" fn parent(window: *mut std::ffi::c_void, data: isize) -> i32 {
        let Some(expected) =
            (unsafe { (data as *const Search).as_ref() }).map(|search| search.process)
        else {
            return 0;
        };
        let mut process = 0;
        unsafe {
            window_process(window, &mut process);
        }
        if process == expected {
            unsafe {
                enumerate_child_windows(window, child, data);
            }
        }
        1
    }
    let mut search = Search {
        process: process.get(),
        class,
        exact_class,
        title,
        matches: 0,
        view: std::ptr::null_mut(),
    };
    assert_ne!(
        unsafe { enumerate_windows(parent, (&mut search as *mut Search) as isize) },
        0
    );
    assert_eq!(
        search.matches, 1,
        "one actual fixture view in the authenticated native child"
    );
    assert!(!search.view.is_null());
    let parent = unsafe { window_ancestor(search.view, 2) }; // GA_ROOT
    assert!(!parent.is_null());
    for window in [parent, search.view] {
        let mut actual = 0;
        assert_ne!(unsafe { window_process(window, &mut actual) }, 0);
        assert_eq!(
            actual,
            process.get(),
            "both HWNDs belong to this observed native child"
        );
    }
    search.view
}

fn assert_editor_configuration(bytes: &[u8], gain: f64, balance: f64, controller_nonce: u32) {
    assert_eq!(bytes.len(), 92);
    assert_eq!(&bytes[..16], b"VST3HOST_STATE\0\0");
    assert_eq!(u32::from_le_bytes(bytes[16..20].try_into().unwrap()), 1);
    assert_eq!(u32::from_le_bytes(bytes[20..24].try_into().unwrap()), 32);
    assert_eq!(u32::from_le_bytes(bytes[24..28].try_into().unwrap()), 32);
    for (state, nonce) in [(&bytes[28..60], 0), (&bytes[60..92], controller_nonce)] {
        assert_eq!(&state[..4], b"MEV1");
        assert_eq!(u32::from_le_bytes(state[4..8].try_into().unwrap()), 1);
        assert_eq!(
            f64::from_le_bytes(state[8..16].try_into().unwrap()).to_bits(),
            gain.to_bits()
        );
        assert_eq!(
            f64::from_le_bytes(state[16..24].try_into().unwrap()).to_bits(),
            balance.to_bits()
        );
        assert_eq!(u32::from_le_bytes(state[24..28].try_into().unwrap()), 0);
        assert_eq!(u32::from_le_bytes(state[28..32].try_into().unwrap()), nonce);
    }
}

fn poll_gui_until(
    state: &mut AppState,
    generation: u64,
    observe: impl FnMut(&AudioPlaybackEvent) -> bool,
) {
    poll_gui_until_after_proposal(state, generation, observe, |_| {});
}

fn poll_gui_until_after_proposal(
    state: &mut AppState,
    generation: u64,
    mut observe: impl FnMut(&AudioPlaybackEvent) -> bool,
    mut after_proposal: impl FnMut(&mut AppState),
) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let poll = state
            .audio_playback
            .poll(AudioPlaybackMode::Idle, audio_anchor(0))
            .expect("original App/Media Idle control path, without outer warmup");
        assert_eq!(poll.snapshot.generation, generation);
        assert_eq!(poll.snapshot.in_flight, 0);
        assert_eq!(poll.snapshot.next_start_sample, 0);
        assert_eq!(
            poll.snapshot.output.expect("software output").buffered_frames,
            0
        );
        let mut completed = false;
        for event in poll.events {
            completed |= observe(&event);
            let author_proposal = matches!(
                &event,
                AudioPlaybackEvent::NativeAuthorProposalObserved { .. }
            );
            state
                .handle_audio_playback_event(event, Instant::now())
                .expect("original App consumer");
            if author_proposal {
                // The original consumer admitted this proposal to Session;
                // no later poll has consumed its producer handoff/Ack yet.
                // Runtime/editor locks are not held across this test hook.
                after_proposal(state);
            }
        }
        if completed {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "actual App GUI control did not complete"
        );
        std::thread::yield_now();
    }
}

#[test]
#[ignore = "current normal Product with actual VST parameter presentation + current owned editor DLL; canonical App handoff, no PCM/device"]
fn app_owned_vst3_editor_group_commits_through_same_source() {
    use super::super::super::product_action::{
        AudioNativeEditorPayload, AudioProductAction, ProductAction,
    };
    use mondrian_core::{
        NativeAudioAuthorCallback as Callback, NativeAudioAuthorCompletion as Completion,
        NativeAudioEditorControlIntent as Intent, NativeAudioEditorControlOutcome as Outcome,
        NativeAudioParameterCallbackPhase as Phase,
    };
    use mondrian_media::{
        AudioPcmNativeAuthorHandoffContinuity as Continuity,
        AudioPcmNativeAuthorHandoffStatus as Status,
    };

    let ColdNativeControl { mut state, _directory, output, renderer: weak } =
        prepare_app_native_control(
            "MONDRIAN_VST3_EDITOR_TEST_PLUGIN",
            "C48B1B724CDE4A91A2C9C4EBB7DD1E83",
            "Mondrian Owned Native Editor Fixture",
            3,
            92,
        );
    let sequence = state.active_sequence().expect("actual author Sequence");
    assert!(sequence.audio_program.note_lanes.is_empty());
    let native_output = &sequence.audio_program.outputs[0];
    let original = native_output.strip.pre_fader.processors[0].clone();
    let gain = mondrian_core::ParameterId::new("vst3.param.0").unwrap();
    let balance = mondrian_core::ParameterId::new("vst3.param.1").unwrap();
    let mode = mondrian_core::ParameterId::new("vst3.param.2").unwrap();
    assert_eq!(original.parameters.len(), 3);
    for (id, parameter) in &original.parameters {
        assert_eq!(&parameter.schema.parameter_id, id);
        assert!(
            parameter.automation.is_none(),
            "captured native state owns each initial value"
        );
    }
    assert_editor_configuration(
        original.opaque_state.as_ref().unwrap().as_ref(),
        0.5,
        0.5,
        0,
    );
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
        processor_id: original.id,
        definition: original.definition.clone(),
        sequence_time: mondrian_core::DomainTime::new(
            mondrian_core::AuthoringTimeDomain::Sequence(sequence.id),
            TimelineTime::ZERO,
        ),
        open: true,
    };
    let author_before = state.authoring.as_ref().unwrap().execution_snapshot();
    let history_before = state.authoring.as_ref().unwrap().history().diagnostics();
    assert_eq!(history_before.undo_entries, 0);
    assert_eq!(history_before.pending_entries, 0);
    let engine_before = state.playback_engine.snapshot();
    let verifier = weak
        .upgrade()
        .expect("same original Timeline")
        .owner_installation_verifier()
        .unwrap();
    let original_claim = verifier.current_claim().unwrap();
    assert!(verifier.validate(&original_claim).is_ok());
    let generation = state.audio_playback_snapshot().generation;
    let mut renderer_id = None;
    poll_idle_until(&mut state, generation, |event| match event {
        AudioPlaybackEvent::NativeOwnerBindingsObserved {
            renderer_id: actual,
            generation: observed,
            status,
        } => {
            assert!(renderer_id.is_none());
            assert_eq!(*observed, generation);
            assert_eq!(
                *status,
                mondrian_media::AudioPcmNativeOwnerBindingStatus::Ready
            );
            renderer_id = Some(*actual);
            true
        }
        AudioPlaybackEvent::NativeEditorObserved { .. } => panic!("no editor request before Ready"),
        _ => false,
    });
    let renderer_id = renderer_id.unwrap();
    assert!(state.native_audio_editor.is_none());
    state
        .dispatch_action(
            ProductAction::Audio(AudioProductAction::NativeEditor(payload.clone()))
                .into_external_action(),
        )
        .expect("one original App open action");
    assert_eq!(
        state.native_audio_editor_surface_state(&payload),
        (false, true)
    );
    let mut opened = None;
    poll_idle_until(&mut state, generation, |event| match event {
        AudioPlaybackEvent::NativeEditorObserved {
            renderer_id: actual,
            generation: observed,
            intent,
            outcome,
        } => {
            assert!(opened.is_none());
            assert_eq!(*actual, renderer_id);
            assert_eq!(*observed, generation);
            let Intent::ResolveAndOpen { locator, .. } = intent else {
                panic!("actual ResolveAndOpen required");
            };
            let Outcome::Opened { target, width, height } = outcome else {
                panic!("real editor must open: {outcome:?}");
            };
            assert!(locator.matches_target(*target));
            assert_eq!(locator.sequence, payload.sequence_id);
            assert_eq!(
                (target.instance, target.installed_generation),
                (original.id, original_claim.generation())
            );
            assert_eq!((*width, *height), (320, 160));
            opened = Some(*target);
            true
        }
        AudioPlaybackEvent::NativeOwnerBindingsObserved { .. } => {
            panic!("no second Source binding")
        }
        _ => false,
    });
    let opened = opened.unwrap();
    assert_eq!(
        state.native_audio_editor_surface_state(&payload),
        (true, false)
    );
    assert!(verifier.validate(&original_claim).is_ok());
    let mut native_object = None;
    // This depends on the actual adapter presentation provider. Existing Core
    // DTO constructors, HWND titles or a new test getter are not PID evidence.
    poll_idle_until(&mut state, generation, |event| match event {
        AudioPlaybackEvent::NativeParameterObservationsObserved {
            renderer_id: actual,
            generation: observed,
            observation,
        } => {
            assert_eq!(*actual, renderer_id);
            assert_eq!(*observed, generation);
            assert_eq!(observation.target(), opened);
            native_object = Some((observation.process_id(), observation.object_serial()));
            true
        }
        AudioPlaybackEvent::NativeOwnerBindingsObserved { .. }
        | AudioPlaybackEvent::NativeEditorObserved { .. } => {
            panic!("no extra binding or editor action")
        }
        _ => false,
    });
    let native_object =
        native_object.expect("real parameter observation supplies native PID and load incarnation");
    let view = owned_editor_view(native_object.0);
    assert_ne!(
        unsafe { post_window_message(view, 0x0100, b'G' as usize, 0) },
        0,
        "one real WM_KEYDOWN G, never R"
    );

    let expected_callbacks = vec![
        Callback::GroupStarted,
        Callback::Parameter {
            parameter: gain.clone(),
            phase: Phase::Begin,
            value_bits: None,
        },
        Callback::Parameter {
            parameter: gain.clone(),
            phase: Phase::Value,
            value_bits: Some(0.75_f64.to_bits()),
        },
        Callback::Parameter {
            parameter: gain.clone(),
            phase: Phase::End,
            value_bits: None,
        },
        Callback::Parameter {
            parameter: balance.clone(),
            phase: Phase::Begin,
            value_bits: None,
        },
        Callback::Parameter {
            parameter: balance.clone(),
            phase: Phase::Value,
            value_bits: Some(0.25_f64.to_bits()),
        },
        Callback::Parameter {
            parameter: balance.clone(),
            phase: Phase::End,
            value_bits: None,
        },
        Callback::DirtyChanged(true),
        Callback::GroupFinished,
        Callback::DirtyChanged(false),
    ];
    let mut delivered: Option<mondrian_core::NativeAudioAuthorProposal> = None;
    let mut acknowledged = None;
    poll_gui_until(&mut state, generation, |event| match event {
        AudioPlaybackEvent::NativeAuthorProposalObserved {
            renderer_id: actual,
            generation: observed,
            proposal,
        } => {
            assert_eq!(*actual, renderer_id);
            assert_eq!(*observed, generation);
            assert_eq!(proposal.target(), opened);
            assert_eq!(proposal.sequence(), payload.sequence_id);
            assert_eq!(proposal.callbacks(), expected_callbacks.as_slice());
            assert_eq!(proposal.revision(), 10);
            assert_eq!(proposal.completion(), Completion::CoherentConfiguration);
            assert!(proposal.terminal());
            assert_eq!(proposal.open_parameter_edits(), 0);
            assert_eq!(proposal.group_depth(), 0);
            assert_editor_configuration(
                proposal.state().expect("actual coherent state").as_ref(),
                0.75,
                0.25,
                1,
            );
            if let Some(prior) = &delivered {
                assert!(prior.same_retained_proposal(proposal));
            } else {
                delivered = Some(proposal.as_ref().clone());
            }
            false
        }
        AudioPlaybackEvent::NativeAuthorHandoffObserved {
            renderer_id: actual,
            generation: observed,
            proposal,
            status,
        } => {
            assert_eq!(*actual, renderer_id);
            assert_eq!(*observed, generation);
            assert!(delivered
                .as_ref()
                .expect("actual proposal preceded handoff")
                .same_retained_proposal(proposal));
            let Status::Acknowledged { target, continuity } = status else {
                panic!("same-source install/exact Ack required: {status:?}");
            };
            assert_eq!(*continuity, Continuity::Preserved);
            assert!(acknowledged.is_none());
            assert_eq!(target.root_session, opened.root_session);
            assert_eq!(target.local_session, opened.local_session);
            assert_eq!(target.instance, opened.instance);
            acknowledged = Some(**target);
            true
        }
        AudioPlaybackEvent::NativeEditorAttachmentObserved {
            renderer_id: actual,
            generation: observed,
            observation,
        } => {
            assert_eq!(*actual, renderer_id);
            assert_eq!(*observed, generation);
            assert_eq!(observation.previous, opened);
            assert!(observation.retained);
            assert_eq!(
                (observation.process_id, observation.object_serial),
                native_object
            );
            false
        }
        AudioPlaybackEvent::NativeParameterObservationsObserved {
            renderer_id: actual,
            generation: observed,
            observation,
        } => {
            assert_eq!(*actual, renderer_id);
            assert_eq!(*observed, generation);
            assert_eq!(
                (observation.process_id(), observation.object_serial()),
                native_object
            );
            false
        }
        AudioPlaybackEvent::NativeDiagnosticsObserved {
            renderer_id: actual,
            generation: observed,
            ..
        } => {
            assert_eq!(*actual, renderer_id);
            assert_eq!(*observed, generation);
            false
        }
        other => panic!("unexpected work, restart/rescan, editor or transport event: {other:?}"),
    });
    let acknowledged = acknowledged.unwrap();
    let delivered = delivered.unwrap();
    assert_eq!(
        state.native_audio_editor_surface_state(&payload),
        (true, false)
    );
    assert!(
        author_before.enter_commit().is_err(),
        "the real author transaction superseded the original snapshot"
    );
    let author = state.authoring.as_ref().unwrap();
    assert_ne!(author.author_generation(), author_before.generation());
    let history_after = author.history().diagnostics();
    assert_eq!(history_after.undo_entries, history_before.undo_entries + 1);
    assert_eq!(history_after.pending_entries, 0);
    assert_eq!(history_after.redo_entries, 0);
    assert!(author.history().can_undo());
    assert!(
        author.is_dirty(),
        "native Dirty(false) is not a saved Project"
    );
    let after = &state.active_sequence().unwrap().audio_program.outputs[0]
        .strip
        .pre_fader
        .processors[0];
    assert_eq!(after.id, original.id);
    assert_eq!(after.definition, original.definition);
    assert_eq!(after.parameters.len(), original.parameters.len());
    for (id, prior) in &original.parameters {
        let current = &after.parameters[id];
        assert_eq!(current.schema, prior.schema);
        assert_eq!(current.ui, prior.ui);
        if *id == gain || *id == balance {
            let curve =
                current.automation.as_ref().expect("actual GUI edit creates the author curve");
            assert_eq!(&curve.parameter_id, id);
            assert!(curve.keyframes.is_empty());
            let expected = if *id == gain { 0.75_f64 } else { 0.25_f64 };
            assert_eq!(curve.default_value.to_bits(), expected.to_bits());
        } else {
            assert_eq!(id, &mode);
            assert!(
                current.automation.is_none(),
                "unedited native mode is not adopted"
            );
        }
    }
    assert_eq!(after.parameters[&mode], original.parameters[&mode]);
    assert_ne!(after.opaque_state, original.opaque_state);
    assert_eq!(after.opaque_state.as_ref(), delivered.state());
    assert_editor_configuration(after.opaque_state.as_ref().unwrap().as_ref(), 0.75, 0.25, 1);
    let accepted_author_state = serde_json::to_value(after).unwrap();
    // A legal Plan installation keeps the Source while replacing its Runtime
    // registration. The old verifier must lose permission, rather than follow
    // that replacement implicitly.
    let installed_verifier = weak
        .upgrade()
        .expect("same original Timeline after legitimate installation")
        .owner_installation_verifier()
        .expect("actual current Runtime registration");
    assert!(verifier.same_source(&installed_verifier));
    assert!(!verifier.same_registration(&installed_verifier));
    assert_eq!(
        verifier.current_claim().unwrap_err(),
        mondrian_core::AudioOwnerInstallationError::Superseded
    );
    assert_eq!(
        verifier.validate(&original_claim),
        Err(mondrian_core::AudioOwnerInstallationError::Superseded)
    );
    let installed_claim = installed_verifier
        .current_claim()
        .expect("current registration in the same Source");
    assert!(installed_verifier.validate(&installed_claim).is_ok());
    assert!(installed_claim.generation() > original_claim.generation());
    assert_eq!(
        acknowledged.installed_generation,
        installed_claim.generation()
    );
    assert!(state
        .native_audio_renderer
        .as_ref()
        .is_some_and(|actual| std::sync::Weak::ptr_eq(actual, &weak)));
    assert!(weak.upgrade().unwrap().native_author_snapshot_current());

    let mut post_object = false;
    poll_gui_until(&mut state, generation, |event| match event {
        AudioPlaybackEvent::NativeParameterObservationsObserved {
            renderer_id: actual,
            generation: observed,
            observation,
        } => {
            assert_eq!(*actual, renderer_id);
            assert_eq!(*observed, generation);
            assert_eq!(observation.target(), acknowledged);
            assert_eq!(
                (observation.process_id(), observation.object_serial()),
                native_object
            );
            for (id, value) in [(&gain, 0.75_f64), (&balance, 0.25_f64)] {
                assert!(observation
                    .facts()
                    .iter()
                    .any(|fact| fact.parameter == *id && fact.value_bits == Some(value.to_bits())));
            }
            post_object = true;
            true
        }
        AudioPlaybackEvent::NativeEditorAttachmentObserved {
            renderer_id: actual,
            generation: observed,
            observation,
        } => {
            assert_eq!(*actual, renderer_id);
            assert_eq!(*observed, generation);
            assert_eq!(observation.previous, opened);
            assert_eq!(observation.current, acknowledged);
            assert!(observation.retained);
            assert_eq!(
                (observation.process_id, observation.object_serial),
                native_object
            );
            false
        }
        AudioPlaybackEvent::NativeDiagnosticsObserved { .. } => false,
        other => panic!("settled A must not replay or hide new control work: {other:?}"),
    });
    assert!(post_object);
    let mut close = payload.clone();
    close.open = false;
    state
        .dispatch_action(
            ProductAction::Audio(AudioProductAction::NativeEditor(close)).into_external_action(),
        )
        .expect("original App close action for its actual retained attachment");
    assert_eq!(
        state.native_audio_editor_surface_state(&payload),
        (true, true)
    );
    let mut closed = false;
    poll_gui_until(&mut state, generation, |event| match event {
        AudioPlaybackEvent::NativeEditorObserved {
            renderer_id: actual,
            generation: observed,
            intent,
            outcome,
        } => {
            assert_eq!(*actual, renderer_id);
            assert_eq!(*observed, generation);
            let Intent::Close { target } = intent else {
                panic!("only actual close was queued");
            };
            assert_eq!(
                *target, opened,
                "the original view attachment survives the parameter Plan swap"
            );
            let Outcome::Closed { target } = outcome else {
                panic!("actual view must close: {outcome:?}");
            };
            assert_eq!(target.root_session, acknowledged.root_session);
            assert_eq!(target.local_session, acknowledged.local_session);
            assert_eq!(target.instance, acknowledged.instance);
            assert!(!closed);
            closed = true;
            true
        }
        AudioPlaybackEvent::NativeEditorAttachmentObserved { observation, .. } => {
            assert_eq!(
                (observation.process_id, observation.object_serial),
                native_object
            );
            assert_eq!(observation.previous, opened);
            assert_eq!(observation.current, acknowledged);
            assert!(observation.retained);
            false
        }
        AudioPlaybackEvent::NativeParameterObservationsObserved { observation, .. } => {
            assert_eq!(
                (observation.process_id(), observation.object_serial()),
                native_object
            );
            assert_eq!(observation.target(), acknowledged);
            false
        }
        AudioPlaybackEvent::NativeDiagnosticsObserved { .. } => false,
        other => panic!("no additional author/restart work while closing: {other:?}"),
    });
    assert!(closed);
    assert_eq!(unsafe { is_window(view) }, 0);
    assert_eq!(
        state.native_audio_editor_surface_state(&payload),
        (false, false)
    );
    assert_eq!(
        state.status_hint().map(|(message, _)| message.id),
        Some("native-editor-closed")
    );
    assert_eq!(
        serde_json::to_value(
            &state.active_sequence().unwrap().audio_program.outputs[0]
                .strip
                .pre_fader
                .processors[0]
        )
        .unwrap(),
        accepted_author_state
    );
    assert_eq!(
        state.authoring.as_ref().unwrap().history().diagnostics(),
        history_after
    );
    assert!(installed_verifier.validate(&installed_claim).is_ok());
    assert_eq!(state.playback_engine.snapshot(), engine_before);
    assert!(state.playback_engine.native_audio_control_hold_ticket().is_none());
    assert!(state.audio_playback.native_control_hold_generation().is_none());
    assert!(state.audio_playback.native_reprepare_pending_generation().is_none());
    assert_eq!(state.audio_playback_snapshot().next_start_sample, 0);
    assert_eq!(state.audio_playback_snapshot().in_flight, 0);
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
        installed_verifier.current_claim(),
        Err(mondrian_core::AudioOwnerInstallationError::NotInstalled)
    ));
    assert!(matches!(
        verifier.current_claim(),
        Err(mondrian_core::AudioOwnerInstallationError::NotInstalled)
    ));
    assert!(weak.upgrade().is_none());
    // The whole App success path is real, with no PCM windows or device. Zero-
    // sample catchup/query/control calls are not a DSP algorithm, physical GUI,
    // real-time or individual cold-probe custody qualification.
    drop(state);
}

struct ClosedOwnedClapAuthorControl {
    control: ColdNativeControl,
    payload: super::super::super::product_action::AudioNativeEditorPayload,
    verifier: mondrian_core::AudioOwnerInstallationVerifier,
    installed_verifier: mondrian_core::AudioOwnerInstallationVerifier,
    installed_claim: mondrian_core::AudioOwnerInstallationClaim,
    target: mondrian_core::NativeAudioEditorTarget,
}

fn prepare_app_clap_editor_author_closed() -> ClosedOwnedClapAuthorControl {
    prepare_app_clap_editor_author_closed_with_hooks(|_, _, _| {}, |_| {})
}

fn prepare_app_clap_editor_author_closed_with_hooks(
    initialize: impl FnOnce(&mut Sequence, &Arc<AssetLibrary>, &std::path::Path),
    mut before_ack: impl FnMut(&mut AppState),
) -> ClosedOwnedClapAuthorControl {
    use super::super::super::product_action::{
        AudioNativeEditorPayload, AudioProductAction, ProductAction,
    };
    use mondrian_core::{
        NativeAudioAuthorCallback as Callback, NativeAudioAuthorCompletion as Completion,
        NativeAudioEditorControlIntent as Intent, NativeAudioEditorControlOutcome as Outcome,
        NativeAudioParameterCallbackPhase as Phase,
    };
    use mondrian_media::{
        AudioPcmNativeAuthorHandoffContinuity as Continuity,
        AudioPcmNativeAuthorHandoffStatus as Status,
    };

    let ColdNativeControl { mut state, _directory, output, renderer: weak } =
        prepare_app_clap_editor_control_with_initializer(initialize);
    let sequence = state.active_sequence().expect("actual author Sequence");
    assert!(sequence.audio_program.note_lanes.is_empty());
    let native_output = &sequence.audio_program.outputs[0];
    let original = native_output.strip.pre_fader.processors[0].clone();
    let volume = mondrian_core::ParameterId::new("clap.param.1").unwrap();
    assert_eq!(original.parameters.len(), 1);
    let original_parameter = &original.parameters[&volume];
    assert_eq!(original_parameter.schema.parameter_id, volume);
    let numeric = original_parameter.schema.numeric.as_ref().expect("native Volume range");
    assert_eq!(numeric.len(), 1);
    assert_eq!(numeric[0].hard_range.min.to_bits(), 0.0_f64.to_bits());
    assert_eq!(numeric[0].hard_range.max.to_bits(), 1.0_f64.to_bits());
    assert!(
        original_parameter.automation.is_none(),
        "captured Volume stays native-owned"
    );
    assert_eq!(
        original.opaque_state.as_ref().expect("actual initial state").as_slice(),
        &1.0_f32.to_le_bytes()
    );
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
        processor_id: original.id,
        definition: original.definition.clone(),
        sequence_time: mondrian_core::DomainTime::new(
            mondrian_core::AuthoringTimeDomain::Sequence(sequence.id),
            TimelineTime::ZERO,
        ),
        open: true,
    };
    let author_before = state.authoring.as_ref().unwrap().execution_snapshot();
    let history_before = state.authoring.as_ref().unwrap().history().diagnostics();
    assert_eq!(history_before.undo_entries, 0);
    assert_eq!(history_before.pending_entries, 0);
    let engine_before = state.playback_engine.snapshot();
    let verifier = weak
        .upgrade()
        .expect("original Timeline")
        .owner_installation_verifier()
        .unwrap();
    let original_claim = verifier.current_claim().unwrap();
    assert!(verifier.validate(&original_claim).is_ok());
    let generation = state.audio_playback_snapshot().generation;
    let mut renderer_id = None;
    poll_idle_until(&mut state, generation, |event| match event {
        AudioPlaybackEvent::NativeOwnerBindingsObserved {
            renderer_id: actual,
            generation: observed,
            status,
        } => {
            assert!(renderer_id.is_none());
            assert_eq!(*observed, generation);
            assert_eq!(
                *status,
                mondrian_media::AudioPcmNativeOwnerBindingStatus::Ready
            );
            renderer_id = Some(*actual);
            true
        }
        AudioPlaybackEvent::NativeEditorObserved { .. } => panic!("no open before actual Ready"),
        _ => false,
    });
    let renderer_id = renderer_id.unwrap();
    assert!(state.native_audio_editor.is_none());
    state
        .dispatch_action(
            ProductAction::Audio(AudioProductAction::NativeEditor(payload.clone()))
                .into_external_action(),
        )
        .expect("original App open action");
    assert_eq!(
        state.native_audio_editor_surface_state(&payload),
        (false, true)
    );
    let mut parameters = None;
    let mut opened = None;
    poll_idle_until(&mut state, generation, |event| match event {
        AudioPlaybackEvent::NativeEditorObserved {
            renderer_id: actual,
            generation: observed,
            intent,
            outcome,
        } => {
            assert!(opened.is_none());
            assert_eq!(*actual, renderer_id);
            assert_eq!(*observed, generation);
            let Intent::ResolveAndOpen { locator, .. } = intent else {
                panic!("actual ResolveAndOpen required");
            };
            let Outcome::Opened { target, width, height } = outcome else {
                panic!("real CLAP editor must open: {outcome:?}");
            };
            assert!(locator.matches_target(*target));
            assert_eq!(locator.sequence, payload.sequence_id);
            assert_eq!(
                (target.instance, target.installed_generation),
                (original.id, original_claim.generation())
            );
            assert_eq!((*width, *height), (320, 100));
            opened = Some(*target);
            true
        }
        AudioPlaybackEvent::NativeParameterObservationsObserved {
            renderer_id: actual,
            generation: observed,
            observation,
        } => {
            assert_eq!(*actual, renderer_id);
            assert_eq!(*observed, generation);
            parameters = Some(observation.as_ref().clone());
            false
        }
        AudioPlaybackEvent::NativeOwnerBindingsObserved { .. } => {
            panic!("no second Source binding")
        }
        _ => false,
    });
    let opened = opened.unwrap();
    assert_eq!(
        state.native_audio_editor_surface_state(&payload),
        (true, false)
    );
    assert!(verifier.validate(&original_claim).is_ok());
    // CLAP emits one real query per Source revision. The Open completion poll
    // may already contain that receipt; never require a manufactured second query.
    if parameters.is_none() {
        poll_idle_until(&mut state, generation, |event| match event {
            AudioPlaybackEvent::NativeParameterObservationsObserved {
                renderer_id: actual,
                generation: observed,
                observation,
            } => {
                assert_eq!(*actual, renderer_id);
                assert_eq!(*observed, generation);
                parameters = Some(observation.as_ref().clone());
                true
            }
            AudioPlaybackEvent::NativeOwnerBindingsObserved { .. }
            | AudioPlaybackEvent::NativeEditorObserved { .. } => {
                panic!("no extra binding or editor action")
            }
            _ => false,
        });
    }
    let initial = parameters.take().expect("actual initial presentation receipt");
    assert_eq!(initial.target(), opened);
    let fact = initial
        .facts()
        .iter()
        .rev()
        .find(|fact| fact.parameter == volume)
        .expect("actual Volume presentation");
    assert_eq!(fact.value_bits, Some(1.0_f64.to_bits()));
    assert_eq!(fact.display.as_deref(), Some("100.00 %"));
    let native_object = (initial.process_id(), initial.object_serial());
    let view = owned_editor_view_matching(
        native_object.0,
        b"MondrianClapGainEditor_",
        false,
        Some(b"Mondrian CLAP Gain Editor"),
    );
    // A real attached GUI blocks ProjectClose before stop or persistence.
    // Continue the original author path afterwards on that exact live owner.
    assert!(state.native_audio_editor.as_ref().unwrap().blocks_source_replacement());
    let close_error = state.close_project().expect_err("actual Opened view blocks ProjectClose");
    assert!(
        matches!(close_error.downcast_ref::<mondrian_core::MondrianError>(),
        Some(mondrian_core::MondrianError::ActionNotExecuted { action, reason })
        if action == "project_audio_handoff"
            && reason == "native author or editor work must settle before Project handoff"),
        "unexpected ProjectClose refusal: {close_error:?}"
    );
    assert_eq!(state.playback_engine.snapshot(), engine_before);
    assert_eq!(state.audio_playback_snapshot().generation, generation);
    assert_eq!(state.audio_playback_snapshot().next_start_sample, 0);
    assert_eq!(state.audio_playback_snapshot().in_flight, 0);
    assert!(verifier.validate(&original_claim).is_ok());
    assert!(state.audio_playback.native_editor_source_available());
    assert!(state
        .native_audio_renderer
        .as_ref()
        .is_some_and(|actual| std::sync::Weak::ptr_eq(actual, &weak)));
    assert_eq!(
        state.native_audio_editor_surface_state(&payload),
        (true, false)
    );
    assert!(state.native_audio_editor.as_ref().unwrap().blocks_source_replacement());
    assert_eq!(state.authoring_session_id(), Some(payload.session_id));
    assert_eq!(
        state.authoring.as_ref().unwrap().history().diagnostics(),
        history_before
    );
    assert!(state.pending_project_close.is_none());
    assert!(state.project_close_fault.is_none());
    assert!(!state.project_lifecycle_blocks_actions());
    assert_eq!(output.lock().queued, 0);
    assert_ne!(unsafe { is_window(view) }, 0);
    assert_ne!(
        unsafe { post_window_message(view, 0x0100, b'G' as usize, 0) },
        0,
        "one real WM_KEYDOWN G enters the actual CLAP window procedure"
    );

    let expected_callbacks = [
        Callback::Parameter {
            parameter: volume.clone(),
            phase: Phase::Begin,
            value_bits: None,
        },
        Callback::Parameter {
            parameter: volume.clone(),
            phase: Phase::Value,
            value_bits: Some(0.75_f64.to_bits()),
        },
        Callback::Parameter {
            parameter: volume.clone(),
            phase: Phase::End,
            value_bits: None,
        },
    ];
    let mut delivered: Option<mondrian_core::NativeAudioAuthorProposal> = None;
    let mut acknowledged = None;
    let observe = |event: &AudioPlaybackEvent| match event {
        AudioPlaybackEvent::NativeAuthorProposalObserved {
            renderer_id: actual,
            generation: observed,
            proposal,
        } => {
            assert_eq!(*actual, renderer_id);
            assert_eq!(*observed, generation);
            assert_eq!(proposal.target(), opened);
            assert_eq!(proposal.sequence(), payload.sequence_id);
            assert_eq!(proposal.callbacks(), expected_callbacks.as_slice());
            assert_eq!(proposal.revision(), 3);
            assert_eq!(proposal.completion(), Completion::CoherentConfiguration);
            assert!(proposal.terminal());
            assert_eq!(proposal.open_parameter_edits(), 0);
            assert_eq!(proposal.group_depth(), 0);
            assert_eq!(
                proposal.state().expect("actual CLAP coherent state").as_slice(),
                &0.75_f32.to_le_bytes()
            );
            if let Some(prior) = &delivered {
                assert!(prior.same_retained_proposal(proposal));
            } else {
                delivered = Some(proposal.as_ref().clone());
            }
            false
        }
        AudioPlaybackEvent::NativeAuthorHandoffObserved {
            renderer_id: actual,
            generation: observed,
            proposal,
            status,
        } => {
            assert_eq!(*actual, renderer_id);
            assert_eq!(*observed, generation);
            assert!(delivered
                .as_ref()
                .expect("proposal precedes handoff")
                .same_retained_proposal(proposal));
            let Status::Acknowledged { target, continuity } = status else {
                panic!("same-Source install and exact Ack required: {status:?}");
            };
            assert_eq!(*continuity, Continuity::Preserved);
            assert!(acknowledged.is_none());
            assert_eq!(target.root_session, opened.root_session);
            assert_eq!(target.local_session, opened.local_session);
            assert_eq!(target.instance, opened.instance);
            acknowledged = Some(**target);
            true
        }
        AudioPlaybackEvent::NativeEditorAttachmentObserved {
            renderer_id: actual,
            generation: observed,
            observation,
        } => {
            assert_eq!(*actual, renderer_id);
            assert_eq!(*observed, generation);
            assert_eq!(observation.previous, opened);
            assert!(observation.retained);
            assert_eq!(
                (observation.process_id, observation.object_serial),
                native_object
            );
            false
        }
        AudioPlaybackEvent::NativeParameterObservationsObserved {
            renderer_id: actual,
            generation: observed,
            observation,
        } => {
            assert_eq!(*actual, renderer_id);
            assert_eq!(*observed, generation);
            assert_eq!(
                (observation.process_id(), observation.object_serial()),
                native_object
            );
            parameters = Some(observation.as_ref().clone());
            false
        }
        AudioPlaybackEvent::NativeDiagnosticsObserved {
            renderer_id: actual,
            generation: observed,
            ..
        } => {
            assert_eq!(*actual, renderer_id);
            assert_eq!(*observed, generation);
            false
        }
        other => panic!("unexpected restart, transport or editor work: {other:?}"),
    };
    poll_gui_until_after_proposal(&mut state, generation, observe, &mut before_ack);
    let acknowledged = acknowledged.unwrap();
    let delivered = delivered.unwrap();
    assert_eq!(
        state.native_audio_editor_surface_state(&payload),
        (true, false)
    );
    assert!(author_before.enter_commit().is_err());
    let author = state.authoring.as_ref().unwrap();
    assert_ne!(author.author_generation(), author_before.generation());
    let history_after = author.history().diagnostics();
    assert_eq!(history_after.undo_entries, history_before.undo_entries + 1);
    assert_eq!(history_after.pending_entries, 0);
    assert_eq!(history_after.redo_entries, 0);
    assert!(author.history().can_undo());
    assert!(author.is_dirty());
    let after = &state.active_sequence().unwrap().audio_program.outputs[0]
        .strip
        .pre_fader
        .processors[0];
    assert_eq!(after.id, original.id);
    assert_eq!(after.definition, original.definition);
    assert_eq!(after.parameters.len(), 1);
    let parameter = &after.parameters[&volume];
    assert_eq!(parameter.schema, original_parameter.schema);
    assert_eq!(parameter.ui, original_parameter.ui);
    let curve = parameter.automation.as_ref().expect("original unkeyed lane retained");
    assert_eq!(curve.parameter_id, volume);
    assert!(curve.keyframes.is_empty());
    assert_eq!(curve.default_value.to_bits(), 0.75_f64.to_bits());
    assert_ne!(after.opaque_state, original.opaque_state);
    assert_eq!(after.opaque_state.as_ref(), delivered.state());
    assert_eq!(
        after.opaque_state.as_ref().unwrap().as_slice(),
        &0.75_f32.to_le_bytes()
    );
    let accepted_author_state = serde_json::to_value(after).unwrap();
    let installed_verifier = weak
        .upgrade()
        .expect("same original Timeline")
        .owner_installation_verifier()
        .expect("current Runtime registration");
    assert!(verifier.same_source(&installed_verifier));
    assert!(!verifier.same_registration(&installed_verifier));
    assert_eq!(
        verifier.current_claim().unwrap_err(),
        mondrian_core::AudioOwnerInstallationError::Superseded
    );
    assert_eq!(
        verifier.validate(&original_claim),
        Err(mondrian_core::AudioOwnerInstallationError::Superseded)
    );
    let installed_claim = installed_verifier.current_claim().expect("same Source current claim");
    assert!(installed_verifier.validate(&installed_claim).is_ok());
    assert!(installed_claim.generation() > original_claim.generation());
    assert_eq!(
        acknowledged.installed_generation,
        installed_claim.generation()
    );
    assert!(state
        .native_audio_renderer
        .as_ref()
        .is_some_and(|actual| std::sync::Weak::ptr_eq(actual, &weak)));
    assert!(weak.upgrade().unwrap().native_author_snapshot_current());

    if !parameters.as_ref().is_some_and(|actual| actual.target() == acknowledged) {
        poll_gui_until(&mut state, generation, |event| match event {
            AudioPlaybackEvent::NativeParameterObservationsObserved {
                renderer_id: actual,
                generation: observed,
                observation,
            } => {
                assert_eq!(*actual, renderer_id);
                assert_eq!(*observed, generation);
                assert_eq!(observation.target(), acknowledged);
                assert_eq!(
                    (observation.process_id(), observation.object_serial()),
                    native_object
                );
                let fact = observation
                    .facts()
                    .iter()
                    .rev()
                    .find(|fact| fact.parameter == volume)
                    .expect("fresh Volume facts for the installed binding");
                assert_eq!(fact.value_bits, Some(0.75_f64.to_bits()));
                assert_eq!(fact.display.as_deref(), Some("75.00 %"));
                parameters = Some(observation.as_ref().clone());
                true
            }
            AudioPlaybackEvent::NativeEditorAttachmentObserved {
                renderer_id: actual,
                generation: observed,
                observation,
            } => {
                assert_eq!(*actual, renderer_id);
                assert_eq!(*observed, generation);
                assert_eq!(observation.previous, opened);
                assert_eq!(observation.current, acknowledged);
                assert!(observation.retained);
                assert_eq!(
                    (observation.process_id, observation.object_serial),
                    native_object
                );
                false
            }
            AudioPlaybackEvent::NativeDiagnosticsObserved {
                renderer_id: actual,
                generation: observed,
                ..
            } => {
                assert_eq!(*actual, renderer_id);
                assert_eq!(*observed, generation);
                false
            }
            other => panic!("settled CLAP prefix may not replay or hide control work: {other:?}"),
        });
    }
    let installed_parameters = parameters.expect("actual post-install current-target receipt");
    assert_eq!(installed_parameters.target(), acknowledged);
    assert_eq!(
        (
            installed_parameters.process_id(),
            installed_parameters.object_serial()
        ),
        native_object
    );
    let fact = installed_parameters
        .facts()
        .iter()
        .rev()
        .find(|fact| fact.parameter == volume)
        .expect("fresh Volume facts at the installed binding");
    assert_eq!(fact.value_bits, Some(0.75_f64.to_bits()));
    assert_eq!(fact.display.as_deref(), Some("75.00 %"));
    let presentation = state
        .native_audio_parameter_presentation(&payload, &volume)
        .expect("App stores actual post-install current-target presentation");
    assert_eq!(presentation.value_bits, Some(0.75_f64.to_bits()));
    assert_eq!(presentation.display.as_deref(), Some("75.00 %"));
    let mut close = payload.clone();
    close.open = false;
    state
        .dispatch_action(
            ProductAction::Audio(AudioProductAction::NativeEditor(close)).into_external_action(),
        )
        .expect("original App close action");
    assert_eq!(
        state.native_audio_editor_surface_state(&payload),
        (true, true)
    );
    poll_gui_until(&mut state, generation, |event| match event {
        AudioPlaybackEvent::NativeEditorObserved {
            renderer_id: actual,
            generation: observed,
            intent,
            outcome,
        } => {
            assert_eq!(*actual, renderer_id);
            assert_eq!(*observed, generation);
            let Intent::Close { target } = intent else {
                panic!("only actual Close queued")
            };
            assert_eq!(
                *target, opened,
                "original view attachment survives the Plan swap"
            );
            let Outcome::Closed { target } = outcome else {
                panic!("actual CLAP view must close: {outcome:?}");
            };
            assert_eq!(target.root_session, acknowledged.root_session);
            assert_eq!(target.local_session, acknowledged.local_session);
            assert_eq!(target.instance, acknowledged.instance);
            true
        }
        AudioPlaybackEvent::NativeEditorAttachmentObserved {
            renderer_id: actual,
            generation: observed,
            observation,
        } => {
            assert_eq!(*actual, renderer_id);
            assert_eq!(*observed, generation);
            assert_eq!(observation.previous, opened);
            assert_eq!(observation.current, acknowledged);
            assert!(observation.retained);
            assert_eq!(
                (observation.process_id, observation.object_serial),
                native_object
            );
            false
        }
        AudioPlaybackEvent::NativeParameterObservationsObserved {
            renderer_id: actual,
            generation: observed,
            observation,
        } => {
            assert_eq!(*actual, renderer_id);
            assert_eq!(*observed, generation);
            assert_eq!(observation.target(), acknowledged);
            assert_eq!(
                (observation.process_id(), observation.object_serial()),
                native_object
            );
            false
        }
        AudioPlaybackEvent::NativeDiagnosticsObserved {
            renderer_id: actual,
            generation: observed,
            ..
        } => {
            assert_eq!(*actual, renderer_id);
            assert_eq!(*observed, generation);
            false
        }
        other => panic!("no additional author work during Close: {other:?}"),
    });
    assert_eq!(unsafe { is_window(view) }, 0);
    assert_eq!(
        state.native_audio_editor_surface_state(&payload),
        (false, false)
    );
    assert_eq!(
        state.status_hint().map(|(message, _)| message.id),
        Some("native-editor-closed")
    );
    assert_eq!(
        serde_json::to_value(
            &state.active_sequence().unwrap().audio_program.outputs[0]
                .strip
                .pre_fader
                .processors[0]
        )
        .unwrap(),
        accepted_author_state
    );
    assert_eq!(
        state.authoring.as_ref().unwrap().history().diagnostics(),
        history_after
    );
    assert!(installed_verifier.validate(&installed_claim).is_ok());
    assert_eq!(state.playback_engine.snapshot(), engine_before);
    assert!(state.playback_engine.native_audio_control_hold_ticket().is_none());
    assert!(state.audio_playback.native_control_hold_generation().is_none());
    assert!(state.audio_playback.native_reprepare_pending_generation().is_none());
    assert_eq!(state.audio_playback_snapshot().next_start_sample, 0);
    assert_eq!(state.audio_playback_snapshot().in_flight, 0);
    assert_eq!(output.lock().queued, 0);
    ClosedOwnedClapAuthorControl {
        control: ColdNativeControl { state, _directory, output, renderer: weak },
        payload,
        verifier,
        installed_verifier,
        installed_claim,
        target: acknowledged,
    }
}

#[test]
#[ignore = "current normal Product and owned CLAP Gain editor DLL; actual App author handoff, no PCM/device"]
fn app_owned_clap_editor_parameter_commits_through_same_source() {
    let ClosedOwnedClapAuthorControl {
        control: ColdNativeControl { mut state, _directory, output: _, renderer: weak },
        verifier,
        installed_verifier,
        ..
    } = prepare_app_clap_editor_author_closed();
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
    for verifier in [&installed_verifier, &verifier] {
        assert!(matches!(
            verifier.current_claim(),
            Err(mondrian_core::AudioOwnerInstallationError::NotInstalled)
        ));
    }
    assert!(weak.upgrade().is_none());
    // Real normal-Product/CLAP GUI and App author flow only. Idle servicing is
    // not zero native calls, acoustic/DSP, device, RT or broader GUI qualification.
    drop(state);
}

#[test]
#[ignore = "current normal Product and owned CLAP Gain editor DLL; public Idle Undo/Redo native-state restoration, no PCM/device"]
fn app_owned_clap_editor_undo_redo_restores_native_state() {
    use super::super::super::product_action::{AudioProductAction, ProductAction};
    use anyhow::Context as _;
    use mondrian_core::{
        AudioOwnerInstallationError, NativeAudioEditorControlIntent as Intent,
        NativeAudioEditorControlOutcome as Outcome,
    };

    let ClosedOwnedClapAuthorControl {
        control: ColdNativeControl { mut state, _directory, output, renderer: weak },
        payload,
        installed_verifier,
        installed_claim,
        ..
    } = prepare_app_clap_editor_author_closed();
    let volume = mondrian_core::ParameterId::new("clap.param.1").unwrap();
    let mut previous_verifier = installed_verifier;
    let mut previous_claim = installed_claim;
    let mut stage = "after-original-close";
    let mut observations = Vec::new();
    // Only the NEW history stages are fenced. Original setup/G/Ack/Close
    // assertions remain unchanged; a history-stage panic is preserved as the
    // final target failure after the original consuming cleanup has completed.
    let history_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
        || -> anyhow::Result<()> {
            for (redo, expected, text, undo_entries, redo_entries) in [
                (false, 1.0_f64, "100.00 %", 0, 1),
                (true, 0.75_f64, "75.00 %", 1, 0),
            ] {
                stage = if redo { "redo-author" } else { "undo-author" };
                anyhow::ensure!(
                    previous_verifier.validate(&previous_claim).is_ok(),
                    "history starts from the actual post-Close current claim"
                );
                let restored = if redo {
                    state.redo_timeline()
                } else {
                    state.undo_timeline()
                }
                .with_context(|| format!("public {stage}"))?;
                anyhow::ensure!(
                    restored,
                    "public {stage} must restore its original history entry"
                );
                let author =
                    state.authoring.as_ref().context("actual restored Authoring Session")?;
                let history = author.history().diagnostics();
                anyhow::ensure!(
                    state.authoring_session_id() == Some(payload.session_id)
                        && state.current_frame() == 0
                        && !state.is_playing(),
                    "public history keeps the original author Session and Idle zero coordinate"
                );
                anyhow::ensure!(
                    history.undo_entries == undo_entries
                        && history.redo_entries == redo_entries
                        && history.pending_entries == 0,
                    "{stage} moves exactly the one native-author history entry: {history:?}"
                );
                let instance =
                    &state.active_sequence().context("restored Sequence")?.audio_program.outputs[0]
                        .strip
                        .pre_fader
                        .processors[0];
                anyhow::ensure!(
                    instance.id == payload.processor_id
                        && instance.definition == payload.definition,
                    "history preserves the authored processor identity and definition"
                );
                let parameter =
                    instance.parameters.get(&volume).context("restored Volume schema/lane")?;
                if redo {
                    let curve =
                        parameter.automation.as_ref().context("restored explicit Volume")?;
                    anyhow::ensure!(
                        curve.parameter_id == volume
                            && curve.default_value.to_bits() == expected.to_bits()
                            && curve.keyframes.is_empty(),
                        "{stage} restores the explicit unkeyed Volume"
                    );
                } else {
                    anyhow::ensure!(
                        parameter.automation.is_none(),
                        "{stage} restores the original native-owned Volume"
                    );
                }
                anyhow::ensure!(
                    instance.opaque_state.as_ref().map(|value| value.as_slice())
                        == Some((expected as f32).to_le_bytes().as_slice()),
                    "{stage} restores the exact original four native-state bytes"
                );

                stage = if redo {
                    "redo-source-retirement"
                } else {
                    "undo-source-retirement"
                };
                let previous_claim_status = previous_verifier.current_claim();
                let source_available = state.audio_playback.native_editor_source_available();
                let presentation = state.native_audio_parameter_presentation(&payload, &volume);
                observations.push(serde_json::json!({
                    "stage": stage,
                    "author_value_bits": parameter.automation.as_ref().map(|curve| curve.default_value.to_bits()),
                    "expected_native_value_bits": expected.to_bits(),
                    "native_state": instance.opaque_state.as_ref().map(|value| value.as_slice()),
                    "undo_entries": history.undo_entries, "redo_entries": history.redo_entries,
                    "old_claim": format!("{previous_claim_status:?}"),
                    "source_available": source_available,
                    "old_presentation_value_bits": presentation.and_then(|fact| fact.value_bits),
                    "old_presentation_display": presentation.and_then(|fact| fact.display.as_deref()),
                }));
                anyhow::ensure!(matches!(previous_claim_status, Err(AudioOwnerInstallationError::NotInstalled))
                    && !source_available && presentation.is_none(),
                    "{stage}: current post-G/Ack/Close claim must be NotInstalled, Media Source absent and old presentation unavailable");

                stage = if redo { "redo-open" } else { "undo-open" };
                state
                    .dispatch_action(
                        ProductAction::Audio(AudioProductAction::NativeEditor(payload.clone()))
                            .into_external_action(),
                    )
                    .with_context(|| format!("original App {stage} action"))?;
                let generation = state.audio_playback_snapshot().generation;
                let mut opened = None;
                let mut parameters = None;
                let mut renderer_id = None;
                poll_gui_until(&mut state, generation, |event| match event {
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
                        assert!(renderer_id.replace(*actual).is_none());
                        false
                    }
                    AudioPlaybackEvent::NativeEditorObserved {
                        renderer_id: actual,
                        generation: observed,
                        intent,
                        outcome,
                    } => {
                        assert_eq!(*observed, generation);
                        if let Some(prior) = renderer_id {
                            assert_eq!(*actual, prior);
                        }
                        renderer_id = Some(*actual);
                        let Intent::ResolveAndOpen { locator, .. } = intent else {
                            panic!("actual history ResolveAndOpen: {intent:?}")
                        };
                        let Outcome::Opened { target, width, height } = outcome else {
                            panic!("actual history Open must succeed: {outcome:?}")
                        };
                        assert!(locator.matches_target(*target));
                        assert_eq!(locator.sequence, payload.sequence_id);
                        assert_eq!(target.instance, payload.processor_id);
                        assert_eq!((*width, *height), (320, 100));
                        assert!(opened.replace(*target).is_none());
                        true
                    }
                    AudioPlaybackEvent::NativeParameterObservationsObserved {
                        renderer_id: actual,
                        generation: observed,
                        observation,
                    } => {
                        assert_eq!(*observed, generation);
                        if let Some(prior) = renderer_id {
                            assert_eq!(*actual, prior);
                        }
                        parameters = Some(observation.as_ref().clone());
                        false
                    }
                    AudioPlaybackEvent::NativeDiagnosticsObserved {
                        generation: observed, ..
                    } => {
                        assert_eq!(*observed, generation);
                        false
                    }
                    other => panic!("unexpected history Open event: {other:?}"),
                });
                let opened = opened.context("actual history Opened target")?;
                let renderer_id = renderer_id.context("actual history renderer materialization")?;
                let current_verifier = state
                    .native_audio_renderer
                    .as_ref()
                    .and_then(std::sync::Weak::upgrade)
                    .context("actual current Timeline after Open")?
                    .owner_installation_verifier()
                    .context("current Runtime registration")?;
                let current_claim =
                    current_verifier.current_claim().context("current actual installation")?;
                anyhow::ensure!(
                    current_verifier.validate(&current_claim).is_ok()
                        && opened.installed_generation == current_claim.generation(),
                    "new Opened target belongs to the actual current claim"
                );
                if parameters.as_ref().is_none_or(|actual| actual.target() != opened) {
                    poll_gui_until(&mut state, generation, |event| match event {
                        AudioPlaybackEvent::NativeParameterObservationsObserved {
                            renderer_id: actual,
                            generation: observed,
                            observation,
                        } => {
                            assert_eq!(*actual, renderer_id);
                            assert_eq!(*observed, generation);
                            assert_eq!(observation.target(), opened);
                            parameters = Some(observation.as_ref().clone());
                            true
                        }
                        AudioPlaybackEvent::NativeDiagnosticsObserved {
                            renderer_id: actual,
                            generation: observed,
                            ..
                        } => {
                            assert_eq!(*actual, renderer_id);
                            assert_eq!(*observed, generation);
                            false
                        }
                        other => panic!("unexpected history presentation event: {other:?}"),
                    });
                }
                let parameters = parameters.context("actual history native parameter receipt")?;
                anyhow::ensure!(
                    parameters.target() == opened,
                    "history facts target the current actual Open"
                );
                let fact = parameters
                    .facts()
                    .iter()
                    .rev()
                    .find(|fact| fact.parameter == volume)
                    .context("actual current Volume fact")?;
                anyhow::ensure!(
                    fact.value_bits == Some(expected.to_bits())
                        && fact.display.as_deref() == Some(text),
                    "{stage}: actual native value/text must match restored state: {fact:?}"
                );
                let presentation = state
                    .native_audio_parameter_presentation(&payload, &volume)
                    .context("App current-target history presentation")?;
                anyhow::ensure!(
                    presentation.value_bits == Some(expected.to_bits())
                        && presentation.display.as_deref() == Some(text),
                    "App stores exact native history facts"
                );
                let native_object = (parameters.process_id(), parameters.object_serial());
                let view = owned_editor_view_matching(
                    native_object.0,
                    b"MondrianClapGainEditor_",
                    false,
                    Some(b"Mondrian CLAP Gain Editor"),
                );
                observations.push(serde_json::json!({"stage": stage, "value_bits": fact.value_bits,
                    "display": fact.display, "process_id": native_object.0, "object_serial": native_object.1,
                    "installed_generation": current_claim.generation().get(), "media_generation": generation}));
                stage = if redo {
                    "redo-open-undo-refusal"
                } else {
                    "undo-open-redo-refusal"
                };
                let blocked_action = if redo {
                    "undo_timeline"
                } else {
                    "redo_timeline"
                };
                let author_before_refusal = state.authoring.as_ref().unwrap();
                let document_before_refusal =
                    serde_json::to_value(author_before_refusal.document())?;
                let author_generation_before_refusal = author_before_refusal.author_generation();
                let history_before_refusal = author_before_refusal.history().diagnostics();
                let engine_before_refusal = state.playback_engine.snapshot();
                let surface_before_refusal = state.native_audio_editor_surface_state(&payload);
                let refused = if redo {
                    state.undo_timeline()
                } else {
                    state.redo_timeline()
                };
                anyhow::ensure!(
                    matches!(&refused,
                        Err(mondrian_core::MondrianError::ActionNotExecuted { action, reason })
                            if action == blocked_action && reason ==
                                "native author or editor work must settle before history restore"),
                    "{stage}: an actual open view must reject history before mutation: {refused:?}"
                );
                let author_after_refusal = state.authoring.as_ref().unwrap();
                anyhow::ensure!(
                    serde_json::to_value(author_after_refusal.document())?
                        == document_before_refusal
                        && author_after_refusal.author_generation()
                            == author_generation_before_refusal
                        && author_after_refusal.history().diagnostics() == history_before_refusal
                        && state.playback_engine.snapshot() == engine_before_refusal
                        && state.audio_playback_snapshot().generation == generation
                        && state.audio_playback.native_editor_source_available()
                        && current_verifier.validate(&current_claim).is_ok()
                        && state.native_audio_editor_surface_state(&payload)
                            == surface_before_refusal
                        && unsafe { is_window(view) } != 0,
                    "{stage}: refusal preserves author, Engine, Source/claim and actual open view"
                );
                let unchanged_presentation = state
                    .native_audio_parameter_presentation(&payload, &volume)
                    .context("refused history retains its actual current presentation")?;
                anyhow::ensure!(
                    unchanged_presentation.value_bits == Some(expected.to_bits())
                        && unchanged_presentation.display.as_deref() == Some(text),
                    "{stage}: refusal does not invalidate the current native facts"
                );
                observations.push(
                    serde_json::json!({"stage": stage, "refused_action": blocked_action,
                    "author_unchanged": true, "source_claim_preserved": true,
                    "media_generation": generation}),
                );
                let history_before_close =
                    state.authoring.as_ref().unwrap().history().diagnostics();
                stage = if redo { "redo-close" } else { "undo-close" };
                let mut close = payload.clone();
                close.open = false;
                state
                    .dispatch_action(
                        ProductAction::Audio(AudioProductAction::NativeEditor(close))
                            .into_external_action(),
                    )
                    .with_context(|| format!("original App {stage} action"))?;
                poll_gui_until(&mut state, generation, |event| match event {
                    AudioPlaybackEvent::NativeEditorObserved {
                        renderer_id: actual,
                        generation: observed,
                        intent,
                        outcome,
                    } => {
                        assert_eq!(*actual, renderer_id);
                        assert_eq!(*observed, generation);
                        assert!(matches!(intent, Intent::Close { target } if *target == opened));
                        assert!(matches!(outcome, Outcome::Closed { target } if *target == opened));
                        true
                    }
                    AudioPlaybackEvent::NativeParameterObservationsObserved {
                        renderer_id: actual,
                        generation: observed,
                        observation,
                    } => {
                        assert_eq!(*actual, renderer_id);
                        assert_eq!(*observed, generation);
                        assert_eq!(observation.target(), opened);
                        assert_eq!(
                            (observation.process_id(), observation.object_serial()),
                            native_object
                        );
                        false
                    }
                    AudioPlaybackEvent::NativeDiagnosticsObserved {
                        renderer_id: actual,
                        generation: observed,
                        ..
                    } => {
                        assert_eq!(*actual, renderer_id);
                        assert_eq!(*observed, generation);
                        false
                    }
                    other => panic!("unexpected history Close event: {other:?}"),
                });
                anyhow::ensure!(
                    unsafe { is_window(view) } == 0
                        && state.native_audio_editor_surface_state(&payload) == (false, false),
                    "{stage} destroys the actual view and completes the original App surface"
                );
                anyhow::ensure!(
                    state.authoring.as_ref().unwrap().history().diagnostics()
                        == history_before_close,
                    "view lifecycle creates no extra author transaction"
                );
                anyhow::ensure!(
                    current_verifier.validate(&current_claim).is_ok(),
                    "Close retains its current installation"
                );
                anyhow::ensure!(
                    state.playback_engine.native_audio_control_hold_ticket().is_none()
                        && state.audio_playback.native_control_hold_generation().is_none()
                        && state.audio_playback.native_reprepare_pending_generation().is_none(),
                    "history phase settles all original control holds"
                );
                anyhow::ensure!(
                    state.audio_playback_snapshot().next_start_sample == 0
                        && state.audio_playback_snapshot().in_flight == 0
                        && output.lock().queued == 0,
                    "Idle history lifecycle performs no PCM or device delivery"
                );
                previous_verifier = current_verifier;
                previous_claim = current_claim;
            }
            stage = "history-complete";
            Ok(())
        },
    ));
    let target_error = match history_result {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(format!("{error:#?}")),
        Err(panic) => Some(
            panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|value| (*value).to_owned()))
                .unwrap_or_else(|| "non-string panic in new history stage".to_owned()),
        ),
    };
    // Freeze target facts BEFORE whole-App cleanup can retire the stale Source.
    let before_cleanup = serde_json::json!({
        "case": "app_owned_clap_editor_undo_redo_restores_native_state", "stage": stage,
        "first_failure": target_error, "observations": observations,
        "history": state.authoring.as_ref().map(|author| format!("{:?}", author.history().diagnostics())),
        "author_native_state": state.active_sequence().and_then(|sequence| sequence.audio_program.outputs.first())
            .and_then(|output| output.strip.pre_fader.processors.first())
            .and_then(|instance| instance.opaque_state.as_ref()).map(|value| value.as_slice()),
        "media_generation": state.audio_playback_snapshot().generation,
        "next_start_sample": state.audio_playback_snapshot().next_start_sample,
        "in_flight": state.audio_playback_snapshot().in_flight, "queued_frames": output.lock().queued,
        "source_available": state.audio_playback.native_editor_source_available(),
        "previous_claim": format!("{:?}", previous_verifier.current_claim()),
        "surface": state.native_audio_editor_surface_state(&payload),
        "old_presentation_available": state.native_audio_parameter_presentation(&payload, &volume).is_some(),
    });
    println!("{before_cleanup}");
    let cleanup = state.shutdown_for_endurance(Instant::now() + Duration::from_secs(2));
    assert!(
        cleanup.all_resources_released(),
        "IDLE_HISTORY_TEST_CLEANUP_HOLD: {cleanup:?}; before_cleanup={before_cleanup}"
    );
    assert!(
        target_error.is_none(),
        "CLAP_IDLE_HISTORY_NATIVE_RESTORE_BEFORE_APP_SHUTDOWN: {before_cleanup}"
    );
    assert!(weak.upgrade().is_none());
}

#[test]
#[ignore = "current normal Product and pinned Surge XT 1.3.4 VST3; real Realtime App Idle GUI, no edit/device/PCM"]
fn app_surge_xt_vst3_editor_opens_and_closes_through_same_source() {
    run_surge_xt_vst3_app_editor_control(SurgeVst3AppControl::Editor);
}

#[test]
#[ignore = "current normal Product and pinned Surge XT 1.3.4 VST3; explicit one-ID App query in full 2855 directory, no edit/device/PCM"]
fn app_surge_xt_vst3_explicit_parameter_query_preserves_native_owned_values() {
    run_surge_xt_vst3_app_editor_control(SurgeVst3AppControl::Query);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SurgeVst3AppControl {
    Editor,
    Query,
    QueryAdopt,
}

fn run_surge_xt_vst3_app_editor_control(control: SurgeVst3AppControl) {
    use super::super::super::product_action::{
        AudioNativeEditorPayload, AudioProductAction, ProductAction,
    };
    use mondrian_core::{
        NativeAudioAuthorCallback as Callback, NativeAudioEditorControlIntent as Intent,
        NativeAudioEditorControlOutcome as Outcome, NativeAudioParameterCallbackPhase as Phase,
    };
    use mondrian_media::{
        AudioPcmNativeAuthorHandoffContinuity as Continuity,
        AudioPcmNativeAuthorHandoffStatus as Status,
    };
    const CLASS: &str = "ABCDEF019182FAEB566D624153675854";
    let explicit_query = control != SurgeVst3AppControl::Editor;
    let workflow_deadline = Instant::now() + Duration::from_secs(30);
    let file = |name| {
        let path =
            std::path::PathBuf::from(std::env::var_os(name).expect("explicit native material"));
        assert!(path.is_absolute() && path.is_file(), "{name}");
        path
    };
    let helper = file("MONDRIAN_VST3_TEST_HELPER");
    let clap = Arc::new(
        mondrian_audio::InstalledClapAudioProcessorSpecResolver::new(helper.clone()).unwrap(),
    );
    let vst3 =
        Arc::new(mondrian_audio::InstalledVst3AudioProcessorSpecResolver::new(helper).unwrap());
    let descriptors = vst3.install_plugin(file("MONDRIAN_SURGE_XT_VST3_DLL")).unwrap();
    let descriptor = descriptors
        .iter()
        .find(|entry| entry.class_id == CLASS)
        .expect("real Surge XT class");
    assert_eq!(descriptor.name, "Surge XT");
    assert_eq!(descriptor.vendor.as_deref(), Some("Surge Synth Team"));
    assert_eq!(descriptor.version.as_deref(), Some("1.3.4"));
    // The capture and actual App factory use this same Realtime chunk contract.
    // There are no Notes, musical-reference substitution, PCM or device calls.
    let config = mondrian_media::AudioPlaybackConfig {
        chunk_frames: 4,
        preroll_frames: 4,
        high_watermark_frames: 8,
        max_in_flight: 1,
        ..mondrian_media::AudioPlaybackConfig::product_default()
    };
    let instance = vst3.create_instance(CLASS, mondrian_audio::AudioRenderContract {
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
    }, None).expect("real Realtime catalog capture");
    assert!(matches!(&instance.definition,
        mondrian_timeline::audio::AudioProcessorDefinitionRef::Vst3 {
            class_id, vendor, binary_sha256: Some(_), ..
        } if class_id == CLASS && vendor.as_deref() == Some("Surge Synth Team")));
    assert_eq!(instance.parameters.len(), 2855);
    assert_eq!(
        instance.opaque_state.as_ref().map(|bytes| bytes.len()),
        Some(51928)
    );
    for (id, parameter) in &instance.parameters {
        assert_eq!(&parameter.schema.parameter_id, id);
        assert!(
            parameter.automation.is_none(),
            "captured state owns every initial value"
        );
    }
    let ColdNativeControl { mut state, _directory, output, renderer: weak } =
        prepare_app_native_control_with_instance(clap, vst3, config, instance, |_, _, _| {});
    let sequence = state.active_sequence().unwrap();
    assert!(sequence.audio_program.note_lanes.is_empty());
    let program_output = &sequence.audio_program.outputs[0];
    let original = program_output.strip.pre_fader.processors[0].clone();
    let payload = AudioNativeEditorPayload {
        session_id: state.authoring_session_id().unwrap(),
        sequence_id: sequence.id,
        address: mondrian_timeline::AudioProcessorRackAddress::ChannelStrip {
            owner: mondrian_timeline::AudioChannelStripOwner::ProgramOutput {
                output_id: program_output.id,
            },
            rack: mondrian_timeline::AudioChannelStripRack::PreFader,
        },
        occurrence: None,
        processor_id: original.id,
        definition: original.definition.clone(),
        sequence_time: mondrian_core::DomainTime::new(
            mondrian_core::AuthoringTimeDomain::Sequence(sequence.id),
            TimelineTime::ZERO,
        ),
        open: true,
    };
    let locator = mondrian_core::NativeAudioEditorLocator {
        sequence: sequence.id,
        instance: original.id,
        owner: mondrian_core::NativeAudioEditorOccurrenceOwner::Output(program_output.id),
        insertion: mondrian_core::NativeAudioEditorInsertion::PreFader,
    };
    let engine = state.playback_engine.snapshot();
    let generation = state.audio_playback_snapshot().generation;
    let mut verifier = weak.upgrade().unwrap().owner_installation_verifier().unwrap();
    let mut claim = verifier.current_claim().unwrap();
    let mut renderer_id = None;
    let mut opened = None;
    let mut current_target = None;
    let mut closed = false;
    let mut early: Option<mondrian_core::NativeAudioAuthorProposal> = None;
    let mut reoffered = false;
    let mut pending: Option<mondrian_core::NativeAudioAuthorProposal> = None;
    let mut installed_awaiting = None;
    let mut proposals = 0_usize;
    let mut installations = 0_usize;
    let mut acknowledgements = 0_usize;
    let mut native_object = None;
    let mut unexpected_value_edit = false;
    let mut phase = 0;
    let mut query_requested = false;
    let mut query_completed = false;
    let mut query_ticket = None;
    // Deterministic canonical author-key order, not native catalog ordinal.
    let query_selection = original
        .parameters
        .iter()
        .enumerate()
        .skip(2048)
        .find(|(_, (_, parameter))| {
            !parameter.ui.hidden
                && parameter.schema.numeric.is_some()
                && parameter.automation.is_none()
        })
        .map(|(index, (id, _))| (index, id.clone()));
    let (query_canonical_index, query_parameter) = query_selection
        .clone()
        .unwrap_or_else(|| (0, original.parameters.keys().next().unwrap().clone()));
    let mut query_caption = None;
    let mut query_finite = false;
    let mut query_history = None;
    let mut query_observation = None;
    let mut adopted_author_value = None;
    // Always consume original Audio cleanup even if a new target assertion fails.
    // The expected parameter model remains the independent initial snapshot.
    let target_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert!(
            !explicit_query || query_selection.is_some(),
            "visible numeric native-owned row after canonical index 2048"
        );
        for current_phase in 0..3 {
            phase = current_phase;
            if phase != 0 {
                let mut request = payload.clone();
                request.open = phase == 1;
                state
                    .dispatch_action(
                        ProductAction::Audio(AudioProductAction::NativeEditor(request))
                            .into_external_action(),
                    )
                    .expect("original App Open/Close action");
                assert_eq!(
                    state.native_audio_editor_surface_state(&payload),
                    if phase == 1 {
                        (false, true)
                    } else {
                        (true, true)
                    }
                );
            }
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                let poll = state
                    .audio_playback
                    .poll(AudioPlaybackMode::Idle, audio_anchor(0))
                    .expect("original Media Idle control");
                assert_eq!(poll.snapshot.generation, generation);
                assert_eq!(
                    (poll.snapshot.in_flight, poll.snapshot.next_start_sample),
                    (0, 0)
                );
                assert_eq!(poll.snapshot.output.unwrap().buffered_frames, 0);
                for event in poll.events {
                    let mut published = None;
                    match &event {
                        AudioPlaybackEvent::NativeOwnerBindingsObserved {
                            renderer_id: actual,
                            generation: observed,
                            status,
                        } => {
                            assert_eq!(phase, 0);
                            assert!(renderer_id.is_none());
                            assert_eq!(*observed, generation);
                            assert_eq!(
                                *status,
                                mondrian_media::AudioPcmNativeOwnerBindingStatus::Ready
                            );
                            renderer_id = Some(*actual);
                        }
                        AudioPlaybackEvent::NativeEditorObserved {
                            renderer_id: actual,
                            generation: observed,
                            intent,
                            outcome,
                        } => {
                            assert_eq!(Some(*actual), renderer_id);
                            assert_eq!(*observed, generation);
                            match (phase, intent, outcome) {
                                (
                                    1,
                                    Intent::ResolveAndOpen { locator: requested, .. },
                                    Outcome::Opened { target, width, height },
                                ) => {
                                    assert!(opened.is_none());
                                    assert_eq!(*requested, locator);
                                    assert!(locator.matches_target(*target));
                                    assert_eq!(target.installed_generation, claim.generation());
                                    assert!(*width > 0 && *height > 0);
                                    if let Some(early) = &early {
                                        assert_eq!(early.target(), *target);
                                    }
                                    opened = Some(*target);
                                    current_target = Some(*target);
                                }
                                (
                                    2,
                                    Intent::Close { target: requested },
                                    Outcome::Closed { target },
                                ) => {
                                    assert!(!closed);
                                    assert_eq!(Some(*requested), opened);
                                    assert_eq!(Some(*target), current_target);
                                    closed = true;
                                }
                                _ => panic!("actual queued GUI operation must complete: {event:?}"),
                            }
                        }
                        AudioPlaybackEvent::NativeAuthorProposalObserved {
                            renderer_id: actual,
                            generation: observed,
                            proposal,
                        } => {
                            assert_eq!(Some(*actual), renderer_id);
                            assert_eq!(*observed, generation);
                            assert_eq!(proposal.sequence(), payload.sequence_id);
                            assert!(locator.matches_target(proposal.target()));
                            // A real edit is still consumed through the original transaction.
                            // It makes the separate no-edit/None oracle fail after cleanup.
                            unexpected_value_edit |= proposal.callbacks().iter().any(|callback| {
                                matches!(
                                    callback,
                                    Callback::Parameter { phase: Phase::Value, .. }
                                        | Callback::ParameterAtExecution {
                                            phase: Phase::Value,
                                            ..
                                        }
                                )
                            });
                            if opened.is_none() {
                                assert!(phase <= 1 && pending.is_none());
                                assert_eq!(
                                    proposal.target().installed_generation,
                                    claim.generation()
                                );
                                if let Some(prior) = &early {
                                    assert!(prior.same_retained_proposal(proposal));
                                } else {
                                    early = Some(proposal.as_ref().clone());
                                }
                            } else if let Some(prior) = &pending {
                                assert!(prior.same_retained_proposal(proposal));
                            } else {
                                if !reoffered && let Some(early) = &early {
                                    assert!(early.same_retained_proposal(proposal));
                                    reoffered = true;
                                }
                                assert_eq!(Some(proposal.target()), current_target);
                                assert!(installed_awaiting.is_none());
                                proposals += 1;
                                pending = Some(proposal.as_ref().clone());
                            }
                        }
                        AudioPlaybackEvent::NativeAuthorHandoffObserved {
                            renderer_id: actual,
                            generation: observed,
                            proposal,
                            status,
                        } => {
                            assert_eq!(Some(*actual), renderer_id);
                            assert_eq!(*observed, generation);
                            assert!(pending
                                .as_ref()
                                .expect("actual proposal precedes handoff")
                                .same_retained_proposal(proposal));
                            let (target, continuity, ack) = match status {
                                Status::Acknowledged { target, continuity } => {
                                    (target, continuity, true)
                                }
                                Status::InstalledAwaitingAcknowledgement { target, continuity } => {
                                    (target, continuity, false)
                                }
                                Status::Refused(reason) => {
                                    panic!("original initialization handoff refused: {reason:?}")
                                }
                            };
                            assert_eq!(*continuity, Continuity::Preserved);
                            let previous = current_target.unwrap();
                            assert_eq!(
                                (
                                    target.root_session,
                                    target.local_session,
                                    target.instance,
                                    target.continuity_epoch
                                ),
                                (
                                    previous.root_session,
                                    previous.local_session,
                                    previous.instance,
                                    previous.continuity_epoch
                                )
                            );
                            if let Some(installed) = installed_awaiting {
                                assert_eq!(**target, installed);
                            } else {
                                published = Some((**target, proposal.state().cloned()));
                                current_target = Some(**target);
                                installations += 1;
                            }
                            if ack {
                                pending = None;
                                installed_awaiting = None;
                                acknowledgements += 1;
                            } else {
                                installed_awaiting = Some(**target);
                            }
                        }
                        AudioPlaybackEvent::NativeEditorAttachmentObserved {
                            renderer_id: actual,
                            generation: observed,
                            observation,
                        } => {
                            assert_eq!(Some(*actual), renderer_id);
                            assert_eq!(*observed, generation);
                            assert_eq!(Some(observation.previous), opened);
                            assert!(
                                locator.matches_target(observation.current) && observation.retained
                            );
                            let actual_object = (observation.process_id, observation.object_serial);
                            if let Some(prior) = native_object {
                                assert_eq!(actual_object, prior);
                            } else {
                                native_object = Some(actual_object);
                            }
                        }
                        AudioPlaybackEvent::NativeParameterObservationsObserved {
                            renderer_id: actual,
                            generation: observed,
                            observation,
                        } => {
                            assert_eq!(Some(*actual), renderer_id);
                            assert_eq!(*observed, generation);
                            assert!(locator.matches_target(observation.target()));
                            // Optional facts never provide a required PID or bootstrap this gate.
                        }
                        AudioPlaybackEvent::NativeParameterQueryCompleted {
                            ticket,
                            renderer_id: actual,
                            generation: observed,
                            query,
                            completion,
                        } => {
                            assert!(explicit_query && query_requested && !query_completed);
                            assert_eq!(phase, 1);
                            assert_eq!(Some(*ticket), query_ticket);
                            assert_eq!(Some(*actual), renderer_id);
                            assert_eq!(*observed, generation);
                            assert_eq!(Some(query.target), current_target);
                            assert_eq!(query.parameter, query_parameter);
                            let mondrian_media::AudioNativeParameterQueryCompletion::Completed(
                                mondrian_core::NativeAudioParameterQueryOutcome::Observed(
                                    observation,
                                ),
                            ) = completion
                            else {
                                panic!("explicit query refused or cancelled: {completion:?}")
                            };
                            assert_eq!(observation.target(), query.target);
                            assert_eq!(observation.facts().len(), 1);
                            let fact = &observation.facts()[0];
                            assert_eq!(fact.parameter, query_parameter);
                            assert!(matches!(
                                fact.origin,
                                mondrian_core::NativeAudioParameterObservationOrigin::MainQuery { .. }
                            ));
                            assert!(fact
                                .value_bits
                                .is_some_and(|bits| f64::from_bits(bits).is_finite()));
                            assert!(fact
                                .author_value_bits
                                .is_some_and(|bits| f64::from_bits(bits).is_finite()));
                            assert!(fact.name.as_ref().is_some_and(|name| !name.is_empty()));
                            if let Some(object) = native_object {
                                assert_eq!(
                                    (observation.process_id(), observation.object_serial()),
                                    object
                                );
                            }
                            if control == SurgeVst3AppControl::QueryAdopt {
                                assert!(query_observation.is_none());
                                query_observation = Some((**observation).clone());
                            }
                            query_caption = fact.name.clone();
                            query_finite = true;
                            query_completed = true;
                        }
                        AudioPlaybackEvent::NativeDiagnosticsObserved {
                            renderer_id: actual,
                            generation: observed,
                            ..
                        } => {
                            assert_eq!(Some(*actual), renderer_id);
                            assert_eq!(*observed, generation);
                        }
                        other => panic!("unexpected refusal, hold, replacement or PCM: {other:?}"),
                    }
                    state
                        .handle_audio_playback_event(event, Instant::now())
                        .expect("original canonical App consumer");
                    if let Some((target, accepted_state)) = published {
                        let next = weak.upgrade().unwrap().owner_installation_verifier().unwrap();
                        assert!(verifier.same_source(&next) && !verifier.same_registration(&next));
                        assert_eq!(
                            verifier.current_claim().unwrap_err(),
                            mondrian_core::AudioOwnerInstallationError::Superseded
                        );
                        let next_claim = next.current_claim().unwrap();
                        assert!(next.validate(&next_claim).is_ok());
                        assert_eq!(target.installed_generation, next_claim.generation());
                        if let Some(accepted) = accepted_state {
                            assert_eq!(
                                state.active_sequence().unwrap().audio_program.outputs[0]
                                    .strip
                                    .pre_fader
                                    .processors[0]
                                    .opaque_state
                                    .as_ref(),
                                Some(&accepted)
                            );
                        }
                        verifier = next;
                        claim = next_claim;
                    }
                }
                if pending.is_none() {
                    assert!(verifier.validate(&claim).is_ok());
                }
                assert!(state
                    .native_audio_renderer
                    .as_ref()
                    .is_some_and(|actual| std::sync::Weak::ptr_eq(actual, &weak)));
                assert_eq!(state.playback_engine.snapshot(), engine);
                assert!(state.playback_engine.native_audio_control_hold_ticket().is_none());
                assert_eq!(
                    state.audio_playback.native_control_hold_generation(),
                    pending.as_ref().map(|_| generation)
                );
                assert!(state.audio_playback.native_reprepare_pending_generation().is_none());
                assert_eq!(output.lock().queued, 0);
                let settled = pending.is_none()
                    && installed_awaiting.is_none()
                    && proposals == acknowledgements
                    && installations == acknowledgements
                    && state.authoring.as_ref().unwrap().history().diagnostics().pending_entries
                        == 0;
                let mut complete = match phase {
                    0 => renderer_id.is_some(),
                    1 => {
                        opened.is_some()
                            && (early.is_none() || reoffered)
                            && settled
                            && state.native_audio_editor_surface_state(&payload) == (true, false)
                    }
                    2 => {
                        closed
                            && settled
                            && state.native_audio_editor_surface_state(&payload) == (false, false)
                    }
                    _ => unreachable!(),
                };
                if phase == 1 && complete && explicit_query && !query_requested {
                    assert!(state
                        .native_audio_parameter_presentation(&payload, &query_parameter)
                        .is_none());
                    let request = crate::app_ui::test_utils::native_parameter_query_payload(
                        &state,
                        payload.address,
                        original.id,
                        &query_parameter,
                    )
                    .expect("actual visible Idle Opened/noFact None row has a manual query action");
                    let scalar = (*request.target).clone();
                    query_history = Some(state.authoring.as_ref().unwrap().history().diagnostics());
                    state
                        .dispatch_action(
                            ProductAction::Audio(AudioProductAction::QueryNativeParameter(request))
                                .into_external_action(),
                        )
                        .expect("manual explicit App query action");
                    query_ticket = std::num::NonZeroU64::new(state.native_parameter_query_serial);
                    assert!(query_ticket.is_some());
                    assert!(
                        state.displayed_native_parameter_query(scalar).is_none(),
                        "at most one App query is pending"
                    );
                    query_requested = true;
                    complete = false;
                }
                if phase == 1 && explicit_query {
                    complete &= query_completed;
                    if complete {
                        let fact = state.native_audio_parameter_presentation(&payload, &query_parameter).expect("original single Fact cache receives only the exact query completion");
                        assert_eq!(fact.name, query_caption);
                        assert_eq!(
                            Some(state.authoring.as_ref().unwrap().history().diagnostics()),
                            query_history
                        );
                    }
                }
                if complete {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "original GUI/initialization completion did not settle"
                );
                std::thread::yield_now();
            }
        }
        assert!(weak.upgrade().unwrap().native_author_snapshot_current());
        let after = &state.active_sequence().unwrap().audio_program.outputs[0]
            .strip
            .pre_fader
            .processors[0];
        assert_eq!(after.id, original.id);
        assert_eq!(after.definition, original.definition);
        assert_eq!(
            after.parameters, original.parameters,
            "all 2855 schema/UI/None values remain the independent original model"
        );
        assert!(
            !unexpected_value_edit,
            "actual Value editing is outside this no-edit GUI gate"
        );
        assert_eq!(
            state.status_hint().map(|(message, _)| message.id),
            Some("native-editor-closed")
        );
        if control == SurgeVst3AppControl::QueryAdopt {
            assert!(query_requested && query_completed && closed);
            assert!(pending.is_none() && installed_awaiting.is_none());
            assert_eq!(
                (proposals, installations),
                (acknowledgements, acknowledgements)
            );
            let observed = query_observation.as_ref().expect("actual query completion witness");
            assert_eq!(Some(observed.target()), current_target);
            adopted_author_value =
                Some(surge_parameter_adoption::adopt_closed_parameter_and_reload(
                    &mut state,
                    &payload,
                    &original,
                    &query_parameter,
                    observed,
                    workflow_deadline,
                ));
        }
    }));
    let failure = target_result.err().map(|panic| {
        panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|value| (*value).to_owned()))
            .unwrap_or_else(|| "non-string target assertion".to_owned())
    });
    let evidence = state.shutdown_audio_playback_and_wait();
    assert_eq!(evidence.render_workers_started, 1);
    assert_eq!(evidence.render_workers_terminated, 1);
    assert_eq!(evidence.render_retirement_workers_started, 1);
    assert_eq!(evidence.render_retirement_workers_terminated, 1);
    assert_eq!(
        (
            evidence.foreign_owner_panics,
            evidence.foreign_owner_abandonments
        ),
        (0, 0)
    );
    assert!(
        evidence.all_workers_terminated(),
        "{evidence:?}; target_failure={failure:?}"
    );
    assert!(evidence.shutdown_resource_facts_complete_at_deadline);
    assert!(!evidence.shutdown_owner_lifetime_unresolved_at_deadline);
    assert!(matches!(
        verifier.current_claim(),
        Err(mondrian_core::AudioOwnerInstallationError::NotInstalled)
    ));
    assert!(weak.upgrade().is_none());
    assert_eq!(output.lock().queued, 0);
    println!(
        "{}",
        serde_json::json!({
            "case":match control { SurgeVst3AppControl::Editor => "app-surge-vst3-editor", SurgeVst3AppControl::Query => "app-surge-vst3-query", SurgeVst3AppControl::QueryAdopt => "app-surge-vst3-query-adopt-project" }, "plugin_version":"1.3.4", "parameters":2855,
            "mode":"Realtime", "rate":config.sample_rate, "block":config.chunk_frames,
            "phase":phase, "initialization_proposals":proposals, "installations":installations,
            "exact_acknowledgements":acknowledgements, "opened":opened.is_some(), "closed":closed,
            "attachment_object_observed":native_object.is_some(), "unexpected_value_edit":unexpected_value_edit,
            "explicit_query_requested":query_requested, "explicit_query_completed":query_completed,
            "query_canonical_index":explicit_query.then_some(query_canonical_index), "query_parameter":explicit_query.then_some(&query_parameter), "query_caption":query_caption, "query_finite":query_finite,
            "host_value_adopted_and_reloaded":adopted_author_value.is_some(), "adopted_author_value_bits":adopted_author_value,
            "native_overlay_execution_qualified":false, "dsp_history_recovery_qualified":false,
            "target_failure":failure, "pcm_qualified":false, "device_qualified":false,
            "large_census_presentation_qualified":false, "realtime_qualified":false,
            "graceful_100ms_qualified":false
        })
    );
    assert!(
        failure.is_none(),
        "SURGE_VST3_APP_GUI_TARGET_FAILED: {failure:?}"
    );
    drop(state);
}

#[test]
#[ignore = "current normal Product and retained Surge XT Effects CLAP; real Idle App GUI lifecycle, no PCM/device"]
fn app_surge_xt_effects_clap_editor_opens_and_closes_through_same_source() {
    use super::super::super::product_action::{
        AudioNativeEditorPayload, AudioProductAction, ProductAction,
    };
    use mondrian_core::{
        NativeAudioEditorControlIntent as Intent, NativeAudioEditorControlOutcome as Outcome,
    };
    use mondrian_media::{
        AudioPcmNativeAuthorHandoffContinuity as Continuity,
        AudioPcmNativeAuthorHandoffStatus as Status,
    };

    const PLUGIN: &str = "org.surge-synth-team.surge-xt-fx";
    let helper = std::path::PathBuf::from(
        std::env::var_os("MONDRIAN_CLAP_TEST_HELPER").expect("current normal Product"),
    );
    let library = std::path::PathBuf::from(
        std::env::var_os("MONDRIAN_SURGE_XT_EFFECTS_CLAP_TEST_PLUGIN")
            .expect("retained original Surge XT Effects CLAP material"),
    );
    assert!(helper.is_absolute() && helper.is_file());
    assert!(library.is_absolute() && library.is_file());
    let clap = Arc::new(
        mondrian_audio::InstalledClapAudioProcessorSpecResolver::new(helper.clone()).unwrap(),
    );
    let vst3 =
        Arc::new(mondrian_audio::InstalledVst3AudioProcessorSpecResolver::new(helper).unwrap());
    // These original standalone catalog probes do not establish Playback custody.
    let descriptors = clap.install_library(library).expect("actual retained CLAP library");
    let descriptor = descriptors
        .iter()
        .find(|entry| entry.plugin_id == PLUGIN)
        .expect("actual Surge Effects identity");
    let descriptor_version = descriptor.version.clone();
    let config = mondrian_media::AudioPlaybackConfig {
        chunk_frames: 4,
        preroll_frames: 4,
        high_watermark_frames: 8,
        max_in_flight: 1,
        ..mondrian_media::AudioPlaybackConfig::product_default()
    };
    let instance = clap.create_instance(
        PLUGIN,
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
    ).expect("actual catalog schemas, lanes and captured opaque state");
    assert!(matches!(&instance.definition,
        mondrian_timeline::audio::AudioProcessorDefinitionRef::Clap {
            plugin_id, binary_sha256: Some(_), ..
        } if plugin_id == PLUGIN));
    assert!(
        !instance.parameters.is_empty(),
        "real presentation lanes, never fabricated PID facts"
    );
    assert!(!instance.opaque_state.as_ref().expect("actual captured state").is_empty());
    let ColdNativeControl { mut state, _directory, output, renderer: weak } =
        prepare_app_native_control_with_instance(clap, vst3, config, instance, |_, _, _| {});
    let sequence = state.active_sequence().unwrap();
    assert!(sequence.audio_program.note_lanes.is_empty());
    let native_output = &sequence.audio_program.outputs[0];
    let original = native_output.strip.pre_fader.processors[0].clone();
    let payload = AudioNativeEditorPayload {
        session_id: state.authoring_session_id().unwrap(),
        sequence_id: sequence.id,
        address: mondrian_timeline::AudioProcessorRackAddress::ChannelStrip {
            owner: mondrian_timeline::AudioChannelStripOwner::ProgramOutput {
                output_id: native_output.id,
            },
            rack: mondrian_timeline::AudioChannelStripRack::PreFader,
        },
        occurrence: None,
        processor_id: original.id,
        definition: original.definition.clone(),
        sequence_time: mondrian_core::DomainTime::new(
            mondrian_core::AuthoringTimeDomain::Sequence(sequence.id),
            TimelineTime::ZERO,
        ),
        open: true,
    };
    let requested_locator = mondrian_core::NativeAudioEditorLocator {
        sequence: payload.sequence_id,
        instance: original.id,
        owner: mondrian_core::NativeAudioEditorOccurrenceOwner::Output(native_output.id),
        insertion: mondrian_core::NativeAudioEditorInsertion::PreFader,
    };
    let engine_before = state.playback_engine.snapshot();
    let mut verifier = weak.upgrade().unwrap().owner_installation_verifier().unwrap();
    let mut claim = verifier.current_claim().unwrap();
    assert!(verifier.validate(&claim).is_ok());
    let generation = state.audio_playback_snapshot().generation;
    let mut renderer_id = None;
    let mut opened = None;
    let mut current_target = None;
    let mut closed = false;
    let mut native_object = None;
    let mut parameters: Option<mondrian_core::NativeAudioParameterObservation> = None;
    // Test receipts only: the production Runtime retains the actual Source
    // batch and naturally reoffers the same Arc after Opened. This local
    // observation neither seeds App target nor resubmits/acknowledges anything.
    let mut before_open: Option<mondrian_core::NativeAudioAuthorProposal> = None;
    let mut pre_open_deliveries = 0_usize;
    let mut pending_open_deferred = 0_usize;
    let mut redelivered_after_open = false;
    let mut pending: Option<mondrian_core::NativeAudioAuthorProposal> = None;
    let mut installed_awaiting = None;
    let mut awaiting_ack_observations = 0_usize;
    let mut installations = 0_usize;
    let mut proposals = 0_usize;
    let mut acknowledgements = 0_usize;
    let mut attachment = None;
    let mut windows = Vec::new();

    // One ordinary Idle poll loop per lifecycle operation, with the same 2s
    // bound and observe-before-handle order as poll_gui_until. Completion is
    // checked after the WHOLE batch: Opened may precede an initialization
    // callback in that same batch. No callback is manually acknowledged.
    for phase in 0..3 {
        if phase != 0 {
            let mut request = payload.clone();
            request.open = phase == 1;
            if phase == 1 {
                parameters = None; // Require actual post-Open presentation.
            }
            state
                .dispatch_action(
                    ProductAction::Audio(AudioProductAction::NativeEditor(request))
                        .into_external_action(),
                )
                .expect("original App editor action");
            assert_eq!(
                state.native_audio_editor_surface_state(&payload),
                if phase == 1 {
                    (false, true)
                } else {
                    (true, true)
                },
                "a refused submission is not a queued operation"
            );
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let poll = state
                .audio_playback
                .poll(AudioPlaybackMode::Idle, audio_anchor(0))
                .expect("original serial Media Idle control");
            assert_eq!(poll.snapshot.generation, generation);
            assert_eq!(poll.snapshot.in_flight, 0);
            assert_eq!(poll.snapshot.next_start_sample, 0);
            assert_eq!(
                poll.snapshot.output.expect("software output").buffered_frames,
                0
            );
            for event in poll.events {
                let mut published = None;
                let mut deferred_before = None;
                let mut ack_retry_before = None;
                match &event {
                    AudioPlaybackEvent::NativeOwnerBindingsObserved {
                        renderer_id: actual,
                        generation: observed,
                        status,
                    } => {
                        assert_eq!(phase, 0, "no replacement Playback owner");
                        assert!(renderer_id.is_none());
                        assert_eq!(*observed, generation);
                        assert_eq!(
                            *status,
                            mondrian_media::AudioPcmNativeOwnerBindingStatus::Ready
                        );
                        renderer_id = Some(*actual);
                    }
                    AudioPlaybackEvent::NativeEditorObserved {
                        renderer_id: actual,
                        generation: observed,
                        intent,
                        outcome,
                    } => {
                        assert_eq!(Some(*actual), renderer_id);
                        assert_eq!(*observed, generation);
                        match (phase, intent, outcome) {
                            (
                                1,
                                Intent::ResolveAndOpen { locator, .. },
                                Outcome::Opened { target, width, height },
                            ) => {
                                assert!(opened.is_none());
                                assert_eq!(*locator, requested_locator);
                                assert!(locator.matches_target(*target));
                                assert_eq!(locator.sequence, payload.sequence_id);
                                if let Some(early) = &before_open {
                                    assert_eq!(
                                        early.target(),
                                        *target,
                                        "actual Opened authenticates the deferred target"
                                    );
                                    assert_eq!(early.sequence(), locator.sequence);
                                }
                                assert_eq!(target.instance, original.id);
                                assert_eq!(target.installed_generation, claim.generation());
                                assert!(*width > 0 && *height > 0, "actual native GUI extent");
                                opened = Some(*target);
                                current_target = Some(*target);
                            }
                            (
                                2,
                                Intent::Close { target: requested },
                                Outcome::Closed { target },
                            ) => {
                                assert!(!closed);
                                assert_eq!(
                                    Some(*requested),
                                    opened,
                                    "Close uses the original actual view attachment"
                                );
                                assert_eq!(Some(*target), current_target);
                                closed = true;
                            }
                            _ => panic!(
                                "real queued operation must complete, never Refused: {event:?}"
                            ),
                        }
                    }
                    AudioPlaybackEvent::NativeParameterObservationsObserved {
                        renderer_id: actual,
                        generation: observed,
                        observation,
                    } => {
                        assert_eq!(Some(*actual), renderer_id);
                        assert_eq!(*observed, generation);
                        assert_eq!(observation.target().instance, original.id);
                        assert!(!observation.facts().is_empty());
                        assert!(
                            observation
                                .facts()
                                .iter()
                                .any(|fact| original.parameters.contains_key(&fact.parameter)),
                            "actual admitted parameter facts authenticate this object"
                        );
                        let object = (observation.process_id(), observation.object_serial());
                        if let Some(previous) = native_object {
                            assert_eq!(object, previous, "same actual native incarnation");
                        } else {
                            native_object = Some(object);
                        }
                        parameters = Some(observation.as_ref().clone());
                    }
                    AudioPlaybackEvent::NativeAuthorProposalObserved {
                        renderer_id: actual,
                        generation: observed,
                        proposal,
                    } => {
                        assert_eq!(Some(*actual), renderer_id);
                        assert_eq!(*observed, generation);
                        assert_eq!(proposal.sequence(), payload.sequence_id);
                        if opened.is_none() {
                            assert!(phase <= 1);
                            assert!(current_target.is_none());
                            assert!(pending.is_none());
                            assert_eq!(acknowledgements, 0);
                            assert!(requested_locator.matches_target(proposal.target()));
                            assert_eq!(proposal.target().installed_generation, claim.generation());
                            assert!(verifier.validate(&claim).is_ok());
                            assert_eq!(
                                state.native_audio_editor_surface_state(&payload),
                                (false, phase == 1),
                                "no attachment; only phase1 has actual queued ResolveAndOpen"
                            );
                            if let Some(facts) = &parameters {
                                assert_eq!(facts.target(), proposal.target());
                                assert_eq!(
                                    Some((facts.process_id(), facts.object_serial())),
                                    native_object
                                );
                            }
                            if let Some(previous) = &before_open {
                                assert!(previous.same_retained_proposal(proposal));
                            } else {
                                before_open = Some(proposal.as_ref().clone());
                            }
                            pre_open_deliveries = pre_open_deliveries.checked_add(1).unwrap();
                            if phase == 1 {
                                pending_open_deferred =
                                    pending_open_deferred.checked_add(1).unwrap();
                            }
                            let author = state.authoring.as_ref().unwrap();
                            deferred_before = Some((
                                author.author_generation(),
                                author.history().diagnostics(),
                                author.is_dirty(),
                                serde_json::to_value(
                                    &state.active_sequence().unwrap().audio_program.outputs[0]
                                        .strip
                                        .pre_fader
                                        .processors[0],
                                )
                                .unwrap(),
                            ));
                        } else {
                            if !redelivered_after_open && let Some(early) = &before_open {
                                assert!(
                                    early.same_retained_proposal(proposal),
                                    "the Runtime naturally reoffers its original held Arc"
                                );
                                redelivered_after_open = true;
                            }
                            if let Some(previous) = &pending {
                                assert!(previous.same_retained_proposal(proposal));
                                if let Some(installed) = installed_awaiting {
                                    assert_eq!(current_target, Some(installed));
                                    let author = state.authoring.as_ref().unwrap();
                                    ack_retry_before = Some((
                                        author.author_generation(),
                                        author.history().diagnostics(),
                                        serde_json::to_value(
                                            &state.active_sequence().unwrap().audio_program.outputs
                                                [0]
                                            .strip
                                            .pre_fader
                                            .processors[0],
                                        )
                                        .unwrap(),
                                    ));
                                }
                            } else {
                                assert_eq!(Some(proposal.target()), current_target);
                                assert!(installed_awaiting.is_none());
                                proposals = proposals.checked_add(1).unwrap();
                                pending = Some(proposal.as_ref().clone());
                            }
                        }
                    }
                    AudioPlaybackEvent::NativeAuthorHandoffObserved {
                        renderer_id: actual,
                        generation: observed,
                        proposal,
                        status,
                    } => {
                        assert_eq!(Some(*actual), renderer_id);
                        assert_eq!(*observed, generation);
                        assert!(pending
                            .as_ref()
                            .expect("actual proposal before handoff")
                            .same_retained_proposal(proposal));
                        let (target, continuity, acknowledged) = match status {
                            Status::Acknowledged { target, continuity } => {
                                (target, continuity, true)
                            }
                            Status::InstalledAwaitingAcknowledgement { target, continuity } => {
                                awaiting_ack_observations =
                                    awaiting_ack_observations.checked_add(1).unwrap();
                                (target, continuity, false)
                            }
                            Status::Refused(reason) => {
                                panic!("initialization prefix must settle, never hide refusal: {reason:?}")
                            }
                        };
                        assert_eq!(*continuity, Continuity::Preserved);
                        let previous = current_target.unwrap();
                        assert_eq!(target.root_session, previous.root_session);
                        assert_eq!(target.local_session, previous.local_session);
                        assert_eq!(target.instance, previous.instance);
                        assert_eq!(target.continuity_epoch, previous.continuity_epoch);
                        if let Some(installed) = installed_awaiting {
                            assert_eq!(
                                **target, installed,
                                "Ack retry cannot install a second target/transaction"
                            );
                            assert_eq!(current_target, Some(installed));
                            assert_eq!(target.installed_generation, claim.generation());
                            assert!(verifier.validate(&claim).is_ok());
                        } else {
                            current_target = Some(**target);
                            published = Some((**target, proposal.state().cloned()));
                            installations = installations.checked_add(1).unwrap();
                        }
                        if acknowledged {
                            installed_awaiting = None;
                            pending = None;
                            acknowledgements = acknowledgements.checked_add(1).unwrap();
                        } else {
                            installed_awaiting = Some(**target);
                        }
                    }
                    AudioPlaybackEvent::NativeEditorAttachmentObserved {
                        renderer_id: actual,
                        generation: observed,
                        observation,
                    } => {
                        assert_eq!(Some(*actual), renderer_id);
                        assert_eq!(*observed, generation);
                        assert_eq!(Some(observation.previous), opened);
                        assert!(observation.retained);
                        let object = (observation.process_id, observation.object_serial);
                        assert_eq!(Some(object), native_object);
                        attachment = Some(observation.current);
                    }
                    AudioPlaybackEvent::NativeDiagnosticsObserved {
                        renderer_id: actual,
                        generation: observed,
                        ..
                    } => {
                        assert_eq!(Some(*actual), renderer_id);
                        assert_eq!(*observed, generation);
                    }
                    // In particular App may log PollRefused and return Ok. It
                    // cannot count as successful initialization in this gate.
                    other => panic!("unexpected refusal, hold, reprepare or PCM: {other:?}"),
                }
                state
                    .handle_audio_playback_event(event, Instant::now())
                    .expect("original canonical App proposal/install/Ack consumer");
                if let Some((author_generation, history, dirty, instance)) = deferred_before {
                    let author = state.authoring.as_ref().unwrap();
                    assert_eq!(author.author_generation(), author_generation);
                    assert_eq!(author.history().diagnostics(), history);
                    assert_eq!(author.is_dirty(), dirty);
                    assert_eq!(
                        serde_json::to_value(
                            &state.active_sequence().unwrap().audio_program.outputs[0]
                                .strip
                                .pre_fader
                                .processors[0],
                        )
                        .unwrap(),
                        instance
                    );
                    assert_eq!(
                        state.native_audio_editor_surface_state(&payload),
                        (false, phase == 1)
                    );
                    assert!(current_target.is_none());
                    assert!(pending.is_none());
                    assert_eq!(acknowledgements, 0);
                    assert!(verifier.validate(&claim).is_ok());
                    let current = weak.upgrade().unwrap().owner_installation_verifier().unwrap();
                    assert!(verifier.same_source(&current));
                    assert!(verifier.same_registration(&current));
                    assert_eq!(
                        current.current_claim().unwrap().generation(),
                        claim.generation()
                    );
                }
                if let Some((author_generation, history, instance)) = ack_retry_before {
                    let author = state.authoring.as_ref().unwrap();
                    assert_eq!(author.author_generation(), author_generation);
                    assert_eq!(author.history().diagnostics(), history);
                    assert_eq!(
                        serde_json::to_value(
                            &state.active_sequence().unwrap().audio_program.outputs[0]
                                .strip
                                .pre_fader
                                .processors[0],
                        )
                        .unwrap(),
                        instance,
                        "redelivery queues only the original exact Ack"
                    );
                    let current = weak.upgrade().unwrap().owner_installation_verifier().unwrap();
                    assert!(verifier.same_source(&current));
                    assert!(verifier.same_registration(&current));
                    assert_eq!(
                        current.current_claim().unwrap().generation(),
                        claim.generation()
                    );
                }
                if let Some((target, accepted_state)) = published {
                    let next = weak
                        .upgrade()
                        .expect("same original Timeline")
                        .owner_installation_verifier()
                        .unwrap();
                    assert!(verifier.same_source(&next));
                    assert!(!verifier.same_registration(&next));
                    assert_eq!(
                        verifier.current_claim().unwrap_err(),
                        mondrian_core::AudioOwnerInstallationError::Superseded
                    );
                    let next_claim = next.current_claim().unwrap();
                    assert!(next.validate(&next_claim).is_ok());
                    assert_eq!(target.installed_generation, next_claim.generation());
                    let after = &state.active_sequence().unwrap().audio_program.outputs[0]
                        .strip
                        .pre_fader
                        .processors[0];
                    assert_eq!(after.id, original.id);
                    assert_eq!(after.definition, original.definition);
                    assert_eq!(after.parameters.len(), original.parameters.len());
                    for (id, parameter) in &original.parameters {
                        assert_eq!(after.parameters[id].schema, parameter.schema);
                        assert_eq!(after.parameters[id].ui, parameter.ui);
                    }
                    if let Some(accepted_state) = accepted_state {
                        assert_eq!(after.opaque_state.as_ref(), Some(&accepted_state));
                    }
                    verifier = next;
                    claim = next_claim;
                }
            }
            // The serial worker may have committed the new registration
            // while its Ack completion is still in flight. The old verifier
            // is not a current-claim oracle during that actual handoff.
            if pending.is_none() {
                assert!(verifier.validate(&claim).is_ok());
            }
            assert!(state
                .native_audio_renderer
                .as_ref()
                .is_some_and(|actual| std::sync::Weak::ptr_eq(actual, &weak)));
            assert_eq!(state.playback_engine.snapshot(), engine_before);
            assert!(state.playback_engine.native_audio_control_hold_ticket().is_none());
            assert_eq!(
                state.audio_playback.native_control_hold_generation(),
                pending.as_ref().map(|_| generation),
                "only the queued exact author receipt may retain AuthorPending"
            );
            assert!(state.audio_playback.native_reprepare_pending_generation().is_none());
            assert_eq!(output.lock().queued, 0);
            let settled = pending.is_none()
                && installed_awaiting.is_none()
                && proposals == acknowledgements
                && installations == acknowledgements
                && state.authoring.as_ref().unwrap().history().diagnostics().pending_entries == 0;
            let complete = match phase {
                0 => renderer_id.is_some(),
                1 => {
                    opened.is_some()
                        && (before_open.is_none() || redelivered_after_open)
                        && settled
                        && parameters
                            .as_ref()
                            .is_some_and(|facts| Some(facts.target()) == current_target)
                        && state.native_audio_editor_surface_state(&payload) == (true, false)
                }
                2 => {
                    closed
                        && settled
                        && state.native_audio_editor_surface_state(&payload) == (false, false)
                }
                _ => unreachable!(),
            };
            if complete {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "actual GUI operation or canonical initialization Ack did not settle"
            );
            std::thread::yield_now();
        }
        if phase == 1 {
            assert!(weak.upgrade().unwrap().native_author_snapshot_current());
            if let Some(current) = attachment {
                assert_eq!(Some(current), current_target);
            }
            let process = native_object.expect("same-target real parameter facts").0;
            // Observe the existing host parent and its entire actual native
            // descendant tree. No plugin class/title/child count is presumed.
            struct Search {
                process: u32,
                parent: *mut std::ffi::c_void,
                parents: usize,
                children: Vec<*mut std::ffi::c_void>,
                valid: bool,
            }
            unsafe extern "system" fn child(window: *mut std::ffi::c_void, data: isize) -> i32 {
                let search = unsafe { &mut *(data as *mut Search) };
                let mut actual = 0;
                // Never unwind through a Windows callback. Report every
                // observation/allocation failure to assertions after EnumWindows.
                if unsafe { window_process(window, &mut actual) } == 0
                    || actual != search.process
                    || unsafe { window_ancestor(window, 2) } != search.parent
                    || search.children.try_reserve_exact(1).is_err()
                {
                    search.valid = false;
                    return 0;
                }
                search.children.push(window);
                1
            }
            unsafe extern "system" fn parent(window: *mut std::ffi::c_void, data: isize) -> i32 {
                let search = unsafe { &mut *(data as *mut Search) };
                let mut actual = 0;
                unsafe {
                    window_process(window, &mut actual);
                }
                if actual != search.process {
                    return 1;
                }
                let mut class = [0_u16; 128];
                let mut title = [0_u16; 128];
                let class_len = unsafe { window_class(window, class.as_mut_ptr(), 128) };
                let title_len = unsafe { window_text(window, title.as_mut_ptr(), 128) };
                if class_len != 16
                    || title_len != 8
                    || !class[..16].iter().copied().eq("VST3PluginWindow".encode_utf16())
                    || !title[..8].iter().copied().eq("Mondrian".encode_utf16())
                {
                    return 1;
                }
                search.parents = search.parents.saturating_add(1);
                search.parent = window;
                unsafe {
                    enumerate_child_windows(window, child, data);
                }
                1
            }
            let mut search = Search {
                process: process.get(),
                parent: std::ptr::null_mut(),
                parents: 0,
                children: Vec::new(),
                valid: true,
            };
            assert_ne!(
                unsafe { enumerate_windows(parent, (&mut search as *mut Search) as isize) },
                0
            );
            assert!(
                search.valid,
                "all captured descendants have the authenticated owner/root"
            );
            assert_eq!(search.parents, 1, "one authenticated actual host parent");
            assert!(
                !search.children.is_empty(),
                "actual embedded native window tree"
            );
            windows.push(search.parent);
            windows.extend(search.children);
            for window in &windows {
                assert_ne!(unsafe { is_window(*window) }, 0);
            }
        }
    }
    if before_open.is_some() {
        assert!(
            redelivered_after_open,
            "actual Opened precedes natural redelivery, canonical install and exact Ack"
        );
    }
    // Natural scheduling is not forced to produce an early callback. Record
    // whether this run actually covered pending-Open deferral; portable
    // consumer controls cover that branch deterministically.

    assert_eq!(
        proposals, acknowledgements,
        "every actual initialization receipt exact-Acked once, not once per reoffer"
    );
    assert_eq!(
        installations, acknowledgements,
        "one canonical installation per exact acknowledged prefix"
    );
    assert!(installed_awaiting.is_none());
    assert!(pending.is_none());
    for window in windows {
        assert_eq!(
            unsafe { is_window(window) },
            0,
            "Closed destroys the captured actual HWNDs"
        );
    }
    assert_eq!(
        state.status_hint().map(|(message, _)| message.id),
        Some("native-editor-closed")
    );
    assert!(verifier.validate(&claim).is_ok());
    assert!(weak.upgrade().unwrap().native_author_snapshot_current());
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
    println!(
        "{}",
        serde_json::json!({
            "case": "app-surge-fx-editor", "descriptor_version": descriptor_version,
            "initialization_proposals": proposals, "exact_acknowledgements": acknowledgements,
            "pre_open_deliveries": pre_open_deliveries,
            "pending_open_deferred": pending_open_deferred,
            "installations": installations,
            "installed_awaiting_ack_observations": awaiting_ack_observations,
            "same_retained_redelivered_after_open": redelivered_after_open,
            "opened": true, "closed": closed, "same_native_object": true,
            "pcm_qualified": false, "device_qualified": false, "author_edit_stimulus": false,
            "algorithm_qualified": false, "realtime_qualified": false
        })
    );
    drop(state);
}

#[test]
#[ignore = "current normal Product and owned CLAP Gain DLL; ProjectClose native custody before whole-App cleanup, no GUI/device/PCM"]
fn app_owned_clap_project_close_releases_native_source_before_app_shutdown() {
    let ColdNativeControl { mut state, _directory, output, renderer: weak } =
        prepare_app_clap_editor_control();
    let sequence = state.active_sequence().expect("actual authored direct-root Program");
    assert!(sequence.audio_program.note_lanes.is_empty());
    assert_eq!(sequence.audio_program.outputs.len(), 1);
    assert_eq!(
        sequence.audio_program.outputs[0].strip.pre_fader.processors.len(),
        1
    );
    let original = sequence.audio_program.outputs[0].strip.pre_fader.processors[0].clone();
    assert_eq!(original.parameters.len(), 1);
    assert_eq!(
        original.opaque_state.as_ref().unwrap().as_slice(),
        &1.0_f32.to_le_bytes()
    );
    assert_eq!(
        state.authoring.as_ref().unwrap().history().diagnostics().undo_entries,
        0
    );
    assert_eq!(
        state.authoring.as_ref().unwrap().history().diagnostics().pending_entries,
        0
    );
    assert!(state.native_audio_editor.is_none());
    assert!(!state.is_playing());
    let verifier = weak
        .upgrade()
        .expect("actual installed Timeline")
        .owner_installation_verifier()
        .expect("original Media Source registration");
    let claim = verifier.current_claim().expect("actual installed claim");
    assert!(verifier.validate(&claim).is_ok());
    let generation = state.audio_playback_snapshot().generation;
    let mut renderer_id = None;
    poll_idle_until(&mut state, generation, |event| match event {
        AudioPlaybackEvent::NativeOwnerBindingsObserved {
            renderer_id: actual,
            generation: observed,
            status,
        } => {
            assert!(renderer_id.is_none());
            assert_eq!(*observed, generation);
            assert_eq!(
                *status,
                mondrian_media::AudioPcmNativeOwnerBindingStatus::Ready
            );
            renderer_id = Some(*actual);
            true
        }
        AudioPlaybackEvent::NativeParameterObservationsObserved { observation, .. } => {
            assert!(observation.target().continuity_epoch.is_none());
            false
        }
        AudioPlaybackEvent::NativeDiagnosticsObserved { .. } => false,
        other => panic!("NoGUI setup admits no author, editor or PCM work: {other:?}"),
    });
    let renderer_id = renderer_id.unwrap();
    assert!(verifier.validate(&claim).is_ok());
    assert!(state.audio_playback.native_editor_source_available());
    assert_eq!(output.lock().queued, 0);

    // Save all target outcomes while AppState is still alive. A target RED
    // must not unwind past real native owners before the consuming cleanup.
    let close = state.close_project().map_err(|error| error.to_string());
    let post_close_generation = state.audio_playback_snapshot().generation;
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut observation_error = None;
    let mut no_pcm = true;
    let mut no_editor_or_hold = true;
    loop {
        if close.is_err() {
            break;
        }
        match state.audio_playback.poll(AudioPlaybackMode::Idle, audio_anchor(0)) {
            Ok(poll) => {
                no_pcm &= poll.snapshot.generation == post_close_generation
                    && poll.snapshot.in_flight == 0
                    && poll.snapshot.next_start_sample == 0
                    && poll.snapshot.output.is_some_and(|snapshot| snapshot.buffered_frames == 0)
                    && output.lock().queued == 0;
                for event in poll.events {
                    let current = match &event {
                        AudioPlaybackEvent::NativeParameterObservationsObserved {
                            renderer_id: actual,
                            generation: observed,
                            observation,
                        } => {
                            *actual == renderer_id
                                && *observed == post_close_generation
                                && observation.target().continuity_epoch.is_none()
                        }
                        AudioPlaybackEvent::NativeDiagnosticsObserved {
                            renderer_id: actual,
                            generation: observed,
                            ..
                        } => *actual == renderer_id && *observed == post_close_generation,
                        _ => false,
                    };
                    if !current && observation_error.is_none() {
                        observation_error = Some(format!(
                            "ProjectClose emitted unexpected author/editor/hold/PCM work: {event:?}"
                        ));
                    }
                    if let Err(error) = state.handle_audio_playback_event(event, Instant::now()) {
                        observation_error.get_or_insert_with(|| {
                            format!("original ProjectClose App event consumer: {error:?}")
                        });
                    }
                }
            }
            Err(error) => {
                observation_error = Some(format!("original ProjectClose Idle poll: {error:?}"));
            }
        }
        no_editor_or_hold &= state.native_audio_editor.is_none()
            && state.playback_engine.native_audio_control_hold_ticket().is_none()
            && state.audio_playback.native_control_hold_generation().is_none()
            && state.audio_playback.native_reprepare_pending_generation().is_none();
        let released = matches!(
            verifier.current_claim(),
            Err(mondrian_core::AudioOwnerInstallationError::NotInstalled)
        ) && !state.audio_playback.native_editor_source_available()
            && weak.upgrade().is_none();
        if released || observation_error.is_some() || Instant::now() >= deadline {
            break;
        }
        std::thread::yield_now();
    }
    // These are frozen BEFORE shutdown_for_endurance can revoke the Source.
    let claim_before_app_shutdown = verifier.current_claim();
    let claim_released_before_app_shutdown = matches!(
        &claim_before_app_shutdown,
        Err(mondrian_core::AudioOwnerInstallationError::NotInstalled)
    );
    let source_available_before_app_shutdown =
        state.audio_playback.native_editor_source_available();
    let renderer_released_before_app_shutdown = weak.upgrade().is_none();
    let project_released_before_app_shutdown =
        state.authoring.is_none() && state.active_sequence().is_none();
    let snapshot_before_app_shutdown = state.audio_playback_snapshot();
    no_pcm &= snapshot_before_app_shutdown.in_flight == 0
        && snapshot_before_app_shutdown.next_start_sample == 0
        && output.lock().queued == 0;
    let target_released = claim_released_before_app_shutdown
        && !source_available_before_app_shutdown
        && renderer_released_before_app_shutdown
        && project_released_before_app_shutdown;

    println!(
        "{}",
        serde_json::json!({
            "case": "app-clap-project-close", "stage": "before-app-shutdown",
            "close_error": close.as_ref().err(), "observation_error": observation_error,
            "claim_observation": format!("{claim_before_app_shutdown:?}"),
            "claim_not_installed": claim_released_before_app_shutdown,
            "media_source_available": source_available_before_app_shutdown,
            "renderer_released": renderer_released_before_app_shutdown,
            "project_released": project_released_before_app_shutdown,
            "target_released": target_released, "no_pcm": no_pcm,
            "no_editor_or_hold": no_editor_or_hold,
            "gui_qualified": false, "device_qualified": false,
            "state_entry_qualified": false, "realtime_qualified": false,
            "graceful_100ms_qualified": false
        })
    );

    // All App/Media owners share this one cleanup deadline. Successful cleanup
    // cannot replace any of the frozen ProjectClose observations above.
    let cleanup = state.shutdown_for_endurance(Instant::now() + Duration::from_secs(2));
    println!(
        "ProjectClose test consuming cleanup: audio={:?}; non_audio_released={}",
        cleanup.audio,
        cleanup.all_non_audio_resources_released()
    );
    assert!(
        cleanup.all_resources_released(),
        "PROJECT_CLOSE_TEST_CLEANUP_HOLD: {cleanup:?}"
    );
    assert!(
        weak.upgrade().is_none(),
        "consuming cleanup releases the real final Runtime alias"
    );
    assert!(matches!(
        verifier.current_claim(),
        Err(mondrian_core::AudioOwnerInstallationError::NotInstalled)
    ));
    assert!(
        close.is_ok(),
        "original ProjectClose failed before custody observation: {close:?}"
    );
    assert!(observation_error.is_none(), "{observation_error:?}");
    assert!(
        no_pcm,
        "ProjectClose must not admit PCM or queue software output"
    );
    assert!(
        no_editor_or_hold,
        "NoGUI/no-author ProjectClose acquired an unrelated hold"
    );
    assert!(target_released,
        "PROJECT_CLOSE_NATIVE_CUSTODY_BEFORE_APP_SHUTDOWN: claim={claim_before_app_shutdown:?}, media_source_available={source_available_before_app_shutdown}, renderer_released={renderer_released_before_app_shutdown}, project_released={project_released_before_app_shutdown}");
}

mod headless_native_author;
mod project_state_roundtrip;
mod headless_vst3_terminal_author;

mod native_parameter_adoption;
mod surge_parameter_adoption;

mod idle_vst3_controller;
mod fresh_vst3_host_overlay;

mod idle_clap_constant;
mod fresh_clap_host_overlay;
