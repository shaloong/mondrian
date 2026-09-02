#![cfg(feature = "validation")]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use mondrian_app::app::endurance_reference_output::PersistentReferenceOutputPump;
use mondrian_app::app::{AppReferenceOutputTeardownStatus, AppState};
use mondrian_core::{AudioChannelLayout, ColorSpace, Rational};
use mondrian_reference_output::{
    ReferenceOutputAncillaryPolicy, ReferenceOutputMode, ReferenceOutputOpenRequest,
    ReferenceOutputPixelFormat, ReferenceOutputRange, ReferenceOutputReferencePolicy,
    ReferenceOutputScan, ReferenceOutputSignal, ReferenceOutputState,
    SimulatedReferenceOutputAdapter,
};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

struct FixtureRoot(PathBuf);

impl FixtureRoot {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "mondrian-reference-pump-{}-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).expect("create Reference pump fixture root");
        Self(path)
    }
}

impl Drop for FixtureRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn request() -> ReferenceOutputOpenRequest {
    ReferenceOutputOpenRequest {
        signal: ReferenceOutputSignal {
            width: 1920,
            height: 1080,
            frame_rate: Rational::FPS_25,
            scan: ReferenceOutputScan::Progressive,
            pixel_format: ReferenceOutputPixelFormat::Yuv422TenV210,
            color_space: ColorSpace::Rec709,
            range: ReferenceOutputRange::Full,
            hdr: None,
            audio_layout: AudioChannelLayout::Stereo,
        },
        reference_policy: ReferenceOutputReferencePolicy::FreeRunAllowed,
        ancillary_policy: ReferenceOutputAncillaryPolicy::Disabled,
        preroll_frames: 2,
        max_scheduled_frames: 3,
    }
}

fn create_app(root: &FixtureRoot) -> AppState {
    let mut app = AppState::new();
    app.create_new_project_at(
        root.0.join("reference-pump.mdp"),
        "Reference pump",
        1920,
        1080,
        Rational::FPS_25,
    )
    .expect("create project");
    app
}

fn install_simulated_output(
    app: &mut AppState,
    request: &ReferenceOutputOpenRequest,
) -> mondrian_reference_output::ReferenceOutputDeviceDescriptor {
    app.install_reference_output_adapter(Box::new(
        SimulatedReferenceOutputAdapter::new(vec![ReferenceOutputMode {
            signal: request.signal.clone(),
            supports_hdr_signal: false,
            supports_static_hdr_metadata: false,
            supports_reference_status: true,
            supports_ancillary: false,
            supports_ancillary_readback: false,
        }])
        .expect("simulated Reference Adapter"),
    ))
    .expect("install Reference Adapter");
    app.discover_reference_output_devices()
        .expect("discover Reference device")
        .remove(0)
}

fn settle_reference_output(app: &mut AppState) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        match app.discover_reference_output_devices() {
            Ok(_) => break,
            Err(mondrian_app::app::AppReferenceOutputError::TeardownInProgress)
                if std::time::Instant::now() < deadline =>
            {
                std::thread::yield_now();
            }
            Err(error) => panic!("Reference Output stop did not settle: {error}"),
        }
    }
    assert_eq!(
        app.reference_output_teardown_status(),
        AppReferenceOutputTeardownStatus::Idle
    );
    assert_eq!(
        app.reference_output_diagnostics()
            .expect("terminal Reference diagnostics")
            .state,
        ReferenceOutputState::Stopped
    );
}

#[test]
fn persistent_pump_schedules_full_reference_preroll_and_contiguous_public_audio() {
    let root = FixtureRoot::new();
    let mut app = create_app(&root);
    let request = request();
    let device = install_simulated_output(&mut app, &request);
    let mut pump = PersistentReferenceOutputPump::prepare(&app, request, 0)
        .expect("prepare persistent Reference pump");

    pump.open_preroll_and_start(&mut app, &device)
        .expect("open, preroll, and start");
    assert_eq!(pump.scheduled_frames(), 2);
    assert_eq!(pump.next_frame_index(), 2);
    assert_eq!(
        app.reference_output_diagnostics().expect("diagnostics").state,
        ReferenceOutputState::Running
    );

    pump.pump_next(&mut app).expect("schedule contiguous running frame");
    assert_eq!(pump.scheduled_frames(), 3);
    assert_eq!(pump.next_frame_index(), 3);
    let diagnostics = app.reference_output_diagnostics().expect("diagnostics");
    assert_eq!(diagnostics.completed_frames, 2);
    assert_eq!(diagnostics.scheduled_audio_frames, 5_760);

    pump.begin_close(&mut app).expect("begin Reference close");
    settle_reference_output(&mut app);
    app.close_project().expect("close project");
}

#[test]
fn persistent_pump_permanently_rejects_author_drift_before_device_open() {
    let root = FixtureRoot::new();
    let mut app = create_app(&root);
    let request = request();
    let device = install_simulated_output(&mut app, &request);
    let mut pump = PersistentReferenceOutputPump::prepare(&app, request, 0)
        .expect("prepare persistent Reference pump");

    app.add_video_track().expect("advance author generation");
    let first = pump
        .open_preroll_and_start(&mut app, &device)
        .expect_err("stale frozen binding must reject device open")
        .to_string();
    let repeated = pump
        .open_preroll_and_start(&mut app, &device)
        .expect_err("faulted generation must not restart")
        .to_string();

    assert_eq!(repeated, first);
    assert!(first.contains("author binding changed"));
    assert!(app.reference_output_binding().is_none());
    assert!(
        app.reference_output_diagnostics()
            .is_some_and(|diagnostics| diagnostics.device_id.is_none()),
        "stale preparation must not open a device Session"
    );
    pump.begin_close(&mut app).expect("close unopened pump");
    app.close_project().expect("close project");
}
