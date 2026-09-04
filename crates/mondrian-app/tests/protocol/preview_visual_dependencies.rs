use super::*;

#[test]
fn observer_terminal_notification_sees_unhealthy_before_normal_or_panic_wake() {
    for panicking in [false, true] {
        let (notifier, watch) =
            super::super::preview_work_notification::preview_work_notification_channel();
        let mut observer =
            PreviewVisualDependencyObserver::with_timing_and_notifier(test_timing(), notifier);
        let healthy = observer.healthy.clone();
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let (sent, received) = mpsc::channel();
        watch
            .install_waker(move || {
                if calls.fetch_add(1, Ordering::Relaxed) > 0 {
                    sent.send(healthy.load(Ordering::Acquire)).expect("record terminal health");
                }
            })
            .unwrap_or_else(|failure| panic!("{}", failure.reason));
        if panicking {
            observer
                .command_tx
                .send(ObservationCommand::PanicForTest)
                .expect("inject body panic");
        } else {
            observer.begin_shutdown();
        }
        assert!(!received.recv_timeout(Duration::from_secs(5)).expect("terminal wake"));
        let outcome = observer.shutdown_until(Instant::now() + Duration::from_secs(5));
        assert_eq!(
            outcome,
            if panicking {
                PreviewOwnedWorkerShutdown::Panicked
            } else {
                PreviewOwnedWorkerShutdown::Terminated
            }
        );
        assert!(watch.shutdown_and_wait().all_resources_released());
    }
}

#[test]
fn observer_failed_spawn_publishes_progress_without_synthetic_results() {
    let (notifier, watch) =
        super::super::preview_work_notification::preview_work_notification_channel();
    let before = watch.revision();
    let mut observer = PreviewVisualDependencyObserver::with_configuration_and_spawn(
        test_timing(),
        1,
        1,
        notifier,
        |_| Err(std::io::Error::other("injected observer spawn failure")),
    );
    assert!(!observer.is_healthy());
    assert_ne!(watch.revision(), before);
    assert!(observer.poll_refreshes().is_empty());
    assert_eq!(
        observer.shutdown_and_wait(),
        PreviewOwnedWorkerShutdown::NotStarted
    );
    assert!(watch.shutdown_and_wait().all_resources_released());
}

#[test]
fn dependency_observer_shutdown_consumes_worker_exactly_once() {
    let mut observer = PreviewVisualDependencyObserver::with_timing(test_timing());
    assert_eq!(
        observer.shutdown_until(Instant::now() + Duration::from_secs(5)),
        PreviewOwnedWorkerShutdown::Terminated
    );
    assert_eq!(
        observer.shutdown_until(Instant::now()),
        PreviewOwnedWorkerShutdown::NotStarted
    );
}

#[test]
fn dependency_observer_shutdown_retains_panic_and_does_not_renew_expired_deadline() {
    let mut observer = PreviewVisualDependencyObserver::with_timing(test_timing());
    assert_eq!(
        observer.shutdown_and_wait(),
        PreviewOwnedWorkerShutdown::Terminated
    );
    observer.worker = Some(std::thread::spawn(|| {
        panic!("injected dependency worker panic")
    }));
    assert_eq!(
        observer.shutdown_until(Instant::now() + Duration::from_secs(5)),
        PreviewOwnedWorkerShutdown::Panicked
    );
    let (release, released) = mpsc::channel();
    let (finished, finish) = mpsc::channel();
    observer.worker = Some(std::thread::spawn(move || {
        released.recv().expect("release controlled worker");
        finished.send(()).expect("observe return");
    }));
    assert_eq!(
        observer.shutdown_until(Instant::now()),
        PreviewOwnedWorkerShutdown::TimedOutDetached
    );
    release.send(()).expect("release test thread");
    finish.recv_timeout(Duration::from_secs(5)).expect("test thread returned");
}
use mondrian_core::automation::{ParameterResourceReference, PropertyValue};
use mondrian_core::{AssetId, Color, FramePosition, Rational, TimelineTime};
use mondrian_effects::{EffectNode, EffectNodeExt, EffectType};
use mondrian_renderer::PreparedVisualProgram;
use mondrian_timeline::{Clip, Sequence, Track};

fn test_timing() -> ObservationTiming {
    ObservationTiming {
        initial_delay: Duration::ZERO,
        stable_interval: Duration::from_millis(5),
        result_retry: Duration::from_millis(2),
        shutdown_poll: Duration::from_millis(5),
    }
}

fn timeline_time(frame: i64, rate: Rational) -> TimelineTime {
    TimelineTime::from_frame_position(FramePosition::new(frame, rate)).expect("valid test time")
}

fn single_solid_sequence(effect: Option<EffectNode>) -> Sequence {
    let mut sequence = Sequence::new("dependency observer");
    sequence.video_tracks.clear();
    let rate = sequence.time_base();
    let mut track = Track::new_video("V1");
    let mut clip = Clip::new_solid_color(
        AssetId::new(),
        Color::BLACK,
        timeline_time(0, rate),
        timeline_time(20, rate),
    )
    .expect("solid Clip");
    if let Some(effect) = effect {
        clip.add_effect_node(effect);
    }
    track.add_clip(clip).expect("add solid Clip");
    sequence.video_tracks.push(track);
    sequence
}

fn prepare_stable(sequence: &Sequence) -> Arc<PreparedVisualProgram> {
    for _ in 0..32 {
        match PreparedVisualProgram::prepare(sequence) {
            Ok(program) => return Arc::new(program),
            Err(mondrian_renderer::PreparedVisualProgramError::EffectRegistryChanged {
                ..
            }) => {}
            Err(error) => panic!("visual preparation failed: {error}"),
        }
    }
    panic!("Effect registry did not stabilize during test")
}

fn lut_effect(path: std::path::PathBuf) -> EffectNode {
    let mut lut = EffectNode::with_defaults(EffectType::Lut3D);
    let processing_space_id = EffectType::Lut3D
        .parameter_id("processing_space")
        .expect("processing-space parameter ID");
    let path_id = EffectType::Lut3D.parameter_id("path").expect("path parameter ID");
    lut.set_static_value_by_parameter(
        &processing_space_id,
        PropertyValue::Enum("scene_linear".to_owned()),
    )
    .expect("set processing space");
    lut.set_static_value_by_parameter(
        &path_id,
        PropertyValue::Resource(ParameterResourceReference::ExternalFile { path }),
    )
    .expect("bind LUT");
    lut
}

fn wait_for_refresh(
    observer: &PreviewVisualDependencyObserver,
) -> Option<PreviewVisualDependencyRefresh> {
    let deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < deadline {
        if let Some(refresh) = observer.poll_refreshes().into_iter().next() {
            return Some(refresh);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    None
}

#[test]
fn identity_program_does_not_publish_false_refresh() {
    let observer = PreviewVisualDependencyObserver::with_timing(test_timing());
    observer.observe(prepare_stable(&single_solid_sequence(None)));
    std::thread::sleep(Duration::from_millis(25));
    assert!(observer.poll_refreshes().is_empty());
}

#[test]
fn retryable_external_blocker_publishes_exact_program_refresh() {
    let missing_path =
        std::env::temp_dir().join(format!("mondrian-observer-missing-{}.cube", AssetId::new()));
    let _ = std::fs::remove_file(&missing_path);
    let program = prepare_stable(&single_solid_sequence(Some(lut_effect(missing_path))));
    let expected_sequence_id = program.sequence_id();
    let expected_revision = program.sequence_revision();
    let work_notifier = PreviewWorkNotifier::default();
    let work_watch = work_notifier.watch();
    let work_revision_before = work_watch.revision();
    let observer =
        PreviewVisualDependencyObserver::with_timing_and_notifier(test_timing(), work_notifier);
    observer.observe(program);

    let refresh = wait_for_refresh(&observer).expect("external blocker refresh");
    assert_ne!(
        work_watch.wait_for_change(work_revision_before, Duration::from_secs(1)),
        work_revision_before
    );
    assert_eq!(refresh.sequence_id, expected_sequence_id);
    assert_eq!(refresh.sequence_revision, expected_revision);
}

#[test]
fn result_backpressure_retries_the_cached_refresh_without_rechecking_the_resource() {
    const IDENTITY_LUT: &str = "LUT_3D_SIZE 2\n\
0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n";
    const CHANGED_LUT: &str = "LUT_3D_SIZE 2\n\
1 1 1\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n0 0 0\n";
    let unique = AssetId::new();
    let path = std::env::temp_dir().join(format!("mondrian-observer-backpressure-{unique}.cube"));
    std::fs::write(&path, IDENTITY_LUT).expect("write initial LUT");
    let changed_program = prepare_stable(&single_solid_sequence(Some(lut_effect(path.clone()))));
    let changed_sequence_id = changed_program.sequence_id();
    std::fs::write(&path, CHANGED_LUT).expect("change observed LUT");
    let timing = test_timing();
    let now = Instant::now();
    let mut entry = ObservationEntry {
        program: Arc::clone(&changed_program),
        next_check: now,
        pending_refresh_reason: None,
    };
    let (result_tx, result_rx) = mpsc::sync_channel(1);
    result_tx
        .try_send(DependencyRefreshResult {
            program: Arc::clone(&changed_program),
            reason: Arc::from("occupy the bounded result slot"),
        })
        .expect("fill result slot");

    assert!(matches!(
        process_due_observation(&mut entry, &result_tx, now, timing),
        DueObservationOutcome::Retained
    ));
    assert!(
        entry.pending_refresh_reason.is_some(),
        "backpressure must cache already-proven refresh evidence"
    );
    let _ = result_rx.try_recv().expect("drain occupying result");
    std::fs::write(&path, IDENTITY_LUT).expect("restore LUT before result retry");

    assert!(matches!(
        process_due_observation(&mut entry, &result_tx, Instant::now(), timing),
        DueObservationOutcome::Published
    ));
    let refreshed = result_rx.try_recv().expect("cached refresh result");
    assert_eq!(refreshed.program.sequence_id(), changed_sequence_id);
    std::fs::remove_file(path).expect("remove LUT");
}

#[test]
fn ordinary_drop_joins_the_observer_worker_within_the_bounded_grace() {
    let observer = PreviewVisualDependencyObserver::with_timing(test_timing());
    let healthy = Arc::clone(&observer.healthy);

    drop(observer);

    assert!(
        !healthy.load(Ordering::Acquire),
        "the joined worker health guard must publish terminal state"
    );
}
