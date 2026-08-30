use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use mondrian_app::app::{AppReferenceOutputError, AppState};
use mondrian_app::app_ui::preferences_store::{
    load_app_ui_preferences_from, persist_app_ui_preferences_to, AppUiPreferences,
};
use mondrian_core::{AudioChannelLayout, ColorSpace, Rational};
use mondrian_reference_output::{
    ReferenceAudioFrame, ReferenceOutputAncillaryPolicy, ReferenceOutputBundle,
    ReferenceOutputDeviceId, ReferenceOutputMode, ReferenceOutputOpenRequest,
    ReferenceOutputPixelFormat, ReferenceOutputProvider, ReferenceOutputRange,
    ReferenceOutputReferencePolicy, ReferenceOutputRoutingPreferences, ReferenceOutputScan,
    ReferenceOutputSignal, ReferenceOutputState, ReferenceVideoFrame,
    SimulatedReferenceOutputAdapter,
};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

struct FixtureRoot(PathBuf);

impl FixtureRoot {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "mondrian-reference-output-{}-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).expect("create Reference Output fixture root");
        Self(path)
    }
}

impl Drop for FixtureRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn signal() -> ReferenceOutputSignal {
    ReferenceOutputSignal {
        width: 1920,
        height: 1080,
        frame_rate: Rational::FPS_25,
        scan: ReferenceOutputScan::Progressive,
        pixel_format: ReferenceOutputPixelFormat::Yuv422TenV210,
        color_space: ColorSpace::Rec709,
        range: ReferenceOutputRange::Full,
        hdr: None,
        audio_layout: AudioChannelLayout::Stereo,
    }
}

fn bundle(signal: &ReferenceOutputSignal, frame_index: u64) -> ReferenceOutputBundle {
    let row_bytes = signal.width.div_ceil(6) * 16;
    let video = ReferenceVideoFrame::from_program_output(
        signal,
        frame_index,
        row_bytes,
        vec![0; row_bytes as usize * signal.height as usize],
    )
    .expect("valid v210 extent");
    let audio_frames =
        signal.audio_frames_for_video_frame(frame_index).expect("audio cadence") as usize;
    let audio = ReferenceAudioFrame::new(
        signal,
        frame_index,
        vec![0; audio_frames * signal.audio_layout.channel_count()],
    )
    .expect("valid embedded audio extent");
    ReferenceOutputBundle {
        video,
        audio,
        ancillary: mondrian_reference_output::AncillaryFrame::empty(frame_index),
    }
}

#[test]
fn reference_output_is_machine_local_and_stale_author_state_stops_playout() {
    let fixture = FixtureRoot::new();
    let mut state = AppState::new();
    state
        .create_new_project_at(
            fixture.0.join("reference-output.mdp"),
            "Reference Output",
            1920,
            1080,
            Rational::FPS_25,
        )
        .expect("create Reference Output project");

    let signal = signal();
    let mode = ReferenceOutputMode {
        signal: signal.clone(),
        supports_hdr_signal: false,
        supports_static_hdr_metadata: false,
        supports_reference_status: true,
        supports_ancillary: true,
        supports_ancillary_readback: true,
    };
    state
        .install_reference_output_adapter(Box::new(
            SimulatedReferenceOutputAdapter::new(vec![mode]).expect("simulated Adapter"),
        ))
        .expect("install Adapter");

    let generation_before = state.project_author_generation();
    let revision_before = state.active_sequence().expect("Sequence").revision;
    let history_before = state.authoring_history().expect("History").diagnostics().undo_entries;
    let device = state
        .discover_reference_output_devices()
        .expect("discover")
        .into_iter()
        .next()
        .expect("simulated device");
    state
        .open_reference_output(
            &device,
            ReferenceOutputOpenRequest {
                signal: signal.clone(),
                reference_policy: ReferenceOutputReferencePolicy::FreeRunAllowed,
                ancillary_policy: ReferenceOutputAncillaryPolicy::Disabled,
                preroll_frames: 2,
                max_scheduled_frames: 3,
            },
            0,
        )
        .expect("open exact Session");
    state
        .schedule_reference_output(bundle(&signal, 0))
        .expect("schedule first bundle");
    state
        .schedule_reference_output(bundle(&signal, 1))
        .expect("schedule second bundle");
    state.start_reference_output().expect("start after preroll");
    assert_eq!(
        state.poll_reference_output(8).expect("drain completions"),
        2
    );

    let diagnostics = state.reference_output_diagnostics().expect("diagnostics");
    assert_eq!(diagnostics.state, ReferenceOutputState::Running);
    assert_eq!(diagnostics.completed_frames, 2);
    assert_eq!(diagnostics.scheduled_audio_frames, 3_840);
    assert!(diagnostics.provider.as_ref().is_some_and(|provider| !provider.hardware_backed));
    assert_eq!(state.project_author_generation(), generation_before);
    assert_eq!(
        state.active_sequence().expect("Sequence").revision,
        revision_before
    );
    assert_eq!(
        state.authoring_history().expect("History").diagnostics().undo_entries,
        history_before,
        "device lifecycle must not enter Project Undo/Redo"
    );

    state.add_video_track().expect("author edit");
    let stale = state
        .poll_reference_output(1)
        .expect_err("stale author binding must stop output");
    assert!(matches!(
        stale,
        AppReferenceOutputError::StaleBinding { .. }
    ));
    assert_eq!(state.reference_output_binding(), None);
    assert_eq!(
        state.reference_output_diagnostics().expect("diagnostics").state,
        ReferenceOutputState::Stopped
    );

    state.close_project().expect("close project");
    assert!(state.reference_output_diagnostics().is_none());
}

#[test]
fn machine_local_reference_output_routing_round_trips_without_activation() {
    let fixture = FixtureRoot::new();
    let path = fixture.0.join("app-ui-preferences.json");
    let preferences = AppUiPreferences {
        reference_output: ReferenceOutputRoutingPreferences {
            provider: Some(ReferenceOutputProvider::DeckLink),
            device_id: Some(
                ReferenceOutputDeviceId::new("decklink:persistent-device")
                    .expect("stable device identity"),
            ),
            pixel_format: ReferenceOutputPixelFormat::Rgb444TwelveIn16Le,
            reference_policy: ReferenceOutputReferencePolicy::RequireExternalLock,
        },
        ..Default::default()
    };

    persist_app_ui_preferences_to(&path, &preferences).expect("persist preferences");
    let loaded = load_app_ui_preferences_from(&path);

    assert_eq!(loaded.reference_output, preferences.reference_output);
    assert!(
        AppState::new().reference_output_diagnostics().is_none(),
        "loading routing preferences is separate from installing/acquiring hardware"
    );
}
