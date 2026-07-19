use std::thread;

use mondrian_core::timeline_data::AlphaInterpretation;
use mondrian_core::types::{AssetId, ColorEngine, ColorSpace};
use mondrian_core::WorkingColorSpace;
use mondrian_media::{DecodedVideoRange, DecodedVideoRangeContract};

use super::*;
use crate::app::preview_access_mode::MediaPreviewJobEnqueueStatus;

fn test_media_key(label: &str) -> MediaPreviewKey {
    MediaPreviewKey {
        asset_id: AssetId::new(),
        path: std::env::temp_dir().join(format!(
            "mondrian-preview-task-{label}-{}.mov",
            AssetId::new()
        )),
        fingerprint: None,
        source_frame: 12,
        source_micros: 500_000,
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
        source_secs: 0.5,
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
    let worker = thread::spawn(move || {
        media_preview_worker(
            MediaPreviewWorkerLane::Playback,
            job_rx,
            result_tx,
            worker_scheduler,
            worker_shutdown,
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
