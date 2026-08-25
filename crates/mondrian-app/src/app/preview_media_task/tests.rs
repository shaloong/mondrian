use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering as AtomicOrdering};
use std::thread;

use super::*;
use crate::app::preview_access_mode::MediaPreviewJobEnqueueStatus;
use crate::app::preview_decode_residency::PreviewDecodeResidencyFamily;
use mondrian_core::types::{AssetId, ColorEngine, ColorSpace};
use mondrian_core::{Resolution, TimelineTime};
use mondrian_media::{DecodedVideoRange, DecodedVideoRangeContract, PreviewSourceColorContract};

fn test_media_key(label: &str) -> MediaPreviewKey {
    test_media_key_at(label, TimelineTime::new(1, 2).expect("exact source time"))
}

fn test_media_key_at(label: &str, source_time: TimelineTime) -> MediaPreviewKey {
    MediaPreviewKey::test_cpu(
        std::env::temp_dir().join(format!(
            "mondrian-preview-task-{label}-{}.mov",
            AssetId::new()
        )),
        MediaPreviewKey::test_fingerprint(17),
        source_time,
        Resolution { width: 320, height: 180 },
        PreviewSourceColorContract::automatic(ColorSpace::Rec709, DecodedVideoRange::Limited),
    )
}

fn with_source_time(mut key: MediaPreviewKey, source_time: TimelineTime) -> MediaPreviewKey {
    key.decode = PreviewDecodeKey::new(
        key.decode.source().clone(),
        mondrian_core::SourceSampleTarget::covering(source_time),
        key.decode.representation(),
        key.decode.source_color(),
    )
    .expect("valid replacement source time");
    key
}

#[test]
fn typed_decode_temporal_mismatch_preserves_retry_suppression_classification() {
    let error = MondrianError::DecodeTemporalMismatch {
        asset_id: "test.mov".to_owned(),
        access_mode: PreviewDecodeAccessMode::PlaybackCursor.as_str().to_owned(),
        requested_pts: Some(118),
        selected_pts: Some(100),
        selected_duration_pts: Some(5),
    };

    assert_eq!(
        media_preview_failure_reason(&error),
        MediaPreviewFailureReason::TemporalMismatch
    );
}

fn test_media_job(
    key: MediaPreviewKey,
    priority: MediaPreviewRequestPriority,
    access_mode: PreviewDecodeAccessMode,
) -> MediaPreviewJob {
    MediaPreviewJob {
        key,
        generation: 7,
        priority,
        access_mode,
        adaptive_hints: PreviewDecodeAdaptiveHints::default(),
        hardware_decode_request: PreviewHardwareDecodeRequest::Auto,
        hardware_decode_device_selector: None,
        enqueued_at: Instant::now(),
        deadline_at: None,
        demand_identity: None,
        execution_id: None,
        residency_work: None,
    }
}

fn test_decoded_frame_evidence(selected_pts: Option<i64>) -> MediaPreviewDecodedFrameEvidence {
    MediaPreviewDecodedFrameEvidence::CpuRgba8 {
        width: 320,
        height: 180,
        color_contract: DecodedRgbaFrameContract {
            source: PreviewSourceColorContract::new(
                ColorSpace::Rec709,
                DecodedVideoRangeContract::Automatic { probed_range: DecodedVideoRange::Limited },
            ),
            encoding: mondrian_media::DecodedRgbaEncoding::SourceEncodedRgb,
            alpha_mode: mondrian_media::DecodedRgbaAlphaMode::Straight,
            applied_matrix: mondrian_media::DecodedVideoMatrix::Bt709,
            applied_range: DecodedVideoRange::Limited,
        },
        selected_pts,
        surface_format: DecodedVideoSurfaceFormat::Yuv420p,
        sampling: DecodedVideoSampling {
            matrix: mondrian_media::DecodedVideoMatrix::Bt709,
            range: DecodedVideoRange::Limited,
            chroma_location: mondrian_media::DecodedVideoChromaLocation::Left,
            bit_depth: 8,
        },
        execution: PreviewDecodeExecutionPath::SoftwareCpu,
    }
}

#[test]
fn decoded_media_frame_identity_tracks_the_complete_media_preview_key() {
    let first = test_media_key("identity");
    let different_time = with_source_time(
        first.clone(),
        TimelineTime::new(3, 4).expect("different exact source time"),
    );
    let mut different_engine = first.clone();
    different_engine.engine = ColorEngine::Aces {
        preset: mondrian_core::AcesConfigPreset::StudioV4Aces2Ocio25,
    };

    let evidence = test_decoded_frame_evidence(Some(24_000));
    let first_identity = media_preview_frame_identity(&first, evidence);
    assert_ne!(
        first_identity,
        media_preview_frame_identity(&different_time, evidence),
        "source-local decode time is media-frame identity"
    );
    assert_ne!(
        first_identity,
        media_preview_frame_identity(&different_engine, evidence),
        "color-engine semantics are media-frame identity"
    );
}

#[test]
fn decoded_media_frame_identity_tracks_actual_selected_output_evidence() {
    let key = test_media_key("selected-output");
    let exact = media_preview_frame_identity(&key, test_decoded_frame_evidence(Some(24_000)));
    let approximate = media_preview_frame_identity(&key, test_decoded_frame_evidence(Some(18_000)));
    let different_payload_kind = media_preview_frame_identity(
        &key,
        MediaPreviewDecodedFrameEvidence::CpuRgbaF32 {
            width: 320,
            height: 180,
            color_contract: DecodedRgbaFrameContract {
                source: PreviewSourceColorContract::new(
                    ColorSpace::Rec709,
                    DecodedVideoRangeContract::Automatic {
                        probed_range: DecodedVideoRange::Limited,
                    },
                ),
                encoding: mondrian_media::DecodedRgbaEncoding::SourceLinearRgb,
                alpha_mode: mondrian_media::DecodedRgbaAlphaMode::Straight,
                applied_matrix: mondrian_media::DecodedVideoMatrix::Rgb,
                applied_range: DecodedVideoRange::Full,
            },
            selected_pts: Some(24_000),
            surface_format: DecodedVideoSurfaceFormat::Yuv420p10le,
            sampling: DecodedVideoSampling {
                matrix: mondrian_media::DecodedVideoMatrix::Bt709,
                range: DecodedVideoRange::Limited,
                chroma_location: mondrian_media::DecodedVideoChromaLocation::Left,
                bit_depth: 10,
            },
            execution: PreviewDecodeExecutionPath::SoftwareCpu,
        },
    );

    assert_ne!(
        exact, approximate,
        "the frame actually selected by scrub approximation is output identity"
    );
    assert_ne!(
        exact, different_payload_kind,
        "materialized payload and color contract are output identity"
    );
}

#[test]
fn missing_file_reports_failure_without_ui_state() {
    let key = test_media_key("missing");
    let result = decode_media_preview(
        test_media_job(
            key.clone(),
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::ScrubCursor,
        ),
        123,
        || false,
    );

    assert_eq!(result.key, key);
    assert!(result.frame.is_none());
    assert!(result.error.is_some());
    assert_eq!(result.generation, 7);
    assert_eq!(result.priority, MediaPreviewRequestPriority::Current);
    assert_eq!(result.queue_wait_us, 123);
    assert!(!result.canceled);
    assert!(result.cancel_reason.is_none());
    assert!(result.color_diagnostics.is_none());
    assert!(result.color_stage_diagnostics.is_none());
}

#[test]
fn cooperative_cancellation_is_not_a_media_failure() {
    let key = test_media_key("canceled");
    let result = decode_media_preview(
        test_media_job(
            key.clone(),
            MediaPreviewRequestPriority::Prefetch,
            PreviewDecodeAccessMode::PlaybackCursor,
        ),
        456,
        || true,
    );

    assert_eq!(result.key, key);
    assert!(result.frame.is_none());
    assert!(result.error.is_none());
    assert!(result.canceled);
    assert!(result.cancel_reason.is_none());
    assert_eq!(result.generation, 7);
    assert_eq!(result.priority, MediaPreviewRequestPriority::Prefetch);
    assert_eq!(result.queue_wait_us, 456);
    assert!(result.decode_diagnostics.is_none());
}

#[test]
fn worker_reports_queued_deadline_without_window_or_widget() {
    let (result_tx, result_rx) = mpsc::sync_channel(1);
    let scheduler = MediaPreviewScheduler::default();
    let (job_tx, job_rx) = scheduler.job_queue();
    let shutdown = Arc::new(PreviewShutdownSignal::default());
    let residency = Arc::new(PreviewDecodeResidencyCoordinator::new());
    let work_notifier = PreviewWorkNotifier::default();
    let work_watch = work_notifier.watch();
    let work_revision_before = work_watch.revision();
    residency.register_worker(MediaPreviewWorkerLane::Playback);
    let key = test_media_key("expired");
    let generation = scheduler.begin_generation();
    let mut job = test_media_job(
        key.clone(),
        MediaPreviewRequestPriority::Current,
        PreviewDecodeAccessMode::PlaybackCursor,
    );
    job.generation = generation;
    job.deadline_at = Some(Instant::now() - Duration::from_millis(1));

    assert_eq!(
        job_tx.enqueue(job),
        MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
    );

    let worker_scheduler = scheduler.clone();
    let worker_shutdown = Arc::clone(&shutdown);
    let worker_residency = Arc::clone(&residency);
    let worker = thread::spawn(move || {
        media_preview_worker(
            MediaPreviewWorkerLane::Playback,
            job_rx,
            result_tx,
            work_notifier,
            worker_scheduler,
            worker_shutdown,
            worker_residency,
            PreviewDecodeSessionContext::bootstrap(),
        );
    });
    let result = result_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("expired playback work should produce a canceled result");
    assert_ne!(
        work_watch.wait_for_change(work_revision_before, Duration::from_secs(1)),
        work_revision_before
    );

    assert_eq!(result.key, key);
    assert!(result.canceled);
    assert_eq!(
        result.cancel_reason,
        Some(MediaPreviewCancelReason::PlaybackDeadline)
    );
    assert_eq!(
        result.cancellation_phase,
        Some(MediaPreviewCancellationPhase::Queued)
    );
    assert_eq!(result.access_mode, PreviewDecodeAccessMode::PlaybackCursor);
    assert_eq!(result.priority, MediaPreviewRequestPriority::Current);
    assert_eq!(result.decode_elapsed_us, 0);
    assert_eq!(
        result
            .logical_cancellation_observed
            .map(|observation| observation.execution_elapsed_us),
        Some(0)
    );
    assert_eq!(
        job_tx.diagnostics().dropped_expired_playback_current_jobs,
        1
    );
    assert_eq!(job_tx.diagnostics().in_flight_jobs, 1);
    let execution_id = result.execution_id.expect("expired execution lease");
    assert!(scheduler.resolve_execution(execution_id, false).status.is_current());
    assert_eq!(job_tx.diagnostics().in_flight_jobs, 0);

    job_tx.close();
    worker.join().expect("worker should stop after queue close");
}

#[test]
fn worker_acknowledges_decoder_residency_retirement_after_bounded_wake() {
    let (result_tx, _result_rx) = mpsc::sync_channel(1);
    let scheduler = MediaPreviewScheduler::default();
    let (job_tx, job_rx) = scheduler.job_queue();
    let shutdown = Arc::new(PreviewShutdownSignal::default());
    let residency = Arc::new(PreviewDecodeResidencyCoordinator::new());
    residency.register_worker(MediaPreviewWorkerLane::Playback);
    let worker_scheduler = scheduler.clone();
    let worker_shutdown = Arc::clone(&shutdown);
    let worker_residency = Arc::clone(&residency);
    let worker = thread::spawn(move || {
        media_preview_worker(
            MediaPreviewWorkerLane::Playback,
            job_rx,
            result_tx,
            PreviewWorkNotifier::default(),
            worker_scheduler,
            worker_shutdown,
            worker_residency,
            PreviewDecodeSessionContext::bootstrap(),
        );
    });

    assert!(residency.activate(PreviewDecodeResidencyFamily::Playback));
    job_tx.interrupt_workers_for_lifecycle();
    assert!(residency.activate(PreviewDecodeResidencyFamily::Interactive));
    assert!(!residency.admits(PreviewDecodeAccessMode::RandomAccessStillFrame));
    job_tx.interrupt_workers_for_lifecycle();

    let deadline = Instant::now() + Duration::from_secs(1);
    while !residency.admits(PreviewDecodeAccessMode::RandomAccessStillFrame)
        && Instant::now() < deadline
    {
        thread::yield_now();
    }
    assert!(residency.admits(PreviewDecodeAccessMode::RandomAccessStillFrame));

    job_tx.close();
    worker.join().expect("worker should stop after queue close");
}

#[test]
fn full_result_queue_cannot_block_decoder_residency_retirement() {
    let (result_tx, _result_rx) = mpsc::sync_channel(0);
    let scheduler = MediaPreviewScheduler::default();
    let (job_tx, job_rx) = scheduler.job_queue();
    let shutdown = Arc::new(PreviewShutdownSignal::default());
    let residency = Arc::new(PreviewDecodeResidencyCoordinator::new());
    residency.register_worker(MediaPreviewWorkerLane::Playback);
    let worker_scheduler = scheduler.clone();
    let worker_shutdown = Arc::clone(&shutdown);
    let worker_residency = Arc::clone(&residency);
    let worker = thread::spawn(move || {
        media_preview_worker(
            MediaPreviewWorkerLane::Playback,
            job_rx,
            result_tx,
            PreviewWorkNotifier::default(),
            worker_scheduler,
            worker_shutdown,
            worker_residency,
            PreviewDecodeSessionContext::bootstrap(),
        );
    });

    let mut job = test_media_job(
        test_media_key("full-result-queue"),
        MediaPreviewRequestPriority::Current,
        PreviewDecodeAccessMode::PlaybackCursor,
    );
    job.generation = scheduler.begin_generation();
    job.deadline_at = Some(Instant::now() - Duration::from_millis(1));
    assert!(matches!(
        job_tx.enqueue(job),
        MediaPreviewJobEnqueueStatus::Enqueued { .. }
    ));

    let publication_deadline = Instant::now() + Duration::from_secs(1);
    while job_tx.diagnostics().in_flight_completed_jobs == 0
        && Instant::now() < publication_deadline
    {
        thread::yield_now();
    }
    assert_eq!(job_tx.diagnostics().in_flight_completed_jobs, 1);

    assert!(residency.activate(PreviewDecodeResidencyFamily::Interactive));
    let retirement_deadline = Instant::now() + Duration::from_secs(1);
    while !residency.admits(PreviewDecodeAccessMode::RandomAccessStillFrame)
        && Instant::now() < retirement_deadline
    {
        thread::yield_now();
    }
    assert!(residency.admits(PreviewDecodeAccessMode::RandomAccessStillFrame));
    assert_eq!(job_tx.diagnostics().in_flight_jobs, 0);
    assert_eq!(scheduler.pending_len(), 0);

    job_tx.close();
    worker.join().expect("worker should stop after queue close");
}

#[test]
fn worker_panic_evidence_is_utf8_safe_and_bounded() {
    let panic_payload = "解码器故障".repeat(MEDIA_PREVIEW_WORKER_PANIC_ERROR_MAX_BYTES);
    let error = bounded_media_preview_worker_panic_error(&panic_payload);
    assert!(error.len() <= MEDIA_PREVIEW_WORKER_PANIC_ERROR_MAX_BYTES);
    assert!(error.ends_with(MEDIA_PREVIEW_WORKER_PANIC_TRUNCATION_SUFFIX));

    let non_string_payload = 17_u64;
    let fallback = bounded_media_preview_worker_panic_error(&non_string_payload);
    assert!(fallback.contains("panic payload is not a string"));
    assert!(fallback.len() <= MEDIA_PREVIEW_WORKER_PANIC_ERROR_MAX_BYTES);
}

#[test]
fn armed_execution_guard_fails_exact_dequeued_binding() {
    let scheduler = MediaPreviewScheduler::default();
    let (job_tx, job_rx) = scheduler.job_queue();
    let generation = scheduler.begin_generation();
    let mut job = test_media_job(
        test_media_key("execution-guard"),
        MediaPreviewRequestPriority::Current,
        PreviewDecodeAccessMode::PlaybackCursor,
    );
    job.generation = generation;
    assert!(matches!(
        job_tx.enqueue(job),
        MediaPreviewJobEnqueueStatus::Enqueued { .. }
    ));

    let dequeued = job_rx
        .recv_for_worker(MediaPreviewWorkerLane::Playback)
        .expect("queued execution");
    let execution_id = dequeued.execution_id.expect("dequeued execution identity");
    assert_eq!(job_tx.diagnostics().in_flight_jobs, 1);
    drop(MediaPreviewExecutionGuard::new(
        scheduler.clone(),
        execution_id,
    ));

    assert_eq!(job_tx.diagnostics().in_flight_jobs, 0);
    assert_eq!(scheduler.pending_len(), 0);
    job_tx.close();
}

#[test]
fn worker_contains_job_panic_rebuilds_context_and_continues_publication() {
    let (result_tx, result_rx) = mpsc::sync_channel(2);
    let scheduler = MediaPreviewScheduler::default();
    let (job_tx, job_rx) = scheduler.job_queue();
    let shutdown = Arc::new(PreviewShutdownSignal::default());
    let residency = Arc::new(PreviewDecodeResidencyCoordinator::new());
    let work_notifier = PreviewWorkNotifier::default();
    let work_watch = work_notifier.watch();
    residency.register_worker(MediaPreviewWorkerLane::Playback);

    let generation = scheduler.begin_generation();
    let first_key = test_media_key("panicked-job");
    let mut first_job = test_media_job(
        first_key.clone(),
        MediaPreviewRequestPriority::Current,
        PreviewDecodeAccessMode::PlaybackCursor,
    );
    first_job.generation = generation;
    assert!(matches!(
        job_tx.enqueue(first_job),
        MediaPreviewJobEnqueueStatus::Enqueued { .. }
    ));

    let invocations = Arc::new(AtomicUsize::new(0));
    let observed_recovery_revision = Arc::new(AtomicU64::new(u64::MAX));
    let worker_invocations = Arc::clone(&invocations);
    let worker_recovery_revision = Arc::clone(&observed_recovery_revision);
    let worker_scheduler = scheduler.clone();
    let worker_shutdown = Arc::clone(&shutdown);
    let worker_residency = Arc::clone(&residency);
    let first_work_revision = work_watch.revision();
    let worker = thread::spawn(move || {
        media_preview_worker_with_decoder(
            MediaPreviewWorkerLane::Playback,
            job_rx,
            result_tx,
            work_notifier,
            worker_scheduler,
            worker_shutdown,
            worker_residency,
            PreviewDecodeSessionContext::bootstrap(),
            move |job, queue_wait_us, decode_context, recovery_revision, should_cancel| {
                let invocation = worker_invocations.fetch_add(1, AtomicOrdering::AcqRel);
                if invocation == 0 {
                    assert_eq!(recovery_revision, 0);
                    std::panic::panic_any(
                        "解码器故障".repeat(MEDIA_PREVIEW_WORKER_PANIC_ERROR_MAX_BYTES),
                    );
                }
                worker_recovery_revision.store(recovery_revision, AtomicOrdering::Release);
                decode_media_preview_with_context(job, queue_wait_us, decode_context, should_cancel)
            },
        );
    });

    let first_result = result_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("contained panic should publish a typed failure");
    assert_ne!(
        work_watch.wait_for_change(first_work_revision, Duration::from_secs(1)),
        first_work_revision
    );
    assert_eq!(first_result.key, first_key);
    assert_eq!(
        first_result.failure_reason,
        Some(MediaPreviewFailureReason::WorkerPanicked)
    );
    assert!(!first_result.canceled);
    assert!(first_result.frame.is_none());
    assert!(first_result.residency_work.is_none());
    let panic_error = first_result
        .error
        .as_deref()
        .expect("panic failure should retain bounded diagnostic evidence");
    assert!(panic_error.len() <= MEDIA_PREVIEW_WORKER_PANIC_ERROR_MAX_BYTES);
    assert!(panic_error.ends_with(MEDIA_PREVIEW_WORKER_PANIC_TRUNCATION_SUFFIX));
    let first_execution_id = first_result.execution_id.expect("panic execution identity");
    assert!(scheduler.resolve_execution(first_execution_id, false).status.is_current());
    assert_eq!(job_tx.diagnostics().in_flight_jobs, 0);

    let second_key = test_media_key("post-panic-job");
    let mut second_job = test_media_job(
        second_key.clone(),
        MediaPreviewRequestPriority::Current,
        PreviewDecodeAccessMode::PlaybackCursor,
    );
    second_job.generation = generation;
    assert!(matches!(
        job_tx.enqueue(second_job),
        MediaPreviewJobEnqueueStatus::Enqueued { .. }
    ));

    let second_result = result_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("the same worker should execute the next job");
    assert_eq!(second_result.key, second_key);
    assert_eq!(
        second_result.failure_reason,
        Some(MediaPreviewFailureReason::DecodeError)
    );
    assert_eq!(
        observed_recovery_revision.load(AtomicOrdering::Acquire),
        1,
        "the next job must execute in a context rebuilt after the panic"
    );
    let second_execution_id = second_result.execution_id.expect("second execution identity");
    assert!(scheduler.resolve_execution(second_execution_id, false).status.is_current());
    assert_eq!(invocations.load(AtomicOrdering::Acquire), 2);
    assert_eq!(job_tx.diagnostics().in_flight_jobs, 0);

    job_tx.close();
    worker.join().expect("contained panic must not terminate the worker");
}

#[derive(Clone, Copy)]
struct FixedRuntimeClock;

impl mondrian_playback::MonotonicRuntimeClock for FixedRuntimeClock {
    fn now(&self) -> mondrian_playback::MonotonicTimestamp {
        mondrian_playback::MonotonicTimestamp::ZERO
    }
}

#[test]
fn active_persistent_observer_wakes_and_joins_when_scheduler_closes() {
    let scheduler = MediaPreviewScheduler::with_clock_for_test(FixedRuntimeClock);
    let (job_tx, job_rx) = scheduler.job_queue();
    let generation = scheduler.begin_generation();
    let mut job = test_media_job(
        test_media_key("active-observer-shutdown"),
        MediaPreviewRequestPriority::Current,
        PreviewDecodeAccessMode::PlaybackCursor,
    );
    job.generation = generation;
    assert!(matches!(
        job_tx.enqueue(job),
        MediaPreviewJobEnqueueStatus::Enqueued { .. }
    ));
    let execution_id = job_rx
        .recv_for_worker(MediaPreviewWorkerLane::Playback)
        .expect("dequeued observer execution")
        .execution_id
        .expect("observer execution identity");

    let (ready_tx, ready_rx) = mpsc::sync_channel(0);
    let (terminal_tx, terminal_rx) = mpsc::sync_channel(0);
    let observer_scheduler = scheduler.clone();
    let observer_worker = thread::spawn(move || {
        let mut observer = MediaPreviewCancellationObserver::start(
            MediaPreviewWorkerLane::Playback,
            observer_scheduler,
        )
        .expect("persistent observer thread");
        let observation = observer
            .observe(
                execution_id,
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::PlaybackCursor,
            )
            .expect("active observation");
        ready_tx.send(observation).expect("publish active observation handle");
        let terminal = observer.finish(execution_id);
        observer.shutdown_and_join();
        terminal_tx.send(terminal).expect("publish observer terminal state");
    });

    let observation = ready_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("observer accepted active execution");
    job_tx.close();
    assert_eq!(
        terminal_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("close must wake the 24-hour observer wait"),
        Ok(MediaPreviewCancellationObserverTerminal::Canceled)
    );
    let snapshot = observation.snapshot();
    assert_eq!(snapshot.reason, Some(MediaPreviewCancelReason::Shutdown));
    assert_eq!(
        snapshot
            .logical_cancellation_observed
            .and_then(|observed| observed.request_elapsed_us),
        Some(0)
    );
    observer_worker
        .join()
        .expect("observer wrapper and persistent thread must terminate");
}

#[test]
fn persistent_observer_rejects_a_late_result_from_a_blocking_decoder() {
    let (result_tx, result_rx) = mpsc::sync_channel(1);
    let scheduler = MediaPreviewScheduler::with_clock_for_test(FixedRuntimeClock);
    let (job_tx, job_rx) = scheduler.job_queue();
    let shutdown = Arc::new(PreviewShutdownSignal::default());
    let residency = Arc::new(PreviewDecodeResidencyCoordinator::new());
    residency.register_worker(MediaPreviewWorkerLane::Playback);
    let generation = scheduler.begin_generation();
    let mut job = test_media_job(
        test_media_key("blocking-cancel-observer"),
        MediaPreviewRequestPriority::Current,
        PreviewDecodeAccessMode::PlaybackCursor,
    );
    job.generation = generation;
    assert!(matches!(
        job_tx.enqueue(job),
        MediaPreviewJobEnqueueStatus::Enqueued { .. }
    ));

    let (started_tx, started_rx) = mpsc::sync_channel(0);
    let (release_tx, release_rx) = mpsc::sync_channel(0);
    let worker_scheduler = scheduler.clone();
    let worker_shutdown = Arc::clone(&shutdown);
    let worker_residency = Arc::clone(&residency);
    let worker = thread::spawn(move || {
        media_preview_worker_with_decoder(
            MediaPreviewWorkerLane::Playback,
            job_rx,
            result_tx,
            PreviewWorkNotifier::default(),
            worker_scheduler,
            worker_shutdown,
            worker_residency,
            PreviewDecodeSessionContext::bootstrap(),
            move |job, queue_wait_us, decode_context, _recovery_revision, _should_cancel| {
                started_tx.send(()).expect("announce blocking decoder");
                release_rx.recv().expect("release blocking decoder");
                decode_media_preview_with_context(job, queue_wait_us, decode_context, || false)
            },
        );
    });

    started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("worker entered blocking decoder");
    scheduler.cancel_all();
    release_tx.send(()).expect("release late decoder result");
    let result = result_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("logical cancellation should publish a terminal result");

    assert!(result.canceled);
    assert_eq!(
        result.cancel_reason,
        Some(MediaPreviewCancelReason::Obsolete)
    );
    assert_eq!(
        result
            .logical_cancellation_observed
            .and_then(|observation| observation.request_elapsed_us),
        Some(0)
    );
    assert!(result.frame.is_none());
    assert!(result.error.is_none());
    let execution_id = result.execution_id.expect("execution identity");
    assert!(!scheduler.resolve_execution(execution_id, true).status.is_current());

    job_tx.close();
    worker.join().expect("worker and observer should join cleanly");
}
