use std::net::TcpListener;
use std::thread;

use mondrian_core::timeline_data::AlphaInterpretation;
use mondrian_core::types::{AssetId, ColorEngine, ColorSpace};
use mondrian_core::{TimelineTime, WorkingColorSpace};
use mondrian_media::{DecodedVideoRange, DecodedVideoRangeContract};
use mondrian_media::{
    MediaFileFingerprint, PreviewDecodeCancellation, PreviewDecodeCancellationCheckpoint,
    PreviewDecodeCancellationSource,
};

use super::*;
use crate::app::preview_access_mode::{MediaPreviewJobEnqueueStatus, MediaPreviewRequestStatus};
use crate::app::preview_decode_residency::PreviewDecodeResidencyFamily;

fn test_media_key(label: &str) -> MediaPreviewKey {
    MediaPreviewKey {
        asset_id: AssetId::new(),
        path: std::env::temp_dir().join(format!(
            "mondrian-preview-task-{label}-{}.mov",
            AssetId::new()
        )),
        fingerprint: None,
        source_time: TimelineTime::new(1, 2).expect("exact source time"),
        target_width: 320,
        target_height: 180,
        source_width: 320,
        source_height: 180,
        input_color_space: ColorSpace::Rec709,
        input_video_range: DecodedVideoRangeContract::Automatic {
            probed_range: DecodedVideoRange::Limited,
        },
        native_surface_hint: None,
        source_has_alpha: false,
        alpha_interpretation: AlphaInterpretation::Straight,
        working_color_space: WorkingColorSpace::LinearRec709,
        tone_map: false,
        engine: ColorEngine::mondrian_standard(),
        ocio_generation: mondrian_core::ocio_config_generation(),
    }
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
    }
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
    let (result_tx, result_rx) = mpsc::channel();
    let scheduler = MediaPreviewScheduler::default();
    let (job_tx, job_rx) = scheduler.job_queue();
    let shutdown = Arc::new(PreviewShutdownSignal::default());
    let residency = Arc::new(PreviewDecodeResidencyCoordinator::new());
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
            worker_scheduler,
            worker_shutdown,
            worker_residency,
        );
    });
    let result = result_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("expired playback work should produce a canceled result");

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
    assert_eq!(result.cancel_observed_elapsed_us, Some(0));
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
    let (result_tx, _result_rx) = mpsc::channel();
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
            worker_scheduler,
            worker_shutdown,
            worker_residency,
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
fn worker_interrupts_blocked_ffmpeg_input_and_publishes_typed_evidence() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind local stall server");
    let address = listener.local_addr().expect("stall server address");
    let (accepted_tx, accepted_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept FFmpeg HTTP connection");
        accepted_tx.send(()).expect("publish accepted connection");
        let _stream = stream;
        let _ = release_rx.recv_timeout(Duration::from_secs(5));
    });

    let scheduler = MediaPreviewScheduler::default();
    let (job_tx, job_rx) = scheduler.job_queue();
    let (result_tx, result_rx) = mpsc::channel();
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
            worker_scheduler,
            worker_shutdown,
            worker_residency,
        );
    });

    let mut key = test_media_key("blocked-input");
    key.path = format!("http://{address}/blocked-open.mp4").into();
    key.fingerprint = Some(MediaFileFingerprint::default());
    key.source_time = TimelineTime::ZERO;
    let generation = scheduler.begin_generation();
    assert!(matches!(
        scheduler.request(
            key.clone(),
            generation,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::PlaybackCursor,
        ),
        MediaPreviewRequestStatus::Scheduled { .. }
    ));

    accepted_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("worker must enter the controlled blocking read");
    let requested_at = Instant::now();
    scheduler.cancel(&key);
    let result = result_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("FFmpeg interrupt must return a terminal worker result");
    let return_latency = requested_at.elapsed();

    assert!(result.canceled);
    assert_eq!(result.key, key);
    assert_eq!(
        result.decode_cancellation,
        Some(PreviewDecodeCancellation {
            checkpoint: PreviewDecodeCancellationCheckpoint::InputOpen,
            source: PreviewDecodeCancellationSource::FfmpegIoInterrupt,
        })
    );
    assert!(result.cancel_observed_elapsed_us.is_some());
    assert!(result.cancel_request_to_observed_us.is_some());
    assert!(
        return_latency <= Duration::from_millis(500),
        "blocked input open returned too late after scheduler cancellation: {return_latency:?}"
    );

    let _ = release_tx.send(());
    server.join().expect("stall server must return");
    job_tx.close();
    worker.join().expect("worker must stop after queue close");
}
