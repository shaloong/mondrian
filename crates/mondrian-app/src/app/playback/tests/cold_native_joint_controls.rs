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
    struct Search {
        process: u32,
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
        const EXPECTED: &[u8] = b"MondrianOwnedVst3EditorFixture";
        if process == search.process
            && length == EXPECTED.len() as i32
            && class[..EXPECTED.len()]
                .iter()
                .copied()
                .eq(EXPECTED.iter().map(|byte| u16::from(*byte)))
        {
            search.matches = search.matches.saturating_add(1);
            search.view = window;
        }
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
    mut observe: impl FnMut(&AudioPlaybackEvent) -> bool,
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
            state
                .handle_audio_playback_event(event, Instant::now())
                .expect("original App consumer");
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
        let curve = parameter
            .automation
            .as_ref()
            .expect("every original writable constant lane retained");
        assert_eq!(&curve.parameter_id, id);
        assert!(curve.keyframes.is_empty());
    }
    assert_eq!(
        original.parameters[&gain].automation.as_ref().unwrap().default_value.to_bits(),
        0.5_f64.to_bits()
    );
    assert_eq!(
        original.parameters[&balance]
            .automation
            .as_ref()
            .unwrap()
            .default_value
            .to_bits(),
        0.5_f64.to_bits()
    );
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
        let curve = current.automation.as_ref().expect("all original lanes retained");
        assert!(curve.keyframes.is_empty());
        let expected = if *id == gain {
            0.75_f64
        } else if *id == balance {
            0.25_f64
        } else {
            prior.automation.as_ref().unwrap().default_value
        };
        assert_eq!(curve.default_value.to_bits(), expected.to_bits());
    }
    assert_eq!(after.parameters[&mode], original.parameters[&mode]);
    assert_ne!(after.opaque_state, original.opaque_state);
    assert_eq!(after.opaque_state.as_ref(), delivered.state());
    assert_editor_configuration(after.opaque_state.as_ref().unwrap().as_ref(), 0.75, 0.25, 1);
    let accepted_author_state = serde_json::to_value(after).unwrap();
    let installed_claim =
        verifier.current_claim().expect("same Source after legitimate installation");
    assert!(verifier.validate(&installed_claim).is_ok());
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
    assert!(verifier.validate(&installed_claim).is_ok());
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
        verifier.current_claim(),
        Err(mondrian_core::AudioOwnerInstallationError::NotInstalled)
    ));
    assert!(weak.upgrade().is_none());
    // The whole App success path is real, with no PCM windows or device. Zero-
    // sample catchup/query/control calls are not a DSP algorithm, physical GUI,
    // real-time or individual cold-probe custody qualification.
    drop(state);
}
