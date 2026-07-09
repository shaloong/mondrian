//! Viewer preview service for the app UI host.
//!
//! The service owns render-plan interpretation and preview-frame cache keys.
//! Panels stay read-only and only consume renderer-ready viewer frame content.

use std::cell::{Cell, RefCell};
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use mondrian_assets::AssetKind;
use mondrian_core::display_contract::{DisplayOutputSnapshot, MonitorProfileStatus};
use mondrian_core::timeline_data::AssetMediaInterpretation;
#[cfg(test)]
use mondrian_core::types::ColorEngine;
use mondrian_core::types::{AssetId, BlendMode, ColorSpace, Rational, SequenceId};
use mondrian_core::MondrianError;
use mondrian_effects::{CompiledEffectGraph, EffectCachePolicy};
use mondrian_media::{
    decode_preview_rgba_scaled_cancellable, preview_decode_cpu_budget, DecodedFrameResidency,
    DecodedGpuFrameHandleKind, DecodedVideoSurfaceFormat, HwAccelBackend, PreviewDecodeAccessMode,
    PreviewDecodeAdaptiveHints, PreviewDecodeCpuBudget, PreviewDecodeDiagnostics,
    PreviewDecodeOutcome, PreviewDecodePath, PreviewDecodeRgbaRequest, PreviewDecodeSeekStrategy,
    PreviewDecodeStageDurations, PreviewDecodeThreadingKind, PreviewFileFingerprint,
    PreviewHardwareDecodeBlocker, PreviewHardwareDecodeDecision, PreviewHardwareDecodeRequest,
    PreviewScrubAdaptiveClass, PreviewSeekIndexSource, VideoColorDiagnostic,
    VideoColorDiagnosticIssueSummary,
};
#[cfg(test)]
use mondrian_renderer::TimelineCompositeColorPath;
use mondrian_renderer::{
    color_report_vocab, composite_timeline_elements_color_frame_with_diagnostics,
    evaluate_timeline_render_plan, execute_cpu_input_stage, execute_cpu_output_boundary_rgba8,
    CpuColorFrame, CpuEncodedColorFrame, GpuCompositingBlockerReason, GpuCompositingDiagnostics,
    RenderColorStageDiagnostics, RenderColorStageGpuBlockerBreakdown,
    RenderColorTransformDiagnostics, RenderColorTransformDirection, RenderInputTransform,
    RenderOutputColorBoundary, TimelineAdjustmentLayer, TimelineCompositeColorPathSummary,
    TimelineCompositeDiagnostics, TimelineCompositeElement, TimelineCompositeLegacyBreakdown,
    TimelineCompositeOptions, TimelineCompositeScratch, TimelineEvaluationRequest,
    TimelineMediaLayer, TimelineRenderPlanElement, TimelineSolidColorLayer,
};
use mondrian_timeline::sequence::{
    ColorContext, InputColorResolution, InputColorResolutionSource,
    InputColorResolutionSourceCounts, MissingColorMetadataPolicy, Sequence,
    MAX_NESTED_SEQUENCE_RENDER_DEPTH,
};
use mondrian_ui_widgets::{ViewerExternalTextureFrame, ViewerFrameContent, ViewerFrameImage};

use crate::app::proxy_generation::request_proxy_generation;
use crate::app::AppState;
use crate::app_ui::panels::{
    ViewerColorPipelineStatus, ViewerPreviewColorRejectionModel, ViewerPreviewSource,
    ViewerPreviewState,
};
use crate::app_ui::preview_access_mode::{
    media_preview_access_mode_for_intent, media_preview_job_queue,
    media_preview_viewer_access_intent, media_preview_worker_count, media_preview_worker_lane,
    MediaPreviewJob, MediaPreviewJobEnqueueStatus, MediaPreviewJobQueueDiagnostics,
    MediaPreviewJobQueueReceive, MediaPreviewJobQueueReceiver, MediaPreviewJobQueueSender,
    MediaPreviewKey, MediaPreviewRequestPriority, MediaPreviewRequestStatus, MediaPreviewScheduler,
    MediaPreviewSchedulerDiagnostics, MediaPreviewWorkerLane, MEDIA_PREVIEW_JOB_QUEUE_CAPACITY,
};
use crate::app_ui::preview_scale::normalize_preview_resolution_scale;

const MEDIA_PREVIEW_CACHE_CAPACITY: usize = 96;
const MEDIA_PREVIEW_FAILURE_CACHE_CAPACITY: usize = MEDIA_PREVIEW_CACHE_CAPACITY * 2;
const VIEWER_PREVIEW_FRAME_CACHE_CAPACITY: usize = 48;
const MEDIA_PREVIEW_FORWARD_PREFETCH_HORIZON_US: u64 = 80_000;
const MEDIA_PREVIEW_FORWARD_PREFETCH_MIN_FRAMES: usize = 1;
const MEDIA_PREVIEW_FORWARD_PREFETCH_MAX_FRAMES: usize = 6;
const MEDIA_PREVIEW_PREFETCH_DECODE_BUDGET_US: u64 = 50_000;
const MEDIA_PREVIEW_PLAYBACK_CURRENT_MIN_DEADLINE_US: u64 = 8_000;
const MEDIA_PREVIEW_PLAYBACK_CURRENT_MAX_DEADLINE_US: u64 = 50_000;
const MEDIA_PREVIEW_PLAYBACK_BUFFERING_STALL_TIMEOUT_US: u64 = 250_000;
const MEDIA_PREVIEW_PLAYBACK_PRESSURE_LATE_STREAK_THRESHOLD: u64 = 2;
const MEDIA_PREVIEW_MAX_COMPLETED_RESULTS_PER_POLL: usize = 8;
const MEDIA_PREVIEW_COMPLETED_RESULTS_POLL_BUDGET_US: u64 = 2_000;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AppUiPreviewPollOutcome {
    /// A decoded frame or terminal decode failure changed visible viewer state.
    pub visible_change: bool,
    /// More completed decode results should be drained on a follow-up event-loop tick.
    pub needs_follow_up_poll: bool,
}

/// Host-owned preview renderer used by the app UI viewer panel.
///
/// This first path renders solid-color render-plan elements through the shared
/// renderer compositor. Media and nested-sequence decode can attach here without
/// changing panel models or widget APIs.
pub struct AppUiPreviewService {
    jobs: MediaPreviewJobQueueSender,
    results: RefCell<mpsc::Receiver<MediaPreviewResult>>,
    workers: RefCell<Vec<JoinHandle<()>>>,
    shutdown: Arc<AtomicBool>,
    media_cache: RefCell<MediaPreviewCache>,
    media_failures: RefCell<MediaPreviewFailureCache>,
    requested_proxy_generations: RefCell<HashSet<PreviewProxyGenerationRequestKey>>,
    scrub_adaptation: RefCell<PreviewScrubAdaptationState>,
    viewer_frame_cache: RefCell<ViewerPreviewFrameCache>,
    external_viewer_frame: RefCell<Option<ScopedExternalViewerFrame>>,
    next_gpu_preview_candidate_id: Cell<u64>,
    scheduler: MediaPreviewScheduler,
    worker_activity: Arc<PreviewWorkerActivity>,
    scratch: RefCell<TimelineCompositeScratch>,
    current_generation: Cell<u64>,
    current_frame_pending: Cell<bool>,
    last_ready_frame: RefCell<Option<ScopedViewerFrame>>,
    last_color_rejection: RefCell<Option<AppUiPreviewColorRejection>>,
    display_snapshot: RefCell<Option<DisplayOutputSnapshot>>,
    last_generation_key: RefCell<Option<ViewerPreviewGenerationKey>>,
    decode_cpu_budget: PreviewDecodeCpuBudget,
    decode_worker_count: usize,
    metrics: AppUiPreviewMetrics,
}

impl AppUiPreviewService {
    /// Create an empty preview service.
    pub fn new() -> Self {
        let decode_cpu_budget = preview_decode_cpu_budget();
        let worker_count = media_preview_worker_count().min(decode_cpu_budget.preview_worker_count);
        Self::with_worker_count(decode_cpu_budget, worker_count)
    }

    #[cfg(test)]
    pub(crate) fn new_without_workers_for_test() -> Self {
        Self::with_worker_count(preview_decode_cpu_budget(), 0)
    }

    fn with_worker_count(decode_cpu_budget: PreviewDecodeCpuBudget, worker_count: usize) -> Self {
        let (job_tx, job_rx) = media_preview_job_queue(MEDIA_PREVIEW_JOB_QUEUE_CAPACITY);
        let (result_tx, result_rx) = mpsc::channel::<MediaPreviewResult>();
        let scheduler = MediaPreviewScheduler::default();
        let worker_activity = Arc::new(PreviewWorkerActivity::default());
        let shutdown = Arc::new(AtomicBool::new(false));
        let mut decode_worker_count = 0;
        let mut workers = Vec::new();
        for worker_index in 0..worker_count {
            let worker_jobs = job_rx.clone();
            let worker_results = result_tx.clone();
            let worker_scheduler = scheduler.clone();
            let worker_activity = Arc::clone(&worker_activity);
            let worker_shutdown = Arc::clone(&shutdown);
            let worker_lane = media_preview_worker_lane(worker_index, worker_count);
            match std::thread::Builder::new()
                .name(format!("mondrian-ui-viewer-preview-{worker_index}"))
                .spawn(move || {
                    media_preview_worker(
                        worker_lane,
                        worker_jobs,
                        worker_results,
                        worker_scheduler,
                        worker_activity,
                        worker_shutdown,
                    )
                }) {
                Ok(handle) => {
                    workers.push(handle);
                    decode_worker_count += 1;
                }
                Err(err) => {
                    tracing::warn!(
                        worker_index,
                        "failed to start app UI viewer preview worker: {err}"
                    );
                }
            }
        }

        Self {
            jobs: job_tx,
            results: RefCell::new(result_rx),
            workers: RefCell::new(workers),
            shutdown,
            media_cache: RefCell::new(MediaPreviewCache::new(MEDIA_PREVIEW_CACHE_CAPACITY)),
            media_failures: RefCell::new(MediaPreviewFailureCache::new(
                MEDIA_PREVIEW_FAILURE_CACHE_CAPACITY,
            )),
            requested_proxy_generations: RefCell::new(HashSet::new()),
            scrub_adaptation: RefCell::new(PreviewScrubAdaptationState::default()),
            viewer_frame_cache: RefCell::new(ViewerPreviewFrameCache::new(
                VIEWER_PREVIEW_FRAME_CACHE_CAPACITY,
            )),
            external_viewer_frame: RefCell::new(None),
            next_gpu_preview_candidate_id: Cell::new(0),
            scheduler,
            worker_activity,
            scratch: RefCell::new(TimelineCompositeScratch::default()),
            current_generation: Cell::new(0),
            current_frame_pending: Cell::new(false),
            last_ready_frame: RefCell::new(None),
            last_color_rejection: RefCell::new(None),
            display_snapshot: RefCell::new(None),
            last_generation_key: RefCell::new(None),
            decode_cpu_budget,
            decode_worker_count,
            metrics: AppUiPreviewMetrics::default(),
        }
    }

    #[cfg(test)]
    pub(crate) fn seed_pending_preview_work_for_test(&self) {
        self.seed_pending_preview_work_with_access_mode_for_test(
            PreviewDecodeAccessMode::ScrubCursor,
        );
    }

    #[cfg(test)]
    pub(crate) fn seed_pending_playback_current_preview_work_for_test(&self) {
        self.seed_pending_preview_work_with_access_mode_for_test(
            PreviewDecodeAccessMode::PlaybackCursor,
        );
    }

    #[cfg(test)]
    fn seed_pending_preview_work_with_access_mode_for_test(
        &self,
        access_mode: PreviewDecodeAccessMode,
    ) {
        let key = MediaPreviewKey {
            asset_id: AssetId::new(),
            path: PathBuf::from("E:/media/pending-preview.mov"),
            fingerprint: None,
            source_frame: 0,
            source_micros: 0,
            target_width: 1920,
            target_height: 1080,
            input_color_space: ColorSpace::Srgb,
            working_color_space: ColorSpace::Srgb,
            tone_map: false,
            engine: ColorEngine::MondrianSmart,
        };
        let generation = self.scheduler.begin_generation();
        let _ = self.scheduler.request(
            key.clone(),
            generation,
            MediaPreviewRequestPriority::Current,
            access_mode,
        );
        let _ = self.jobs.enqueue(MediaPreviewJob {
            key,
            source_secs: 0.0,
            generation,
            priority: MediaPreviewRequestPriority::Current,
            access_mode,
            adaptive_hints: PreviewDecodeAdaptiveHints::default(),
            enqueued_at: Instant::now(),
            deadline_at: None,
        });
        self.current_generation.set(generation);
        self.current_frame_pending.set(true);
    }

    /// Return a point-in-time snapshot of preview scheduling and cache health.
    pub fn diagnostics(&self) -> AppUiPreviewDiagnostics {
        let scheduler = self.scheduler.diagnostics();
        AppUiPreviewDiagnostics {
            render_requests: self.metrics.render_requests.get(),
            ready_frames: self.metrics.ready_frames.get(),
            loading_frames: self.metrics.loading_frames.get(),
            stale_frames: self.metrics.stale_frames.get(),
            unavailable_frames: self.metrics.unavailable_frames.get(),
            playback_current_stalled_expirations: self
                .metrics
                .playback_current_stalled_expirations
                .get(),
            gpu_preview_candidate_requests: self.metrics.gpu_preview_candidate_requests.get(),
            gpu_preview_candidate_ready: self.metrics.gpu_preview_candidate_ready.get(),
            gpu_preview_candidate_current: self.metrics.gpu_preview_candidate_current.get(),
            gpu_preview_candidate_loading: self.metrics.gpu_preview_candidate_loading.get(),
            gpu_preview_candidate_unavailable: self.metrics.gpu_preview_candidate_unavailable.get(),
            gpu_preview_candidate_pixels: self.metrics.gpu_preview_candidate_pixels.get(),
            gpu_preview_external_frames_registered: self
                .metrics
                .gpu_preview_external_frames_registered
                .get(),
            gpu_preview_external_frames_rejected: self
                .metrics
                .gpu_preview_external_frames_rejected
                .get(),
            gpu_preview_external_frames_cleared: self
                .metrics
                .gpu_preview_external_frames_cleared
                .get(),
            input_color_resolution_override: self.metrics.input_color_resolution_override.get(),
            input_color_resolution_detected_metadata: self
                .metrics
                .input_color_resolution_detected_metadata
                .get(),
            input_color_resolution_missing_assume_rec709: self
                .metrics
                .input_color_resolution_missing_assume_rec709
                .get(),
            input_color_resolution_missing_assume_working: self
                .metrics
                .input_color_resolution_missing_assume_working
                .get(),
            input_color_resolution_missing_rejected: self
                .metrics
                .input_color_resolution_missing_rejected
                .get(),
            input_color_resolution_data_texture: self
                .metrics
                .input_color_resolution_data_texture
                .get(),
            media_proxy_path_hits: self.metrics.media_proxy_path_hits.get(),
            media_proxy_path_misses: self.metrics.media_proxy_path_misses.get(),
            media_proxy_path_stale: self.metrics.media_proxy_path_stale.get(),
            media_proxy_path_bypasses: self.metrics.media_proxy_path_bypasses.get(),
            media_proxy_generation_requests: self.metrics.media_proxy_generation_requests.get(),
            media_proxy_generation_request_dedupes: self
                .metrics
                .media_proxy_generation_request_dedupes
                .get(),
            scrub_adaptive_normal_requests: self.metrics.scrub_adaptive_normal_requests.get(),
            scrub_adaptive_hot_region_requests: self
                .metrics
                .scrub_adaptive_hot_region_requests
                .get(),
            scrub_adaptive_slow_latency_requests: self
                .metrics
                .scrub_adaptive_slow_latency_requests
                .get(),
            scrub_adaptive_recovery_requests: self.metrics.scrub_adaptive_recovery_requests.get(),
            media_cache_hits: self.metrics.media_cache_hits.get(),
            media_cache_misses: self.metrics.media_cache_misses.get(),
            media_failure_hits: self.metrics.media_failure_hits.get(),
            decode_cpu_budget: self.decode_cpu_budget,
            decode_worker_count: self.decode_worker_count,
            decode_successes: self.metrics.decode_successes.get(),
            decode_failures: self.metrics.decode_failures.get(),
            decode_timeout_failures: self.metrics.decode_timeout_failures.get(),
            decode_budget_exhausted_failures: self.metrics.decode_budget_exhausted_failures.get(),
            decode_canceled_jobs: self.metrics.decode_canceled_jobs.get(),
            decode_canceled_shutdown_jobs: self.metrics.decode_canceled_shutdown_jobs.get(),
            decode_canceled_obsolete_jobs: self.metrics.decode_canceled_obsolete_jobs.get(),
            decode_canceled_prefetch_deadline_jobs: self
                .metrics
                .decode_canceled_prefetch_deadline_jobs
                .get(),
            decode_canceled_playback_deadline_jobs: self
                .metrics
                .decode_canceled_playback_deadline_jobs
                .get(),
            decode_canceled_prefetch_preempted_jobs: self
                .metrics
                .decode_canceled_prefetch_preempted_jobs
                .get(),
            decode_canceled_still_preempted_jobs: self
                .metrics
                .decode_canceled_still_preempted_jobs
                .get(),
            decode_canceled_unknown_jobs: self.metrics.decode_canceled_unknown_jobs.get(),
            decode_canceled_total_duration_us: self.metrics.decode_canceled_total_duration_us.get(),
            decode_canceled_max_duration_us: self.metrics.decode_canceled_max_duration_us.get(),
            decode_canceled_last_duration_us: self.metrics.decode_canceled_last_duration_us.get(),
            decode_canceled_return_latency_total_us: self
                .metrics
                .decode_canceled_return_latency_total_us
                .get(),
            decode_canceled_return_latency_max_us: self
                .metrics
                .decode_canceled_return_latency_max_us
                .get(),
            decode_canceled_return_latency_last_us: self
                .metrics
                .decode_canceled_return_latency_last_us
                .get(),
            decode_in_process_cpu_rgba_frames: self.metrics.decode_in_process_cpu_rgba_frames.get(),
            decode_external_ffmpeg_cpu_rgba_frames: self
                .metrics
                .decode_external_ffmpeg_cpu_rgba_frames
                .get(),
            decode_playback_session_ring_hit_frames: self
                .metrics
                .decode_playback_session_ring_hit_frames
                .get(),
            decode_cache_hit_frames: self.metrics.decode_cache_hit_frames.get(),
            decode_playback_cursor_frames: self.metrics.decode_playback_cursor_frames.get(),
            decode_scrub_cursor_frames: self.metrics.decode_scrub_cursor_frames.get(),
            decode_random_access_still_frames: self.metrics.decode_random_access_still_frames.get(),
            decode_canceled_playback_cursor_jobs: self
                .metrics
                .decode_canceled_playback_cursor_jobs
                .get(),
            decode_canceled_scrub_cursor_jobs: self.metrics.decode_canceled_scrub_cursor_jobs.get(),
            decode_canceled_random_access_still_jobs: self
                .metrics
                .decode_canceled_random_access_still_jobs
                .get(),
            decode_total_duration_us: self.metrics.decode_total_duration_us.get(),
            decode_max_duration_us: self.metrics.decode_max_duration_us.get(),
            decode_last_duration_us: self.metrics.decode_last_duration_us.get(),
            decode_queue_wait_total_us: self.metrics.decode_queue_wait_total_us.get(),
            decode_queue_wait_max_us: self.metrics.decode_queue_wait_max_us.get(),
            decode_queue_wait_last_us: self.metrics.decode_queue_wait_last_us.get(),
            decode_current_queue_wait_max_us: self.metrics.decode_current_queue_wait_max_us.get(),
            decode_prefetch_queue_wait_max_us: self.metrics.decode_prefetch_queue_wait_max_us.get(),
            decode_seeked_frames: self.metrics.decode_seeked_frames.get(),
            decode_decoded_frame_count: self.metrics.decode_decoded_frame_count.get(),
            decode_max_decoded_frame_count: self.metrics.decode_max_decoded_frame_count.get(),
            decode_threading_none_frames: self.metrics.decode_threading_none_frames.get(),
            decode_threading_frame_frames: self.metrics.decode_threading_frame_frames.get(),
            decode_threading_slice_frames: self.metrics.decode_threading_slice_frames.get(),
            decode_last_threading_count: self.metrics.decode_last_threading_count.get(),
            decode_max_threading_count: self.metrics.decode_max_threading_count.get(),
            decode_stage_durations: self.metrics.decode_stage_durations.get(),
            decode_max_frame_stage_durations: self.metrics.decode_max_frame_stage_durations.get(),
            decode_max_frame_queue_wait_us: self.metrics.decode_max_frame_queue_wait_us.get(),
            decode_max_frame_bottleneck: self.metrics.decode_max_frame_bottleneck.get(),
            decode_access_mode_profiles: self.metrics.decode_access_mode_profiles.get(),
            render_timed_frames: self.metrics.render_timed_frames.get(),
            render_total_duration_us: self.metrics.render_total_duration_us.get(),
            render_max_duration_us: self.metrics.render_max_duration_us.get(),
            render_last_duration_us: self.metrics.render_last_duration_us.get(),
            render_stage_durations: self.metrics.render_stage_durations.get(),
            render_max_frame_stage_durations: self.metrics.render_max_frame_stage_durations.get(),
            completion_poll_calls: self.metrics.completion_poll_calls.get(),
            completion_poll_results: self.metrics.completion_poll_results.get(),
            completion_poll_total_duration_us: self.metrics.completion_poll_total_duration_us.get(),
            completion_poll_max_duration_us: self.metrics.completion_poll_max_duration_us.get(),
            completion_poll_last_duration_us: self.metrics.completion_poll_last_duration_us.get(),
            completion_poll_max_results_per_poll: self
                .metrics
                .completion_poll_max_results_per_poll
                .get(),
            completion_poll_count_budget_exhaustions: self
                .metrics
                .completion_poll_count_budget_exhaustions
                .get(),
            completion_poll_time_budget_exhaustions: self
                .metrics
                .completion_poll_time_budget_exhaustions
                .get(),
            enqueued_jobs: self.metrics.enqueued_jobs.get(),
            prefetch_skipped_current_pending: self.metrics.prefetch_skipped_current_pending.get(),
            prefetch_skipped_worker_busy: self.metrics.prefetch_skipped_worker_busy.get(),
            prefetch_skipped_prefetch_backlog: self.metrics.prefetch_skipped_prefetch_backlog.get(),
            queue_full_drops: self.metrics.queue_full_drops.get(),
            queue_invalid_access_mode_drops: self.metrics.queue_invalid_access_mode_drops.get(),
            queue_evicted_prefetch_jobs: self.metrics.queue_evicted_prefetch_jobs.get(),
            queue_evicted_still_jobs: self.metrics.queue_evicted_still_jobs.get(),
            interactive_cancel_requests: self.metrics.interactive_cancel_requests.get(),
            interactive_cancel_scheduler_requests: self
                .metrics
                .interactive_cancel_scheduler_requests
                .get(),
            interactive_cancel_queued_jobs: self.metrics.interactive_cancel_queued_jobs.get(),
            queue_canceled_jobs: self.metrics.queue_canceled_jobs.get(),
            queue_pruned_obsolete_jobs: self.metrics.queue_pruned_obsolete_jobs.get(),
            queue_promoted_current_jobs: self.metrics.queue_promoted_current_jobs.get(),
            worker_disconnected_drops: self.metrics.worker_disconnected_drops.get(),
            worker_queue: self.jobs.diagnostics(),
            worker_activity: self.worker_activity.snapshot(),
            scheduler,
            playback_schedule: self.playback_schedule_diagnostics(),
            viewer_frame_cache_hits: self.metrics.viewer_frame_cache_hits.get(),
            viewer_frame_cache_misses: self.metrics.viewer_frame_cache_misses.get(),
            viewer_frame_cache_entries: self.viewer_frame_cache.borrow().len(),
            media_cache_entries: self.media_cache.borrow().len(),
            media_failure_entries: self.media_failures.borrow().len(),
            color_input_transform_calls: self.metrics.color_input_transform_calls.get(),
            color_input_transform_pixels: self.metrics.color_input_transform_pixels.get(),
            color_output_transform_calls: self.metrics.color_output_transform_calls.get(),
            color_output_transform_pixels: self.metrics.color_output_transform_pixels.get(),
            color_rgba8_boundary_calls: self.metrics.color_rgba8_boundary_calls.get(),
            color_stage_plans: self.metrics.color_stage_plans.get(),
            color_stage_total_stages: self.metrics.color_stage_total_stages.get(),
            color_stage_cpu_input_stages: self.metrics.color_stage_cpu_input_stages.get(),
            color_stage_cpu_output_stages: self.metrics.color_stage_cpu_output_stages.get(),
            color_stage_gpu_color_stages: self.metrics.color_stage_gpu_color_stages.get(),
            color_stage_upload_stages: self.metrics.color_stage_upload_stages.get(),
            color_stage_readback_stages: self.metrics.color_stage_readback_stages.get(),
            color_stage_gpu_blockers: self.metrics.color_stage_gpu_blockers.get(),
            color_stage_gpu_shader_module_blockers: self
                .metrics
                .color_stage_gpu_shader_module_blockers
                .get(),
            color_stage_gpu_ocio_resource_blockers: self
                .metrics
                .color_stage_gpu_ocio_resource_blockers
                .get(),
            color_stage_gpu_wrapper_blockers: self.metrics.color_stage_gpu_wrapper_blockers.get(),
            color_stage_gpu_render_pipeline_blockers: self
                .metrics
                .color_stage_gpu_render_pipeline_blockers
                .get(),
            color_stage_gpu_ocio_config_blockers: self
                .metrics
                .color_stage_gpu_ocio_config_blockers
                .get(),
            color_stage_gpu_ocio_processor_blockers: self
                .metrics
                .color_stage_gpu_ocio_processor_blockers
                .get(),
            color_stage_gpu_ocio_shader_extraction_blockers: self
                .metrics
                .color_stage_gpu_ocio_shader_extraction_blockers
                .get(),
            color_stage_pixels: self.metrics.color_stage_pixels.get(),
            color_composite_plans: self.metrics.color_composite_plans.get(),
            color_composite_elements: self.metrics.color_composite_elements.get(),
            color_composite_float_linear: self.metrics.color_composite_float_linear.get(),
            color_composite_legacy_rgba8: self.metrics.color_composite_legacy_rgba8.get(),
            color_composite_legacy_media_blend_mode: self
                .metrics
                .color_composite_legacy_media_blend_mode
                .get(),
            color_composite_legacy_media_transform: self
                .metrics
                .color_composite_legacy_media_transform
                .get(),
            color_composite_legacy_media_effect: self
                .metrics
                .color_composite_legacy_media_effect
                .get(),
            color_composite_legacy_solid_blend_mode: self
                .metrics
                .color_composite_legacy_solid_blend_mode
                .get(),
            color_composite_legacy_solid_transform: self
                .metrics
                .color_composite_legacy_solid_transform
                .get(),
            color_composite_legacy_solid_effect: self
                .metrics
                .color_composite_legacy_solid_effect
                .get(),
            color_composite_legacy_adjustment_blend_mode: self
                .metrics
                .color_composite_legacy_adjustment_blend_mode
                .get(),
            color_composite_legacy_adjustment_effect: self
                .metrics
                .color_composite_legacy_adjustment_effect
                .get(),
            cpu_output_fallback_frames: self.metrics.cpu_output_fallback_frames.get(),
            cpu_output_fallback_pixels: self.metrics.cpu_output_fallback_pixels.get(),
            preview_gpu_output_blocker_frames: self.metrics.preview_gpu_output_blocker_frames.get(),
            preview_gpu_output_blocker_breakdown: *self
                .metrics
                .preview_gpu_output_blocker_breakdown
                .borrow(),
            gpu_compositing: *self.metrics.gpu_compositing.borrow(),
        }
    }

    /// Return the latest color-management rejection captured for the current viewer request.
    pub fn last_color_rejection(&self) -> Option<AppUiPreviewColorRejection> {
        self.last_color_rejection.borrow().clone()
    }

    /// Cancel outstanding preview decode work without shutting down workers.
    ///
    /// Closing a project, switching projects, or quitting should make any
    /// queued/in-flight frame immediately obsolete so decode workers can
    /// cooperatively stop instead of continuing to consume CPU for invisible
    /// media.
    pub fn cancel_interactive_work(&self) {
        let pending_requests = self.scheduler.diagnostics().pending_requests as u64;
        let generation = self.scheduler.cancel_all();
        let queued_jobs = self.jobs.clear() as u64;
        bump(&self.metrics.interactive_cancel_requests);
        add_cell(
            &self.metrics.interactive_cancel_scheduler_requests,
            pending_requests,
        );
        add_cell(&self.metrics.interactive_cancel_queued_jobs, queued_jobs);
        add_cell(&self.metrics.queue_canceled_jobs, queued_jobs);
        self.last_generation_key.replace(None);
        self.current_generation.set(generation);
        self.current_frame_pending.set(false);
        self.last_ready_frame.replace(None);
        self.external_viewer_frame.replace(None);
        self.media_cache.borrow_mut().clear();
        self.media_failures.borrow_mut().clear();
        self.viewer_frame_cache.borrow_mut().clear();
    }

    /// Shut down preview workers for application exit.
    pub fn shutdown(&self) {
        let already_shutdown = self.shutdown.swap(true, Ordering::AcqRel);
        self.cancel_interactive_work();
        self.jobs.close();
        if !already_shutdown {
            self.reap_workers_async();
        }
    }

    fn reap_workers_async(&self) {
        let handles = self.workers.borrow_mut().drain(..).collect::<Vec<_>>();
        if handles.is_empty() {
            return;
        }

        if let Err(err) = thread::Builder::new()
            .name("mondrian-ui-viewer-preview-reaper".to_owned())
            .spawn(move || join_preview_workers(handles))
        {
            tracing::warn!(
                "failed to start app UI viewer preview reaper; workers will finish detached: {err}"
            );
        }
    }

    /// Poll completed background media preview decodes.
    pub fn poll_finished(&self) -> bool {
        self.poll_finished_outcome().visible_change
    }

    pub(crate) fn poll_finished_outcome(&self) -> AppUiPreviewPollOutcome {
        self.poll_finished_outcome_with_budget(
            MEDIA_PREVIEW_MAX_COMPLETED_RESULTS_PER_POLL,
            Duration::from_micros(MEDIA_PREVIEW_COMPLETED_RESULTS_POLL_BUDGET_US),
        )
    }

    pub(crate) fn expire_stalled_playback_current(&self) -> bool {
        self.expire_stalled_playback_current_with_timeout(Duration::from_micros(
            MEDIA_PREVIEW_PLAYBACK_BUFFERING_STALL_TIMEOUT_US,
        ))
    }

    fn expire_stalled_playback_current_with_timeout(&self, timeout: Duration) -> bool {
        let expired = self.scheduler.expire_realtime_current_older_than(timeout);
        if expired.is_empty() {
            return false;
        }
        let mut canceled_queued_jobs = 0u64;
        for key in &expired {
            canceled_queued_jobs =
                canceled_queued_jobs.saturating_add(self.jobs.cancel_key(key) as u64);
        }
        add_cell(
            &self.metrics.playback_current_stalled_expirations,
            expired.len() as u64,
        );
        self.record_playback_current_late_drop(expired.len() as u64);
        add_cell(&self.metrics.queue_canceled_jobs, canceled_queued_jobs);
        self.current_frame_pending.set(false);
        true
    }

    #[cfg(test)]
    fn poll_finished_with_budget(&self, max_results: usize, time_budget: Duration) -> bool {
        self.poll_finished_outcome_with_budget(max_results, time_budget).visible_change
    }

    fn poll_finished_outcome_with_budget(
        &self,
        max_results: usize,
        time_budget: Duration,
    ) -> AppUiPreviewPollOutcome {
        let poll_started = Instant::now();
        bump(&self.metrics.completion_poll_calls);
        self.metrics
            .completion_poll_max_results_per_poll
            .set(self.metrics.completion_poll_max_results_per_poll.get().max(max_results as u64));
        let mut outcome = AppUiPreviewPollOutcome::default();
        let mut drained = 0usize;
        while drained < max_results {
            if drained > 0 && poll_started.elapsed() >= time_budget {
                bump(&self.metrics.completion_poll_time_budget_exhaustions);
                outcome.needs_follow_up_poll = true;
                break;
            }
            let result = match self.results.borrow().try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => break,
            };
            drained += 1;
            let completion =
                self.scheduler.complete(&result.key, result.generation, result.access_mode);
            self.record_preview_decode_queue_wait(
                result.priority,
                result.access_mode,
                result.queue_wait_us,
            );
            if result.canceled {
                self.record_preview_decode_cancel(
                    result.access_mode,
                    result.cancel_reason,
                    result.decode_elapsed_us,
                    result.cancel_observed_elapsed_us,
                );
                continue;
            }
            if let Some(diagnostics) = result.decode_diagnostics {
                self.scrub_adaptation.borrow_mut().observe_decode(diagnostics);
                self.record_preview_decode(diagnostics, result.priority, result.queue_wait_us);
            }
            match result.frame {
                Some(frame) => {
                    bump(&self.metrics.decode_successes);
                    if let Some(diagnostics) = result.color_diagnostics {
                        self.record_color_transform(diagnostics);
                    }
                    if let Some(diagnostics) = result.color_stage_diagnostics {
                        self.record_color_stage(diagnostics);
                    }
                    if completion.should_cache() {
                        self.media_cache.borrow_mut().insert(result.key.clone(), frame);
                        self.media_failures.borrow_mut().remove(&result.key);
                    }
                    outcome.visible_change |= completion.is_current();
                }
                None => {
                    self.record_preview_decode_failure(result.access_mode, result.failure_reason);
                    if let Some(error) = result.error {
                        tracing::debug!(
                            asset_id = %result.key.asset_id,
                            path = %result.key.path.display(),
                            "viewer preview decode failed: {error}"
                        );
                    }
                    if completion.should_cache() {
                        self.media_failures.borrow_mut().insert(result.key);
                    }
                    outcome.visible_change |= completion.is_current();
                }
            }
        }
        if max_results > 0 && drained == max_results {
            bump(&self.metrics.completion_poll_count_budget_exhaustions);
            outcome.needs_follow_up_poll = true;
        }
        if drained > 0 {
            add_cell(&self.metrics.completion_poll_results, drained as u64);
        }
        self.record_completion_poll_duration(poll_started.elapsed());
        outcome
    }

    fn record_completion_poll_duration(&self, duration: Duration) {
        let duration_us = app_duration_us(duration);
        add_cell(&self.metrics.completion_poll_total_duration_us, duration_us);
        self.metrics
            .completion_poll_max_duration_us
            .set(self.metrics.completion_poll_max_duration_us.get().max(duration_us));
        self.metrics.completion_poll_last_duration_us.set(duration_us);
    }

    fn render_preview(&self, state: &AppState) -> ViewerPreviewState {
        bump(&self.metrics.render_requests);
        self.current_frame_pending.set(false);
        self.last_color_rejection.replace(None);
        let Some(sequence) = state.sequence.as_ref() else {
            self.invalidate_preview_generation();
            self.scheduler.prune_obsolete();
            self.last_ready_frame.replace(None);
            bump(&self.metrics.unavailable_frames);
            return ViewerPreviewState::Unavailable;
        };
        let frame = state.current_frame().max(0);
        let (width, height) = preview_dimensions_for_sequence(sequence);
        let display_snapshot = self.display_snapshot.borrow();
        let display_color_space = match preview_display_color_space(
            sequence,
            &state.project_settings.color_management,
            display_snapshot.as_ref(),
        ) {
            Ok(color_space) => color_space,
            Err(blocker) => {
                self.record_preview_gpu_output_blocker(&blocker);
                self.scheduler.prune_obsolete();
                self.last_ready_frame.replace(None);
                bump(&self.metrics.unavailable_frames);
                return ViewerPreviewState::Unavailable;
            }
        };
        let color_context = sequence.settings.root_preview_color_context(
            &state.project_settings.color_management,
            display_color_space,
        );
        self.activate_preview_generation(ViewerPreviewGenerationKey::from_state(
            state,
            sequence,
            frame,
            width,
            height,
            display_color_space,
        ));
        let render_started_at = Instant::now();
        let resolve_started_at = Instant::now();
        let resolved =
            self.resolve_sequence_elements(state, sequence, frame, width, height, 0, color_context);
        let mut render_stage_durations = AppUiPreviewRenderStageDurations {
            resolve_us: app_duration_us(resolve_started_at.elapsed()),
            ..AppUiPreviewRenderStageDurations::default()
        };
        let preview_state = match resolved {
            Some(resolved) => {
                let final_cache_lookup_started_at = Instant::now();
                if let Some(frame) = resolved
                    .cache_key
                    .as_ref()
                    .and_then(|cache_key| self.external_viewer_frame_for_key(cache_key))
                {
                    render_stage_durations.final_cache_lookup_us =
                        app_duration_us(final_cache_lookup_started_at.elapsed());
                    self.record_render_stage_durations(
                        app_duration_us(render_started_at.elapsed()),
                        render_stage_durations,
                    );
                    ViewerPreviewState::Ready(ViewerFrameContent::ExternalTexture(frame))
                } else if let Some(frame) = resolved
                    .cache_key
                    .as_ref()
                    .and_then(|cache_key| self.cached_viewer_frame(cache_key))
                {
                    render_stage_durations.final_cache_lookup_us =
                        app_duration_us(final_cache_lookup_started_at.elapsed());
                    self.record_render_stage_durations(
                        app_duration_us(render_started_at.elapsed()),
                        render_stage_durations,
                    );
                    self.last_ready_frame.replace(Some(ScopedViewerFrame {
                        sequence_id: sequence.id,
                        width,
                        height,
                        frame: frame.clone(),
                    }));
                    ViewerPreviewState::Ready(ViewerFrameContent::Raster(frame))
                } else if state.is_playing()
                    && preview_elements_require_deferred_composite(&resolved.elements)
                {
                    render_stage_durations.final_cache_lookup_us =
                        app_duration_us(final_cache_lookup_started_at.elapsed());
                    self.record_render_stage_durations(
                        app_duration_us(render_started_at.elapsed()),
                        render_stage_durations,
                    );
                    self.stale_frame_for_sequence(sequence, width, height)
                        .map(|frame| ViewerPreviewState::Stale(ViewerFrameContent::Raster(frame)))
                        .unwrap_or(ViewerPreviewState::Loading)
                } else {
                    render_stage_durations.final_cache_lookup_us =
                        app_duration_us(final_cache_lookup_started_at.elapsed());
                    let output = match composite_resolved_preview(
                        self,
                        width,
                        height,
                        &resolved.elements,
                        &resolved.color_context,
                        &mut self.scratch.borrow_mut(),
                    ) {
                        Ok(rgba) => rgba,
                        Err(err) => {
                            tracing::warn!("viewer preview color render failed: {err}");
                            return ViewerPreviewState::Unavailable;
                        }
                    };
                    render_stage_durations.accumulate(output.render_stage_durations);
                    self.record_composite(output.composite_diagnostics);
                    self.record_color_transform(output.color_diagnostics);
                    self.record_color_stage(output.color_stage_diagnostics);
                    let rgba = output.rgba;
                    let frame_packaging_started_at = Instant::now();
                    let key =
                        resolved.cache_key.as_ref().map(viewer_raster_frame_key).unwrap_or_else(
                            || uncached_viewer_raster_frame_key(sequence.id, frame, width, height),
                        );
                    match ViewerFrameImage::new(key, width, height, rgba) {
                        Some(frame) => {
                            if let Some(cache_key) = resolved.cache_key {
                                self.viewer_frame_cache
                                    .borrow_mut()
                                    .insert(cache_key, frame.clone());
                            }
                            render_stage_durations.frame_packaging_us =
                                app_duration_us(frame_packaging_started_at.elapsed());
                            self.record_render_stage_durations(
                                app_duration_us(render_started_at.elapsed()),
                                render_stage_durations,
                            );
                            self.last_ready_frame.replace(Some(ScopedViewerFrame {
                                sequence_id: sequence.id,
                                width,
                                height,
                                frame: frame.clone(),
                            }));
                            ViewerPreviewState::Ready(ViewerFrameContent::Raster(frame))
                        }
                        None => ViewerPreviewState::Unavailable,
                    }
                }
            }
            None if self.current_frame_pending.get() => self
                .stale_frame_for_sequence(sequence, width, height)
                .map(|frame| ViewerPreviewState::Stale(ViewerFrameContent::Raster(frame)))
                .unwrap_or(ViewerPreviewState::Loading),
            None => ViewerPreviewState::Unavailable,
        };
        self.schedule_media_prefetches(state, sequence, frame, width, height);
        self.scheduler.prune_obsolete();
        self.record_preview_state(&preview_state);
        preview_state
    }

    /// Build a CPU working-space preview frame suitable for the app-window GPU output boundary.
    ///
    /// The returned frame is not display encoded. The app window owns wgpu recording,
    /// output texture registration, and texture lifetime.
    pub(crate) fn gpu_preview_frame_for_state(
        &self,
        state: &AppState,
    ) -> AppUiGpuPreviewFrameState {
        bump(&self.metrics.gpu_preview_candidate_requests);
        self.current_frame_pending.set(false);
        self.last_color_rejection.replace(None);
        let Some(sequence) = state.sequence.as_ref() else {
            self.invalidate_preview_generation();
            self.scheduler.prune_obsolete();
            self.external_viewer_frame.replace(None);
            bump(&self.metrics.gpu_preview_candidate_unavailable);
            return AppUiGpuPreviewFrameState::Unavailable;
        };
        let frame = state.current_frame().max(0);
        let (width, height) = preview_dimensions_for_sequence(sequence);
        let display_snapshot = self.display_snapshot.borrow();
        let display_color_space = match preview_display_color_space(
            sequence,
            &state.project_settings.color_management,
            display_snapshot.as_ref(),
        ) {
            Ok(color_space) => color_space,
            Err(blocker) => {
                self.record_preview_gpu_output_blocker(&blocker);
                self.scheduler.prune_obsolete();
                self.external_viewer_frame.replace(None);
                bump(&self.metrics.gpu_preview_candidate_unavailable);
                return AppUiGpuPreviewFrameState::Unavailable;
            }
        };
        let color_context = sequence.settings.root_preview_color_context(
            &state.project_settings.color_management,
            display_color_space,
        );
        self.activate_preview_generation(ViewerPreviewGenerationKey::from_state(
            state,
            sequence,
            frame,
            width,
            height,
            display_color_space,
        ));
        let resolved = match self.resolve_sequence_elements(
            state,
            sequence,
            frame,
            width,
            height,
            0,
            color_context,
        ) {
            Some(resolved) => resolved,
            None if self.current_frame_pending.get() => {
                self.schedule_media_prefetches(state, sequence, frame, width, height);
                self.scheduler.prune_obsolete();
                bump(&self.metrics.gpu_preview_candidate_loading);
                return AppUiGpuPreviewFrameState::Loading;
            }
            None => {
                self.scheduler.prune_obsolete();
                self.external_viewer_frame.replace(None);
                bump(&self.metrics.gpu_preview_candidate_unavailable);
                return AppUiGpuPreviewFrameState::Unavailable;
            }
        };
        let Some(cache_key) = resolved.cache_key.clone() else {
            self.scheduler.prune_obsolete();
            self.external_viewer_frame.replace(None);
            bump(&self.metrics.gpu_preview_candidate_unavailable);
            return AppUiGpuPreviewFrameState::Unavailable;
        };
        let candidate_id = self.next_gpu_preview_candidate_id.get().saturating_add(1);
        self.next_gpu_preview_candidate_id.set(candidate_id);
        if self.external_viewer_frame_for_key(&cache_key).is_some() {
            self.schedule_media_prefetches(state, sequence, frame, width, height);
            self.scheduler.prune_obsolete();
            bump(&self.metrics.gpu_preview_candidate_current);
            return AppUiGpuPreviewFrameState::Current;
        }
        let boundary = output_boundary_from_color_context(&resolved.color_context);
        let working_input = match gpu_composite_layers_for_resolved(
            width,
            height,
            &resolved.elements,
            resolved.color_context.working_color_space,
        ) {
            Ok(layers) => {
                self.record_composite(TimelineCompositeDiagnostics {
                    elements: resolved.elements.len() as u64,
                    float_linear_composites: 1,
                    ..TimelineCompositeDiagnostics::default()
                });
                AppUiGpuPreviewWorkingInput::GpuComposite { layers }
            }
            Err(reason) => {
                self.record_gpu_compositing(GpuCompositingDiagnostics {
                    cpu_fallback_composites: 1,
                    cpu_composited_pixels: (width as u64).saturating_mul(height as u64),
                    first_blocker: Some(reason),
                    ..GpuCompositingDiagnostics::default()
                });
                self.schedule_media_prefetches(state, sequence, frame, width, height);
                self.scheduler.prune_obsolete();
                bump(&self.metrics.gpu_preview_candidate_unavailable);
                return AppUiGpuPreviewFrameState::Unavailable;
            }
        };
        self.schedule_media_prefetches(state, sequence, frame, width, height);
        self.scheduler.prune_obsolete();
        bump(&self.metrics.gpu_preview_candidate_ready);
        add_cell(
            &self.metrics.gpu_preview_candidate_pixels,
            (width as u64).saturating_mul(height as u64),
        );
        AppUiGpuPreviewFrameState::Ready(Box::new(AppUiGpuPreviewFrame {
            cache_key,
            sequence_id: sequence.id,
            frame,
            width,
            height,
            working_color_space: resolved.color_context.working_color_space,
            working_input,
            boundary,
            preview_candidate_id: candidate_id,
        }))
    }

    /// Mark a GPU preview output texture as the current viewer frame for its resolved plan.
    pub(crate) fn set_external_viewer_frame(
        &self,
        frame: &AppUiGpuPreviewFrame,
        texture_key: impl Into<String>,
    ) -> bool {
        let Some(content) = ViewerExternalTextureFrame::new(texture_key, frame.width, frame.height)
        else {
            bump(&self.metrics.gpu_preview_external_frames_rejected);
            return false;
        };
        self.external_viewer_frame.replace(Some(ScopedExternalViewerFrame {
            cache_key: frame.cache_key.clone(),
            content,
        }));
        bump(&self.metrics.gpu_preview_external_frames_registered);
        true
    }

    /// Clear any external GPU viewer frame currently advertised by the service.
    pub(crate) fn clear_external_viewer_frame(&self) {
        self.external_viewer_frame.replace(None);
        bump(&self.metrics.gpu_preview_external_frames_cleared);
    }

    /// Synchronize the display output contract snapshot used by preview scheduling.
    pub(crate) fn set_display_output_snapshot(&self, snapshot: Option<&DisplayOutputSnapshot>) {
        let previous_generation = self
            .display_snapshot
            .borrow()
            .as_ref()
            .map(DisplayOutputSnapshot::contract_generation);
        let next_generation = snapshot.map(DisplayOutputSnapshot::contract_generation);

        if previous_generation != next_generation {
            self.clear_cached_frames_for_display_change();
        }

        self.display_snapshot.replace(snapshot.cloned());
    }

    fn clear_cached_frames_for_display_change(&self) {
        self.viewer_frame_cache.borrow_mut().clear();
        self.external_viewer_frame.replace(None);
        self.last_ready_frame.replace(None);
        self.invalidate_preview_generation();
        self.scheduler.prune_obsolete();
    }

    fn activate_preview_generation(&self, key: ViewerPreviewGenerationKey) -> u64 {
        let mut last_key = self.last_generation_key.borrow_mut();
        if last_key.as_ref() == Some(&key) {
            return self.current_generation.get();
        }
        *last_key = Some(key);
        let generation = self.scheduler.begin_generation();
        self.current_generation.set(generation);
        generation
    }

    fn invalidate_preview_generation(&self) {
        self.last_generation_key.replace(None);
        let generation = self.scheduler.begin_generation();
        self.current_generation.set(generation);
    }

    fn cached_viewer_frame(&self, key: &ViewerPreviewCacheKey) -> Option<ViewerFrameImage> {
        let frame = self.viewer_frame_cache.borrow_mut().get(key);
        if frame.is_some() {
            bump(&self.metrics.viewer_frame_cache_hits);
        } else {
            bump(&self.metrics.viewer_frame_cache_misses);
        }
        frame
    }

    fn external_viewer_frame_for_key(
        &self,
        key: &ViewerPreviewCacheKey,
    ) -> Option<ViewerExternalTextureFrame> {
        let frame = self.external_viewer_frame.borrow();
        let frame = frame.as_ref()?;
        (&frame.cache_key == key).then(|| frame.content.clone())
    }

    fn record_preview_state(&self, state: &ViewerPreviewState) {
        match state {
            ViewerPreviewState::Ready(_) => bump(&self.metrics.ready_frames),
            ViewerPreviewState::Loading => bump(&self.metrics.loading_frames),
            ViewerPreviewState::Stale(_) => bump(&self.metrics.stale_frames),
            ViewerPreviewState::Unavailable => bump(&self.metrics.unavailable_frames),
        }
    }

    fn record_input_color_resolution(&self, source: InputColorResolutionSource) {
        match source {
            InputColorResolutionSource::Override => {
                bump(&self.metrics.input_color_resolution_override);
            }
            InputColorResolutionSource::DataTexture => {
                bump(&self.metrics.input_color_resolution_data_texture);
            }
            InputColorResolutionSource::DetectedMetadata => {
                bump(&self.metrics.input_color_resolution_detected_metadata);
            }
            InputColorResolutionSource::MissingPolicyAssumeRec709 => {
                bump(&self.metrics.input_color_resolution_missing_assume_rec709);
            }
            InputColorResolutionSource::MissingPolicyAssumeSequenceWorkingSpace => {
                bump(&self.metrics.input_color_resolution_missing_assume_working);
            }
            InputColorResolutionSource::MissingPolicyRejectMedia => {
                bump(&self.metrics.input_color_resolution_missing_rejected);
            }
        }
    }

    fn record_color_transform(&self, diagnostics: RenderColorTransformDiagnostics) {
        match diagnostics.direction {
            RenderColorTransformDirection::InputToWorking => {
                bump(&self.metrics.color_input_transform_calls);
                add_cell(
                    &self.metrics.color_input_transform_pixels,
                    diagnostics.pixel_count as u64,
                );
            }
            RenderColorTransformDirection::WorkingToOutput => {
                bump(&self.metrics.color_output_transform_calls);
                add_cell(
                    &self.metrics.color_output_transform_pixels,
                    diagnostics.pixel_count as u64,
                );
            }
        }
        if diagnostics.used_rgba8_boundary {
            bump(&self.metrics.color_rgba8_boundary_calls);
        }
    }

    fn record_color_stage(&self, diagnostics: RenderColorStageDiagnostics) {
        bump(&self.metrics.color_stage_plans);
        add_cell(
            &self.metrics.color_stage_total_stages,
            diagnostics.total_stages,
        );
        add_cell(
            &self.metrics.color_stage_cpu_input_stages,
            diagnostics.cpu_input_stages,
        );
        add_cell(
            &self.metrics.color_stage_cpu_output_stages,
            diagnostics.cpu_output_stages,
        );
        add_cell(
            &self.metrics.color_stage_gpu_color_stages,
            diagnostics.gpu_color_stages,
        );
        add_cell(
            &self.metrics.color_stage_upload_stages,
            diagnostics.upload_stages,
        );
        add_cell(
            &self.metrics.color_stage_readback_stages,
            diagnostics.readback_stages,
        );
        add_cell(
            &self.metrics.color_stage_gpu_blockers,
            diagnostics.gpu_blockers,
        );
        add_cell(
            &self.metrics.color_stage_gpu_shader_module_blockers,
            diagnostics.gpu_blocker_breakdown.shader_module_not_prepared,
        );
        add_cell(
            &self.metrics.color_stage_gpu_ocio_resource_blockers,
            diagnostics.gpu_blocker_breakdown.ocio_resource_bind_group_not_prepared,
        );
        add_cell(
            &self.metrics.color_stage_gpu_wrapper_blockers,
            diagnostics.gpu_blocker_breakdown.fullscreen_wrapper_not_prepared,
        );
        add_cell(
            &self.metrics.color_stage_gpu_render_pipeline_blockers,
            diagnostics.gpu_blocker_breakdown.render_pipeline_not_prepared,
        );
        add_cell(
            &self.metrics.color_stage_gpu_ocio_config_blockers,
            diagnostics.gpu_blocker_breakdown.ocio_config_not_loaded,
        );
        add_cell(
            &self.metrics.color_stage_gpu_ocio_processor_blockers,
            diagnostics.gpu_blocker_breakdown.ocio_processor_unavailable,
        );
        add_cell(
            &self.metrics.color_stage_gpu_ocio_shader_extraction_blockers,
            diagnostics.gpu_blocker_breakdown.ocio_gpu_shader_extraction_failed,
        );
        add_cell(&self.metrics.color_stage_pixels, diagnostics.stage_pixels);
    }

    fn record_preview_decode(
        &self,
        diagnostics: PreviewDecodeDiagnostics,
        priority: MediaPreviewRequestPriority,
        queue_wait_us: u64,
    ) {
        self.record_playback_current_success(priority, diagnostics.access_mode);
        match diagnostics.path {
            PreviewDecodePath::InProcessFfmpegCpuRgba => {
                bump(&self.metrics.decode_in_process_cpu_rgba_frames);
            }
            PreviewDecodePath::ExternalFfmpegCpuRgba => {
                bump(&self.metrics.decode_external_ffmpeg_cpu_rgba_frames);
            }
            PreviewDecodePath::PlaybackSessionRingHit => {
                bump(&self.metrics.decode_playback_session_ring_hit_frames);
            }
            PreviewDecodePath::PreviewCacheHit => {
                bump(&self.metrics.decode_cache_hit_frames);
            }
        }
        match diagnostics.access_mode {
            PreviewDecodeAccessMode::PlaybackCursor => {
                bump(&self.metrics.decode_playback_cursor_frames);
            }
            PreviewDecodeAccessMode::ScrubCursor => {
                bump(&self.metrics.decode_scrub_cursor_frames);
            }
            PreviewDecodeAccessMode::RandomAccessStillFrame => {
                bump(&self.metrics.decode_random_access_still_frames);
            }
        }
        add_cell(
            &self.metrics.decode_total_duration_us,
            diagnostics.elapsed_us,
        );
        if diagnostics.elapsed_us >= self.metrics.decode_max_duration_us.get() {
            self.metrics.decode_max_duration_us.set(diagnostics.elapsed_us);
            self.metrics.decode_max_frame_stage_durations.set(diagnostics.stage_durations);
            self.metrics.decode_max_frame_queue_wait_us.set(queue_wait_us);
            self.metrics.decode_max_frame_bottleneck.set(classify_preview_decode_bottleneck(
                diagnostics.stage_durations,
                queue_wait_us,
            ));
        }
        self.metrics.decode_last_duration_us.set(diagnostics.elapsed_us);
        if diagnostics.seek_performed {
            bump(&self.metrics.decode_seeked_frames);
        }
        add_cell(
            &self.metrics.decode_decoded_frame_count,
            u64::from(diagnostics.decoded_frame_count),
        );
        self.metrics.decode_max_decoded_frame_count.set(
            self.metrics
                .decode_max_decoded_frame_count
                .get()
                .max(u64::from(diagnostics.decoded_frame_count)),
        );
        match diagnostics.threading_kind {
            PreviewDecodeThreadingKind::None => bump(&self.metrics.decode_threading_none_frames),
            PreviewDecodeThreadingKind::Frame => bump(&self.metrics.decode_threading_frame_frames),
            PreviewDecodeThreadingKind::Slice => bump(&self.metrics.decode_threading_slice_frames),
        }
        let threading_count = u64::from(diagnostics.threading_count);
        self.metrics.decode_last_threading_count.set(threading_count);
        self.metrics
            .decode_max_threading_count
            .set(self.metrics.decode_max_threading_count.get().max(threading_count));
        let mut stage_durations = self.metrics.decode_stage_durations.get();
        stage_durations.accumulate(diagnostics.stage_durations);
        self.metrics.decode_stage_durations.set(stage_durations);
        let mut access_mode_profiles = self.metrics.decode_access_mode_profiles.get();
        access_mode_profiles.record(diagnostics, queue_wait_us);
        self.metrics.decode_access_mode_profiles.set(access_mode_profiles);
    }

    fn record_preview_decode_cancel(
        &self,
        access_mode: PreviewDecodeAccessMode,
        reason: Option<MediaPreviewCancelReason>,
        elapsed_us: u64,
        observed_elapsed_us: Option<u64>,
    ) {
        bump(&self.metrics.decode_canceled_jobs);
        let reason = reason.unwrap_or(MediaPreviewCancelReason::Unknown);
        match reason {
            MediaPreviewCancelReason::Shutdown => bump(&self.metrics.decode_canceled_shutdown_jobs),
            MediaPreviewCancelReason::Obsolete => bump(&self.metrics.decode_canceled_obsolete_jobs),
            MediaPreviewCancelReason::PrefetchDeadline => {
                bump(&self.metrics.decode_canceled_prefetch_deadline_jobs);
            }
            MediaPreviewCancelReason::PlaybackDeadline => {
                bump(&self.metrics.decode_canceled_playback_deadline_jobs);
                self.record_playback_current_late_drop(1);
            }
            MediaPreviewCancelReason::PrefetchPreemptedByCurrent => {
                bump(&self.metrics.decode_canceled_prefetch_preempted_jobs);
            }
            MediaPreviewCancelReason::StillPreemptedByRealtimeCurrent => {
                bump(&self.metrics.decode_canceled_still_preempted_jobs);
            }
            MediaPreviewCancelReason::Unknown => bump(&self.metrics.decode_canceled_unknown_jobs),
        }
        match access_mode {
            PreviewDecodeAccessMode::PlaybackCursor => {
                bump(&self.metrics.decode_canceled_playback_cursor_jobs);
            }
            PreviewDecodeAccessMode::ScrubCursor => {
                bump(&self.metrics.decode_canceled_scrub_cursor_jobs);
            }
            PreviewDecodeAccessMode::RandomAccessStillFrame => {
                bump(&self.metrics.decode_canceled_random_access_still_jobs);
            }
        }
        add_cell(&self.metrics.decode_canceled_total_duration_us, elapsed_us);
        self.metrics
            .decode_canceled_max_duration_us
            .set(self.metrics.decode_canceled_max_duration_us.get().max(elapsed_us));
        self.metrics.decode_canceled_last_duration_us.set(elapsed_us);
        let return_latency_us = observed_elapsed_us
            .map(|observed_us| elapsed_us.saturating_sub(observed_us))
            .unwrap_or(elapsed_us);
        add_cell(
            &self.metrics.decode_canceled_return_latency_total_us,
            return_latency_us,
        );
        self.metrics
            .decode_canceled_return_latency_max_us
            .set(self.metrics.decode_canceled_return_latency_max_us.get().max(return_latency_us));
        self.metrics.decode_canceled_return_latency_last_us.set(return_latency_us);
        let mut access_mode_profiles = self.metrics.decode_access_mode_profiles.get();
        access_mode_profiles.record_cancel(access_mode, reason, elapsed_us, return_latency_us);
        self.metrics.decode_access_mode_profiles.set(access_mode_profiles);
    }

    fn record_preview_decode_failure(
        &self,
        access_mode: PreviewDecodeAccessMode,
        reason: Option<MediaPreviewFailureReason>,
    ) {
        let reason = reason.unwrap_or(MediaPreviewFailureReason::DecodeError);
        bump(&self.metrics.decode_failures);
        match reason {
            MediaPreviewFailureReason::Timeout => {
                bump(&self.metrics.decode_timeout_failures);
            }
            MediaPreviewFailureReason::ForwardDecodeBudgetExhausted => {
                bump(&self.metrics.decode_budget_exhausted_failures);
            }
            MediaPreviewFailureReason::DecodeError => {}
        }
        let mut access_mode_profiles = self.metrics.decode_access_mode_profiles.get();
        access_mode_profiles.record_failure(access_mode, reason);
        self.metrics.decode_access_mode_profiles.set(access_mode_profiles);
    }

    fn record_preview_decode_queue_wait(
        &self,
        priority: MediaPreviewRequestPriority,
        access_mode: PreviewDecodeAccessMode,
        queue_wait_us: u64,
    ) {
        add_cell(&self.metrics.decode_queue_wait_total_us, queue_wait_us);
        self.metrics
            .decode_queue_wait_max_us
            .set(self.metrics.decode_queue_wait_max_us.get().max(queue_wait_us));
        self.metrics.decode_queue_wait_last_us.set(queue_wait_us);
        match priority {
            MediaPreviewRequestPriority::Current => {
                self.metrics
                    .decode_current_queue_wait_max_us
                    .set(self.metrics.decode_current_queue_wait_max_us.get().max(queue_wait_us));
            }
            MediaPreviewRequestPriority::Prefetch => {
                self.metrics
                    .decode_prefetch_queue_wait_max_us
                    .set(self.metrics.decode_prefetch_queue_wait_max_us.get().max(queue_wait_us));
            }
        }
        let mut access_mode_profiles = self.metrics.decode_access_mode_profiles.get();
        access_mode_profiles.record_queue_wait(access_mode, queue_wait_us);
        self.metrics.decode_access_mode_profiles.set(access_mode_profiles);
    }

    fn playback_schedule_diagnostics(&self) -> AppUiPreviewPlaybackScheduleDiagnostics {
        AppUiPreviewPlaybackScheduleDiagnostics {
            last_current_deadline_budget_us: self.metrics.playback_current_deadline_budget_us.get(),
            current_deadline_assignments: self.metrics.playback_current_deadline_assignments.get(),
            current_deadline_missing_frame_rate: self
                .metrics
                .playback_current_deadline_missing_frame_rate
                .get(),
            current_decode_decisions: self.metrics.playback_current_decode_decisions.get(),
            current_drop_late_decisions: self.metrics.playback_current_drop_late_decisions.get(),
            current_proxy_or_hardware_recommended_decisions: self
                .metrics
                .playback_current_proxy_or_hardware_recommended_decisions
                .get(),
            current_proxy_generation_requests: self.metrics.media_proxy_generation_requests.get(),
            current_proxy_generation_request_dedupes: self
                .metrics
                .media_proxy_generation_request_dedupes
                .get(),
            current_late_streak: self.metrics.playback_current_late_streak.get(),
            sustained_pressure_active: self.playback_sustained_pressure_active(),
            sustained_pressure_events: self.metrics.playback_sustained_pressure_events.get(),
            sustained_pressure_recoveries: self
                .metrics
                .playback_sustained_pressure_recoveries
                .get(),
            prefetch_skipped_sustained_pressure: self
                .metrics
                .playback_prefetch_skipped_sustained_pressure
                .get(),
            forward_prefetch_horizon_us: MEDIA_PREVIEW_FORWARD_PREFETCH_HORIZON_US,
            last_forward_prefetch_window_frames: self
                .metrics
                .playback_forward_prefetch_window_frames
                .get(),
            forward_prefetch_min_frames: MEDIA_PREVIEW_FORWARD_PREFETCH_MIN_FRAMES,
            forward_prefetch_max_frames: MEDIA_PREVIEW_FORWARD_PREFETCH_MAX_FRAMES,
            forward_prefetch_window_evaluations: self
                .metrics
                .playback_forward_prefetch_window_evaluations
                .get(),
            forward_prefetch_invalid_frame_rate: self
                .metrics
                .playback_forward_prefetch_invalid_frame_rate
                .get(),
        }
    }

    fn record_playback_current_deadline_budget(&self, budget_us: Option<u64>) {
        match budget_us {
            Some(budget_us) => {
                self.metrics.playback_current_deadline_budget_us.set(Some(budget_us));
                bump(&self.metrics.playback_current_deadline_assignments);
                bump(&self.metrics.playback_current_decode_decisions);
            }
            None => {
                bump(&self.metrics.playback_current_deadline_missing_frame_rate);
                bump(&self.metrics.playback_current_proxy_or_hardware_recommended_decisions);
            }
        }
    }

    fn record_playback_current_late_drop(&self, count: u64) {
        if count == 0 {
            return;
        }
        add_cell(&self.metrics.playback_current_drop_late_decisions, count);
        add_cell(
            &self.metrics.playback_current_proxy_or_hardware_recommended_decisions,
            count,
        );
        let previous = self.metrics.playback_current_late_streak.get();
        let next = previous.saturating_add(count);
        self.metrics.playback_current_late_streak.set(next);
        if previous < MEDIA_PREVIEW_PLAYBACK_PRESSURE_LATE_STREAK_THRESHOLD
            && next >= MEDIA_PREVIEW_PLAYBACK_PRESSURE_LATE_STREAK_THRESHOLD
        {
            bump(&self.metrics.playback_sustained_pressure_events);
        }
    }

    fn record_playback_current_success(
        &self,
        priority: MediaPreviewRequestPriority,
        access_mode: PreviewDecodeAccessMode,
    ) {
        if priority != MediaPreviewRequestPriority::Current
            || access_mode != PreviewDecodeAccessMode::PlaybackCursor
        {
            return;
        }
        let previous = self.metrics.playback_current_late_streak.replace(0);
        if previous >= MEDIA_PREVIEW_PLAYBACK_PRESSURE_LATE_STREAK_THRESHOLD {
            bump(&self.metrics.playback_sustained_pressure_recoveries);
        }
    }

    fn playback_sustained_pressure_active(&self) -> bool {
        self.metrics.playback_current_late_streak.get()
            >= MEDIA_PREVIEW_PLAYBACK_PRESSURE_LATE_STREAK_THRESHOLD
    }

    fn record_playback_forward_prefetch_window(&self, window_frames: Option<usize>) {
        match window_frames {
            Some(window_frames) => {
                self.metrics.playback_forward_prefetch_window_frames.set(Some(window_frames));
                bump(&self.metrics.playback_forward_prefetch_window_evaluations);
            }
            None => bump(&self.metrics.playback_forward_prefetch_invalid_frame_rate),
        }
    }

    fn record_render_stage_durations(
        &self,
        total_duration_us: u64,
        durations: AppUiPreviewRenderStageDurations,
    ) {
        bump(&self.metrics.render_timed_frames);
        add_cell(&self.metrics.render_total_duration_us, total_duration_us);
        if total_duration_us >= self.metrics.render_max_duration_us.get() {
            self.metrics.render_max_duration_us.set(total_duration_us);
            self.metrics.render_max_frame_stage_durations.set(durations);
        }
        self.metrics.render_last_duration_us.set(total_duration_us);
        let mut stage_durations = self.metrics.render_stage_durations.get();
        stage_durations.accumulate(durations);
        self.metrics.render_stage_durations.set(stage_durations);
    }

    fn record_composite(&self, diagnostics: TimelineCompositeDiagnostics) {
        bump(&self.metrics.color_composite_plans);
        add_cell(&self.metrics.color_composite_elements, diagnostics.elements);
        add_cell(
            &self.metrics.color_composite_float_linear,
            diagnostics.float_linear_composites,
        );
        add_cell(
            &self.metrics.color_composite_legacy_rgba8,
            diagnostics.legacy_rgba8_composites,
        );
        add_cell(
            &self.metrics.color_composite_legacy_media_blend_mode,
            diagnostics.legacy_media_blend_mode,
        );
        add_cell(
            &self.metrics.color_composite_legacy_media_transform,
            diagnostics.legacy_media_transform,
        );
        add_cell(
            &self.metrics.color_composite_legacy_media_effect,
            diagnostics.legacy_media_effect,
        );
        add_cell(
            &self.metrics.color_composite_legacy_solid_blend_mode,
            diagnostics.legacy_solid_blend_mode,
        );
        add_cell(
            &self.metrics.color_composite_legacy_solid_transform,
            diagnostics.legacy_solid_transform,
        );
        add_cell(
            &self.metrics.color_composite_legacy_solid_effect,
            diagnostics.legacy_solid_effect,
        );
        add_cell(
            &self.metrics.color_composite_legacy_adjustment_blend_mode,
            diagnostics.legacy_adjustment_blend_mode,
        );
        add_cell(
            &self.metrics.color_composite_legacy_adjustment_effect,
            diagnostics.legacy_adjustment_effect,
        );
    }

    pub(crate) fn record_cpu_output_fallback(&self, width: u32, height: u32) {
        bump(&self.metrics.cpu_output_fallback_frames);
        add_cell(
            &self.metrics.cpu_output_fallback_pixels,
            (width as u64).saturating_mul(height as u64),
        );
    }

    pub(crate) fn record_preview_gpu_output_blocker(
        &self,
        blocker: &crate::app_ui::preview_gpu_output_blocker::PreviewGpuOutputBlocker,
    ) {
        bump(&self.metrics.preview_gpu_output_blocker_frames);
        self.metrics.preview_gpu_output_blocker_breakdown.borrow_mut().record(blocker);
    }

    pub(crate) fn record_preview_gpu_output_blocker_breakdown(
        &self,
        breakdown: crate::app_ui::preview_gpu_output_blocker::PreviewGpuOutputBlockerBreakdown,
    ) {
        if breakdown.is_empty() {
            return;
        }
        bump(&self.metrics.preview_gpu_output_blocker_frames);
        self.metrics
            .preview_gpu_output_blocker_breakdown
            .borrow_mut()
            .accumulate(breakdown);
    }

    pub(crate) fn record_gpu_compositing(&self, diagnostics: GpuCompositingDiagnostics) {
        if diagnostics == GpuCompositingDiagnostics::default() {
            return;
        }
        self.metrics.gpu_compositing.borrow_mut().accumulate(diagnostics);
    }

    fn stale_frame_for_sequence(
        &self,
        sequence: &Sequence,
        width: u32,
        height: u32,
    ) -> Option<ViewerFrameImage> {
        let last = self.last_ready_frame.borrow();
        let frame = last.as_ref()?;
        (frame.sequence_id == sequence.id && frame.width == width && frame.height == height)
            .then(|| frame.frame.clone())
    }

    fn render_nested_sequence_frame(
        &self,
        state: &AppState,
        sequence: &Sequence,
        frame: i64,
        depth: usize,
        parent_color_context: ColorContext,
    ) -> Option<MediaPreviewFrame> {
        if depth > MAX_NESTED_SEQUENCE_RENDER_DEPTH {
            return None;
        }
        let (width, height) = preview_dimensions_for_sequence(sequence);
        let color_context = sequence.settings.nested_render_color_context(parent_color_context);
        let resolved = self
            .resolve_sequence_elements(
                state,
                sequence,
                frame.max(0),
                width,
                height,
                depth,
                color_context.clone(),
            )?
            .elements;
        let mut scratch = TimelineCompositeScratch::default();
        let output = composite_resolved_preview(
            self,
            width,
            height,
            &resolved,
            &color_context,
            &mut scratch,
        )
        .ok()?;
        let render_stage_durations = output.render_stage_durations;
        let render_total_us = render_stage_durations
            .working_prepare_us
            .saturating_add(render_stage_durations.cpu_composite_us)
            .saturating_add(render_stage_durations.cpu_output_boundary_us);
        self.record_composite(output.composite_diagnostics);
        self.record_color_transform(output.color_diagnostics);
        self.record_color_stage(output.color_stage_diagnostics);
        self.record_render_stage_durations(render_total_us, render_stage_durations);
        let rgba = output.rgba;
        let signature =
            nested_preview_frame_signature(sequence.id, frame.max(0), width, height, &rgba);
        let source = CpuEncodedColorFrame::source_rgba8(
            width,
            height,
            color_context.output_color_space,
            rgba,
        );
        let frame = execute_cpu_input_stage(
            &source,
            &RenderInputTransform::to_working(
                color_context.output_color_space,
                false,
                color_context.engine.clone(),
            ),
        )
        .ok()?;
        self.record_color_transform(frame.result.diagnostics);
        self.record_color_stage(frame.stage_diagnostics);
        Some(MediaPreviewFrame {
            width,
            height,
            frame: Some(frame.result.frame),
            gpu_source: None,
            signature,
        })
    }

    fn resolve_sequence_elements(
        &self,
        state: &AppState,
        sequence: &Sequence,
        frame: i64,
        width: u32,
        height: u32,
        depth: usize,
        color_context: ColorContext,
    ) -> Option<ResolvedPreviewPlan> {
        let evaluation = evaluate_timeline_render_plan(
            sequence,
            TimelineEvaluationRequest::preview(
                frame,
                normalize_preview_resolution_scale(sequence.settings.preview.resolution_scale),
            ),
        );
        if evaluation.is_empty() {
            return None;
        }

        let mut resolved = Vec::with_capacity(evaluation.len());
        for element in evaluation.elements {
            match element {
                TimelineRenderPlanElement::SolidColor(solid) => {
                    resolved.push(ResolvedPreviewElement::SolidColor(
                        TimelineSolidColorLayer {
                            color: solid.color,
                            opacity: solid.opacity,
                            blend_mode: solid.blend_mode,
                            transform: solid.transform,
                            effect_graph: solid.effect_graph,
                            frame_seed: solid.frame_seed,
                        },
                    ));
                }
                TimelineRenderPlanElement::Media(media) => {
                    let frame = self.media_frame_for_plan(
                        state,
                        &media.asset_id,
                        media.color_space_override,
                        media.source_frame,
                        media.source_secs,
                        width,
                        height,
                        &color_context,
                        sequence.settings.frame_rate,
                    )?;
                    resolved.push(ResolvedPreviewElement::Media {
                        frame,
                        opacity: media.opacity,
                        blend_mode: media.blend_mode,
                        transform: media.transform,
                        effect_graph: media.effect_graph,
                        frame_seed: media.frame_seed,
                    });
                }
                TimelineRenderPlanElement::Adjustment(adjustment) => {
                    resolved.push(ResolvedPreviewElement::Adjustment(
                        TimelineAdjustmentLayer {
                            effect_graph: adjustment.effect_graph,
                            opacity: adjustment.opacity,
                            blend_mode: Some(adjustment.blend_mode),
                            frame_seed: adjustment.frame_seed,
                        },
                    ));
                }
                TimelineRenderPlanElement::NestedSequence(nested) => {
                    let nested_sequence = state.sequence_by_id(nested.sequence_id)?;
                    let frame = self.render_nested_sequence_frame(
                        state,
                        nested_sequence,
                        nested.source_frame,
                        depth + 1,
                        color_context.clone(),
                    )?;
                    resolved.push(ResolvedPreviewElement::Media {
                        frame,
                        opacity: nested.opacity,
                        blend_mode: nested.blend_mode,
                        transform: nested.transform,
                        effect_graph: nested.effect_graph,
                        frame_seed: nested.frame_seed,
                    });
                }
            }
        }
        let cache_key = Some(viewer_preview_cache_key_for_resolved_plan(
            sequence.id,
            width,
            height,
            &resolved,
            &color_context,
        ));

        Some(ResolvedPreviewPlan { elements: resolved, cache_key, color_context })
    }
}

fn join_preview_workers(handles: Vec<JoinHandle<()>>) {
    let current_thread_id = thread::current().id();
    for handle in handles {
        if handle.thread().id() == current_thread_id {
            continue;
        }
        if handle.join().is_err() {
            tracing::warn!("app UI viewer preview worker panicked during shutdown");
        }
    }
}

struct ResolvedPreviewPlan {
    elements: Vec<ResolvedPreviewElement>,
    cache_key: Option<ViewerPreviewCacheKey>,
    color_context: ColorContext,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ViewerPreviewGenerationKey {
    sequence_id: SequenceId,
    frame: i64,
    width: u32,
    height: u32,
    display_color_space: ColorSpace,
    playing: bool,
    seek_source: crate::app::ui_actions::TimelineSeekSource,
}

impl ViewerPreviewGenerationKey {
    fn from_state(
        state: &AppState,
        sequence: &Sequence,
        frame: i64,
        width: u32,
        height: u32,
        display_color_space: ColorSpace,
    ) -> Self {
        Self {
            sequence_id: sequence.id,
            frame,
            width,
            height,
            display_color_space,
            playing: state.is_playing(),
            seek_source: state.last_timeline_seek_source,
        }
    }
}

/// Aggregated CPU-side viewer render stage timings after media decode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct AppUiPreviewRenderStageDurations {
    /// Time spent resolving sequence elements, media cache keys, and current-frame readiness.
    pub resolve_us: u64,
    /// Time spent checking external/final viewer frame caches.
    pub final_cache_lookup_us: u64,
    /// Time spent preparing working-space inputs for CPU composition.
    pub working_prepare_us: u64,
    /// Time spent in CPU timeline compositing and basic property/effect application.
    pub cpu_composite_us: u64,
    /// Time spent applying the final CPU output/color boundary.
    pub cpu_output_boundary_us: u64,
    /// Time spent hashing, packaging, and storing the final raster viewer frame.
    pub frame_packaging_us: u64,
}

impl AppUiPreviewRenderStageDurations {
    fn accumulate(&mut self, other: Self) {
        self.resolve_us = self.resolve_us.saturating_add(other.resolve_us);
        self.final_cache_lookup_us =
            self.final_cache_lookup_us.saturating_add(other.final_cache_lookup_us);
        self.working_prepare_us = self.working_prepare_us.saturating_add(other.working_prepare_us);
        self.cpu_composite_us = self.cpu_composite_us.saturating_add(other.cpu_composite_us);
        self.cpu_output_boundary_us =
            self.cpu_output_boundary_us.saturating_add(other.cpu_output_boundary_us);
        self.frame_packaging_us = self.frame_packaging_us.saturating_add(other.frame_packaging_us);
    }
}

/// Playback-clock scheduling contract observed by the app preview service.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct AppUiPreviewPlaybackScheduleDiagnostics {
    /// Most recent current-frame playback deadline budget derived from sequence frame rate.
    pub last_current_deadline_budget_us: Option<u64>,
    /// Current playback requests that received a display deadline.
    pub current_deadline_assignments: u64,
    /// Current playback requests whose frame rate could not produce a valid deadline.
    pub current_deadline_missing_frame_rate: u64,
    /// Current playback frames admitted for decode under the playback clock.
    pub current_decode_decisions: u64,
    /// Current playback frames dropped because their display deadline was missed.
    pub current_drop_late_decisions: u64,
    /// Current playback frames that should drive proxy or hardware-decode work.
    pub current_proxy_or_hardware_recommended_decisions: u64,
    /// Current playback path resolutions that requested app-layer proxy generation.
    pub current_proxy_generation_requests: u64,
    /// Current playback proxy generation candidates already queued for the same source revision.
    pub current_proxy_generation_request_dedupes: u64,
    /// Consecutive current playback frames dropped or expired before a successful current frame.
    pub current_late_streak: u64,
    /// Whether playback is currently suppressing prefetch to recover from sustained late frames.
    pub sustained_pressure_active: bool,
    /// Times playback entered sustained pressure recovery.
    pub sustained_pressure_events: u64,
    /// Times a successful current playback frame exited sustained pressure recovery.
    pub sustained_pressure_recoveries: u64,
    /// Playback prefetch passes skipped while sustained pressure recovery was active.
    pub prefetch_skipped_sustained_pressure: u64,
    /// Wall-clock horizon used to derive the forward prefetch window.
    pub forward_prefetch_horizon_us: u64,
    /// Most recent forward prefetch window derived from sequence frame rate.
    pub last_forward_prefetch_window_frames: Option<usize>,
    /// Minimum allowed forward prefetch window.
    pub forward_prefetch_min_frames: usize,
    /// Maximum allowed forward prefetch window.
    pub forward_prefetch_max_frames: usize,
    /// Playback prefetch passes whose frame rate produced a valid dynamic window.
    pub forward_prefetch_window_evaluations: u64,
    /// Playback prefetch passes skipped because frame rate could not produce a valid window.
    pub forward_prefetch_invalid_frame_rate: u64,
}

/// Point-in-time preview service counters for local performance diagnostics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct AppUiPreviewDiagnostics {
    /// Viewer preview render requests received by the service.
    pub render_requests: u64,
    /// Requests that produced a current ready frame.
    pub ready_frames: u64,
    /// Requests waiting for the first current frame.
    pub loading_frames: u64,
    /// Requests that reused a scoped previous frame while the current frame is pending.
    pub stale_frames: u64,
    /// Requests with no renderable preview frame.
    pub unavailable_frames: u64,
    /// Playback current-frame requests expired so buffering cannot hold the shell indefinitely.
    pub playback_current_stalled_expirations: u64,
    /// Requests for a CPU working-frame candidate for the app-window GPU output path.
    pub gpu_preview_candidate_requests: u64,
    /// GPU preview candidate requests that produced a working-frame candidate.
    pub gpu_preview_candidate_ready: u64,
    /// GPU preview candidate requests skipped because the matching external texture is current.
    pub gpu_preview_candidate_current: u64,
    /// GPU preview candidate requests waiting on pending media.
    pub gpu_preview_candidate_loading: u64,
    /// GPU preview candidate requests with no renderable frame.
    pub gpu_preview_candidate_unavailable: u64,
    /// Pixels in working-frame candidates handed to the app-window GPU output path.
    pub gpu_preview_candidate_pixels: u64,
    /// External GPU preview frames accepted into the preview service.
    pub gpu_preview_external_frames_registered: u64,
    /// External GPU preview frames rejected before becoming viewer content.
    pub gpu_preview_external_frames_rejected: u64,
    /// External GPU preview frames cleared by the app-window output path.
    pub gpu_preview_external_frames_cleared: u64,
    /// Media input color resolutions that used a clip/media override.
    pub input_color_resolution_override: u64,
    /// Media input color resolutions that used detected media metadata.
    pub input_color_resolution_detected_metadata: u64,
    /// Media input color resolutions that assumed Rec.709 through missing-metadata policy.
    pub input_color_resolution_missing_assume_rec709: u64,
    /// Media input color resolutions that assumed the sequence working space through policy.
    pub input_color_resolution_missing_assume_working: u64,
    /// Media input color resolutions rejected by missing-metadata policy.
    pub input_color_resolution_missing_rejected: u64,
    /// Media input color resolutions that treated the asset as non-color data.
    pub input_color_resolution_data_texture: u64,
    /// Media preview path resolutions that used an existing proxy file.
    pub media_proxy_path_hits: u64,
    /// Media preview path resolutions that wanted a proxy but fell back to source.
    pub media_proxy_path_misses: u64,
    /// Media preview path resolutions that rejected a stale proxy file.
    pub media_proxy_path_stale: u64,
    /// Media preview path resolutions that intentionally used source media.
    pub media_proxy_path_bypasses: u64,
    /// Playback path resolutions that requested app-layer proxy generation.
    pub media_proxy_generation_requests: u64,
    /// Playback proxy generation candidates already queued for the same source revision.
    pub media_proxy_generation_request_dedupes: u64,
    /// Scrub current-frame requests that used normal decode policy.
    pub scrub_adaptive_normal_requests: u64,
    /// Scrub current-frame requests that tightened policy for a hot seek region.
    pub scrub_adaptive_hot_region_requests: u64,
    /// Scrub current-frame requests that tightened policy because scrub latency is slow.
    pub scrub_adaptive_slow_latency_requests: u64,
    /// Scrub current-frame requests that used conservative recovery policy after slow latency.
    pub scrub_adaptive_recovery_requests: u64,
    /// Media preview cache hits.
    pub media_cache_hits: u64,
    /// Media preview cache misses.
    pub media_cache_misses: u64,
    /// Requests skipped because a media preview key is known to have failed.
    pub media_failure_hits: u64,
    /// Coordinated CPU budget used for preview workers and FFmpeg decoder threads.
    pub decode_cpu_budget: PreviewDecodeCpuBudget,
    /// Preview decode workers successfully started for this service.
    pub decode_worker_count: usize,
    /// Successful background media decodes received by the UI service.
    pub decode_successes: u64,
    /// Failed background media decodes received by the UI service.
    pub decode_failures: u64,
    /// Failed background media decodes caused by a structured decode timeout.
    pub decode_timeout_failures: u64,
    /// Failed background media decodes caused by access-mode forward-scan budget exhaustion.
    pub decode_budget_exhausted_failures: u64,
    /// Background media decodes canceled before producing a frame.
    pub decode_canceled_jobs: u64,
    /// Background media decodes canceled because the preview service is shutting down.
    pub decode_canceled_shutdown_jobs: u64,
    /// Background media decodes canceled because their pending request became obsolete.
    pub decode_canceled_obsolete_jobs: u64,
    /// Prefetch decodes canceled by the app-level prefetch deadline.
    pub decode_canceled_prefetch_deadline_jobs: u64,
    /// Current playback decodes canceled because their display deadline was missed.
    pub decode_canceled_playback_deadline_jobs: u64,
    /// Prefetch decodes canceled because visible current-frame work was pending.
    pub decode_canceled_prefetch_preempted_jobs: u64,
    /// Still-frame decodes canceled because realtime current-frame work was pending.
    pub decode_canceled_still_preempted_jobs: u64,
    /// Background media decodes canceled without a structured app-level reason.
    pub decode_canceled_unknown_jobs: u64,
    /// Total worker execution time spent in canceled decode jobs.
    pub decode_canceled_total_duration_us: u64,
    /// Slowest worker execution time for a canceled decode job.
    pub decode_canceled_max_duration_us: u64,
    /// Most recent worker execution time for a canceled decode job.
    pub decode_canceled_last_duration_us: u64,
    /// Total latency after canceled decode jobs first observed cancellation.
    pub decode_canceled_return_latency_total_us: u64,
    /// Slowest latency after a canceled decode job first observed cancellation.
    pub decode_canceled_return_latency_max_us: u64,
    /// Most recent latency after a canceled decode job first observed cancellation.
    pub decode_canceled_return_latency_last_us: u64,
    /// Successful decodes produced by the in-process FFmpeg CPU RGBA path.
    pub decode_in_process_cpu_rgba_frames: u64,
    /// Successful decodes produced by the external ffmpeg CPU RGBA path.
    pub decode_external_ffmpeg_cpu_rgba_frames: u64,
    /// Successful playback decodes served from the playback session-local ring.
    pub decode_playback_session_ring_hit_frames: u64,
    /// Successful decodes served from the preview frame cache.
    pub decode_cache_hit_frames: u64,
    /// Decode results requested through the playback cursor access contract.
    pub decode_playback_cursor_frames: u64,
    /// Decode results requested through the scrub cursor access contract.
    pub decode_scrub_cursor_frames: u64,
    /// Decode results requested through the random-access still-frame contract.
    pub decode_random_access_still_frames: u64,
    /// Canceled decode jobs requested through the playback cursor access contract.
    pub decode_canceled_playback_cursor_jobs: u64,
    /// Canceled decode jobs requested through the scrub cursor access contract.
    pub decode_canceled_scrub_cursor_jobs: u64,
    /// Canceled decode jobs requested through the random-access still-frame contract.
    pub decode_canceled_random_access_still_jobs: u64,
    /// Total preview decode duration in microseconds.
    pub decode_total_duration_us: u64,
    /// Slowest preview decode duration in microseconds.
    pub decode_max_duration_us: u64,
    /// Most recent successful preview decode duration in microseconds.
    pub decode_last_duration_us: u64,
    /// Total time decoded jobs spent waiting in the preview worker queue.
    pub decode_queue_wait_total_us: u64,
    /// Slowest decoded job queue wait.
    pub decode_queue_wait_max_us: u64,
    /// Most recent decoded job queue wait.
    pub decode_queue_wait_last_us: u64,
    /// Slowest current-frame decode queue wait.
    pub decode_current_queue_wait_max_us: u64,
    /// Slowest prefetch decode queue wait.
    pub decode_prefetch_queue_wait_max_us: u64,
    /// Decode requests that required a decoder seek before frame selection.
    pub decode_seeked_frames: u64,
    /// Total decoded frames consumed by preview decode requests.
    pub decode_decoded_frame_count: u64,
    /// Largest decoded-frame count consumed by one preview decode request.
    pub decode_max_decoded_frame_count: u64,
    /// Decode results that reported no FFmpeg decoder threading.
    pub decode_threading_none_frames: u64,
    /// Decode results that reported frame-level FFmpeg decoder threading.
    pub decode_threading_frame_frames: u64,
    /// Decode results that reported slice-level FFmpeg decoder threading.
    pub decode_threading_slice_frames: u64,
    /// Most recent FFmpeg decoder thread count reported by preview decode.
    pub decode_last_threading_count: u64,
    /// Largest FFmpeg decoder thread count reported by preview decode.
    pub decode_max_threading_count: u64,
    /// Aggregated stage-level timings reported by preview decode.
    pub decode_stage_durations: PreviewDecodeStageDurations,
    /// Stage-level timings from the slowest decoded preview frame.
    pub decode_max_frame_stage_durations: PreviewDecodeStageDurations,
    /// Queue wait observed by the same decoded preview frame that produced
    /// `decode_max_frame_stage_durations`.
    pub decode_max_frame_queue_wait_us: u64,
    /// Dominant bottleneck for the same decoded preview frame that produced
    /// `decode_max_frame_stage_durations`.
    pub decode_max_frame_bottleneck: AppUiPreviewDecodeBottleneck,
    /// Decode profile split by playback, scrub, and random-access still modes.
    pub decode_access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles,
    /// Viewer render requests with post-decode stage timing evidence.
    pub render_timed_frames: u64,
    /// Total post-decode viewer render duration in microseconds.
    pub render_total_duration_us: u64,
    /// Slowest post-decode viewer render duration in microseconds.
    pub render_max_duration_us: u64,
    /// Most recent post-decode viewer render duration in microseconds.
    pub render_last_duration_us: u64,
    /// Aggregated CPU-side viewer render stage timings after media decode.
    pub render_stage_durations: AppUiPreviewRenderStageDurations,
    /// CPU-side viewer render stage timings from the slowest post-decode frame.
    pub render_max_frame_stage_durations: AppUiPreviewRenderStageDurations,
    /// UI-thread completion polling passes for decoded preview results.
    pub completion_poll_calls: u64,
    /// Decoded preview results processed by UI-thread completion polling.
    pub completion_poll_results: u64,
    /// Total UI-thread completion polling duration in microseconds.
    pub completion_poll_total_duration_us: u64,
    /// Slowest UI-thread completion polling pass in microseconds.
    pub completion_poll_max_duration_us: u64,
    /// Most recent UI-thread completion polling pass in microseconds.
    pub completion_poll_last_duration_us: u64,
    /// Largest configured completion-result count budget observed by diagnostics.
    pub completion_poll_max_results_per_poll: u64,
    /// Completion polling passes that stopped at the result-count budget.
    pub completion_poll_count_budget_exhaustions: u64,
    /// Completion polling passes that yielded after the UI-thread time budget.
    pub completion_poll_time_budget_exhaustions: u64,
    /// Media preview jobs accepted by the worker queue.
    pub enqueued_jobs: u64,
    /// Playback prefetch passes skipped because visible current-frame media was pending.
    pub prefetch_skipped_current_pending: u64,
    /// Playback prefetch passes skipped because current-frame work was queued or running.
    pub prefetch_skipped_worker_busy: u64,
    /// Playback prefetch passes skipped because queued/running prefetch already held the window.
    pub prefetch_skipped_prefetch_backlog: u64,
    /// Media preview jobs dropped because the bounded worker queue was full.
    pub queue_full_drops: u64,
    /// Media preview jobs rejected by the worker queue for invalid priority/access-mode pairs.
    pub queue_invalid_access_mode_drops: u64,
    /// Queued prefetch jobs evicted so current-frame decode work can run.
    pub queue_evicted_prefetch_jobs: u64,
    /// Queued still-frame jobs evicted so real-time current work can run.
    pub queue_evicted_still_jobs: u64,
    /// User escape-path cancellations requested by transport, close, or quit.
    pub interactive_cancel_requests: u64,
    /// Scheduler pending requests canceled by user escape-path cancellations.
    pub interactive_cancel_scheduler_requests: u64,
    /// Worker-queue jobs cleared by user escape-path cancellations.
    pub interactive_cancel_queued_jobs: u64,
    /// Queued jobs removed because their scheduler-side pending request was canceled.
    pub queue_canceled_jobs: u64,
    /// Obsolete queued jobs removed before scheduling current-frame decode.
    pub queue_pruned_obsolete_jobs: u64,
    /// Queued prefetch jobs promoted after the same key became current-frame work.
    pub queue_promoted_current_jobs: u64,
    /// Media preview jobs dropped because the worker channel was disconnected.
    pub worker_disconnected_drops: u64,
    /// Current worker transport queue depth grouped by scheduling contract.
    pub worker_queue: MediaPreviewJobQueueDiagnostics,
    /// Current preview worker in-flight decode activity grouped by scheduling contract.
    pub worker_activity: PreviewWorkerActivityDiagnostics,
    /// Scheduler-side request, drop, completion, and pruning counters.
    pub scheduler: MediaPreviewSchedulerDiagnostics,
    /// Playback-clock deadline and forward-prefetch scheduling contract.
    pub playback_schedule: AppUiPreviewPlaybackScheduleDiagnostics,
    /// Final viewer preview frame cache hits.
    pub viewer_frame_cache_hits: u64,
    /// Final viewer preview frame cache misses.
    pub viewer_frame_cache_misses: u64,
    /// Current number of final viewer preview frames in the bounded cache.
    pub viewer_frame_cache_entries: usize,
    /// Current number of frames in the media preview LRU cache.
    pub media_cache_entries: usize,
    /// Current number of keys in the media preview failure LRU cache.
    pub media_failure_entries: usize,
    /// Source/media color transforms into the timeline working space.
    pub color_input_transform_calls: u64,
    /// Pixels processed by source/media color transforms into the timeline working space.
    pub color_input_transform_pixels: u64,
    /// Timeline working-space transforms into preview/output encoding.
    pub color_output_transform_calls: u64,
    /// Pixels processed by timeline working-space transforms into preview/output encoding.
    pub color_output_transform_pixels: u64,
    /// Color transforms that crossed the temporary RGBA8 CPU boundary.
    pub color_rgba8_boundary_calls: u64,
    /// Render color stage plans executed by preview.
    pub color_stage_plans: u64,
    /// Render color stages executed by preview.
    pub color_stage_total_stages: u64,
    /// CPU source/media input stages executed by preview.
    pub color_stage_cpu_input_stages: u64,
    /// CPU working-to-output stages executed by preview.
    pub color_stage_cpu_output_stages: u64,
    /// GPU color stages planned by preview.
    pub color_stage_gpu_color_stages: u64,
    /// CPU-to-GPU upload stages planned by preview.
    pub color_stage_upload_stages: u64,
    /// GPU-to-CPU readback stages planned by preview.
    pub color_stage_readback_stages: u64,
    /// GPU color stage blockers surfaced by preview.
    pub color_stage_gpu_blockers: u64,
    /// GPU blockers caused by missing shader modules.
    pub color_stage_gpu_shader_module_blockers: u64,
    /// GPU blockers caused by missing OCIO LUT/uniform bind groups.
    pub color_stage_gpu_ocio_resource_blockers: u64,
    /// GPU blockers caused by missing fullscreen wrappers.
    pub color_stage_gpu_wrapper_blockers: u64,
    /// GPU blockers caused by missing render pipelines.
    pub color_stage_gpu_render_pipeline_blockers: u64,
    /// GPU blockers caused by missing OCIO config.
    pub color_stage_gpu_ocio_config_blockers: u64,
    /// GPU blockers caused by unavailable OCIO processor.
    pub color_stage_gpu_ocio_processor_blockers: u64,
    /// GPU blockers caused by failed OCIO GPU shader extraction.
    pub color_stage_gpu_ocio_shader_extraction_blockers: u64,
    /// Pixels covered by preview color stage plans.
    pub color_stage_pixels: u64,
    /// Timeline composite plans executed by preview.
    pub color_composite_plans: u64,
    /// Timeline composite elements processed by preview.
    pub color_composite_elements: u64,
    /// Composite plans that stayed on the float/linear path.
    pub color_composite_float_linear: u64,
    /// Composite plans that fell back to the legacy RGBA8 path.
    pub color_composite_legacy_rgba8: u64,
    /// Legacy RGBA8 fallbacks caused by media layer blend modes.
    pub color_composite_legacy_media_blend_mode: u64,
    /// Legacy RGBA8 fallbacks caused by media layer transforms.
    pub color_composite_legacy_media_transform: u64,
    /// Legacy RGBA8 fallbacks caused by media effect graphs.
    pub color_composite_legacy_media_effect: u64,
    /// Legacy RGBA8 fallbacks caused by solid layer blend modes.
    pub color_composite_legacy_solid_blend_mode: u64,
    /// Legacy RGBA8 fallbacks caused by solid layer transforms.
    pub color_composite_legacy_solid_transform: u64,
    /// Legacy RGBA8 fallbacks caused by solid layer effects.
    pub color_composite_legacy_solid_effect: u64,
    /// Legacy RGBA8 fallbacks caused by adjustment layer blend modes.
    pub color_composite_legacy_adjustment_blend_mode: u64,
    /// Legacy RGBA8 fallbacks caused by adjustment effect graphs.
    pub color_composite_legacy_adjustment_effect: u64,
    /// Number of raster preview frames that used CPU output transform fallback.
    pub cpu_output_fallback_frames: u64,
    /// Pixels processed through CPU output transform fallback.
    pub cpu_output_fallback_pixels: u64,
    /// Number of preview frames with structured GPU output blockers.
    pub preview_gpu_output_blocker_frames: u64,
    /// Structured GPU output blocker breakdown across preview frames.
    pub preview_gpu_output_blocker_breakdown:
        crate::app_ui::preview_gpu_output_blocker::PreviewGpuOutputBlockerBreakdown,
    /// GPU compositing capability diagnostics.
    pub gpu_compositing: mondrian_renderer::GpuCompositingDiagnostics,
}

/// Point-in-time preview worker decode activity grouped by scheduling contract.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct PreviewWorkerActivityDiagnostics {
    /// Decode jobs currently running in preview workers.
    pub in_flight_jobs: usize,
    /// Current-frame decode jobs currently running in preview workers.
    pub in_flight_current_jobs: usize,
    /// Prefetch decode jobs currently running in preview workers.
    pub in_flight_prefetch_jobs: usize,
    /// Playback cursor decode jobs currently running in preview workers.
    pub in_flight_playback_cursor_jobs: usize,
    /// Scrub cursor decode jobs currently running in preview workers.
    pub in_flight_scrub_cursor_jobs: usize,
    /// Random-access still-frame decode jobs currently running in preview workers.
    pub in_flight_random_access_still_jobs: usize,
    /// Jobs currently running on the single-worker fallback lane.
    pub in_flight_any_lane_jobs: usize,
    /// Jobs currently running on the playback-reserved worker lane.
    pub in_flight_playback_lane_jobs: usize,
    /// Jobs currently running on the scrub-reserved worker lane.
    pub in_flight_scrub_lane_jobs: usize,
    /// Jobs currently running on the still-frame-reserved worker lane.
    pub in_flight_still_lane_jobs: usize,
    /// Jobs currently running on the shared non-playback interactive lane.
    pub in_flight_interactive_lane_jobs: usize,
    /// Current-frame jobs running on a worker lane that would not normally
    /// accept their access mode. This is allowed as overflow for visible work,
    /// but persistent counts mean the lane split or worker budget is too tight.
    pub in_flight_cross_lane_current_jobs: usize,
}

/// Structured color-management rejection captured from the viewer preview path.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AppUiPreviewColorRejection {
    /// Asset that could not be interpreted for preview.
    pub asset_id: AssetId,
    /// Media path shown in diagnostics.
    pub path: PathBuf,
    /// Active missing-metadata policy that rejected the asset.
    pub missing_metadata_policy: MissingColorMetadataPolicy,
    /// Resolution branch that produced the rejection.
    pub source: InputColorResolutionSource,
    /// Clip/media color-space override in effect, if any.
    pub override_color_space: Option<ColorSpace>,
    /// Explicitly detected media color space, if any.
    pub detected_color_space: Option<ColorSpace>,
    /// Sequence working color space active during the decision.
    pub working_color_space: ColorSpace,
    /// Compact media color diagnostic summary from `mondrian-media`.
    pub diagnostic_summary: String,
    /// Machine-readable media color diagnostic issue summary.
    pub diagnostic_issue_summary: VideoColorDiagnosticIssueSummary,
}

/// Stable preview color-path health summary for perf JSONL and diagnostics tooling.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct AppUiPreviewColorHealthSummary {
    /// Preview composite plans represented by this snapshot.
    pub composite_plans: u64,
    /// Inputs resolved from detected media metadata.
    pub detected_metadata: u64,
    /// Inputs resolved from user overrides.
    pub override_count: u64,
    /// Inputs resolved by missing-metadata policy assumptions.
    pub policy_assumptions: u64,
    /// Inputs bypassing color management as data/utility textures.
    pub data_textures: u64,
    /// Inputs rejected by missing-metadata policy.
    pub policy_rejections: u64,
    /// Inputs resolved from explicit metadata or user overrides.
    pub explicit_metadata_or_override: u64,
    /// CPU input color-transform stages.
    pub cpu_input_stages: u64,
    /// CPU output/display color-transform stages.
    pub cpu_output_stages: u64,
    /// Native GPU color-transform stages.
    pub gpu_color_stages: u64,
    /// GPU scheduling blockers across native GPU color stages.
    pub gpu_blockers: u64,
    /// Structured native GPU blocker reasons.
    pub gpu_blocker_breakdown: RenderColorStageGpuBlockerBreakdown,
    /// Upload/readback transfer stages around color work.
    pub transfer_stages: u64,
    /// Temporary RGBA8 CPU boundary crossings observed by preview transforms.
    pub rgba8_boundary_calls: u64,
    /// Float/linear timeline composites.
    pub float_linear_composites: u64,
    /// Legacy RGBA8 timeline composites.
    pub legacy_rgba8_composites: u64,
    /// Structured legacy RGBA8 fallback reason count.
    pub legacy_reason_total: u64,
    /// Structured legacy RGBA8 fallback reasons.
    pub legacy_breakdown: TimelineCompositeLegacyBreakdown,
    /// Whether all diagnosed composites stayed in the float/linear path.
    pub fully_float_linear: bool,
    /// Whether native GPU color scheduling was free of upload/readback and blockers.
    pub gpu_path_ready: bool,
    /// Number of raster preview frames that used CPU output transform fallback.
    pub cpu_output_fallback_frames: u64,
    /// Pixels processed through CPU output transform fallback.
    pub cpu_output_fallback_pixels: u64,
    /// Structured GPU output blocker breakdown across preview frames.
    pub preview_gpu_output_blocker_breakdown:
        crate::app_ui::preview_gpu_output_blocker::PreviewGpuOutputBlockerBreakdown,
    /// GPU compositing capability diagnostics.
    pub gpu_compositing: mondrian_renderer::GpuCompositingDiagnostics,
}

/// Fixed latency buckets for compact preview decode distribution diagnostics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct AppUiPreviewDecodeLatencyBuckets {
    /// Samples at or below 10 ms.
    pub le_10ms: u64,
    /// Samples above 10 ms and at or below 16 ms.
    pub le_16ms: u64,
    /// Samples above 16 ms and at or below 25 ms.
    pub le_25ms: u64,
    /// Samples above 25 ms and at or below 40 ms.
    pub le_40ms: u64,
    /// Samples above 40 ms and at or below 50 ms.
    pub le_50ms: u64,
    /// Samples above 50 ms and at or below 80 ms.
    pub le_80ms: u64,
    /// Samples above 80 ms.
    pub gt_80ms: u64,
}

impl AppUiPreviewDecodeLatencyBuckets {
    fn record(&mut self, duration_us: u64) {
        match duration_us {
            0..=10_000 => self.le_10ms = self.le_10ms.saturating_add(1),
            10_001..=16_000 => self.le_16ms = self.le_16ms.saturating_add(1),
            16_001..=25_000 => self.le_25ms = self.le_25ms.saturating_add(1),
            25_001..=40_000 => self.le_40ms = self.le_40ms.saturating_add(1),
            40_001..=50_000 => self.le_50ms = self.le_50ms.saturating_add(1),
            50_001..=80_000 => self.le_80ms = self.le_80ms.saturating_add(1),
            _ => self.gt_80ms = self.gt_80ms.saturating_add(1),
        }
    }

    fn total(self) -> u64 {
        self.le_10ms
            .saturating_add(self.le_16ms)
            .saturating_add(self.le_25ms)
            .saturating_add(self.le_40ms)
            .saturating_add(self.le_50ms)
            .saturating_add(self.le_80ms)
            .saturating_add(self.gt_80ms)
    }

    fn estimated_p95_upper_bound_us(self) -> u64 {
        self.estimated_quantile_upper_bound_us(95)
    }

    fn estimated_quantile_upper_bound_us(self, percentile: u64) -> u64 {
        let total = self.total();
        if total == 0 {
            return 0;
        }
        let rank = total.saturating_mul(percentile.min(100)).saturating_add(99) / 100;
        let mut cumulative = 0_u64;
        for (count, upper_bound_us) in [
            (self.le_10ms, 10_000),
            (self.le_16ms, 16_000),
            (self.le_25ms, 25_000),
            (self.le_40ms, 40_000),
            (self.le_50ms, 50_000),
            (self.le_80ms, 80_000),
            (self.gt_80ms, 80_001),
        ] {
            cumulative = cumulative.saturating_add(count);
            if cumulative >= rank {
                return upper_bound_us;
            }
        }
        80_001
    }
}

/// Preview decode profile for one access mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct AppUiPreviewDecodeAccessModeProfile {
    /// Successful decode/cache results for this access mode.
    pub frames: u64,
    /// Successful in-process CPU RGBA results for this access mode.
    pub in_process_cpu_rgba_frames: u64,
    /// Successful external ffmpeg CPU RGBA results for this access mode.
    pub external_ffmpeg_cpu_rgba_frames: u64,
    /// Successful playback ring hits for this access mode.
    pub playback_session_ring_hit_frames: u64,
    /// Successful preview cache hits for this access mode.
    pub cache_hit_frames: u64,
    /// Total end-to-end decode duration for this access mode.
    pub total_duration_us: u64,
    /// Slowest end-to-end decode duration for this access mode.
    pub max_duration_us: u64,
    /// Most recent end-to-end decode duration for this access mode.
    pub last_duration_us: u64,
    /// Fixed distribution buckets for end-to-end decode duration.
    pub latency_buckets: AppUiPreviewDecodeLatencyBuckets,
    /// Total worker-queue wait before decode started for this access mode.
    pub queue_wait_total_us: u64,
    /// Slowest worker-queue wait before decode started for this access mode.
    pub queue_wait_max_us: u64,
    /// Most recent worker-queue wait before decode started for this access mode.
    pub queue_wait_last_us: u64,
    /// Fixed distribution buckets for worker-queue wait.
    pub queue_wait_buckets: AppUiPreviewDecodeLatencyBuckets,
    /// Canceled decode jobs for this access mode.
    pub canceled_jobs: u64,
    /// Canceled jobs caused by preview shutdown for this access mode.
    pub canceled_shutdown_jobs: u64,
    /// Canceled jobs caused by obsolete pending work for this access mode.
    pub canceled_obsolete_jobs: u64,
    /// Canceled prefetch jobs that exceeded their deadline for this access mode.
    pub canceled_prefetch_deadline_jobs: u64,
    /// Canceled current playback jobs that missed their display deadline.
    pub canceled_playback_deadline_jobs: u64,
    /// Canceled prefetch jobs that yielded to visible current-frame work.
    pub canceled_prefetch_preempted_jobs: u64,
    /// Canceled still-frame jobs that yielded to realtime current-frame work.
    pub canceled_still_preempted_jobs: u64,
    /// Canceled jobs without a structured reason for this access mode.
    pub canceled_unknown_jobs: u64,
    /// Total worker execution time spent in canceled decode jobs for this access mode.
    pub canceled_total_duration_us: u64,
    /// Slowest worker execution time for a canceled decode job in this access mode.
    pub canceled_max_duration_us: u64,
    /// Most recent canceled decode worker execution time in this access mode.
    pub canceled_last_duration_us: u64,
    /// Total latency after canceled jobs first observed cancellation for this access mode.
    pub canceled_return_latency_total_us: u64,
    /// Slowest latency after a canceled job first observed cancellation for this access mode.
    pub canceled_return_latency_max_us: u64,
    /// Most recent latency after a canceled job first observed cancellation for this access mode.
    pub canceled_return_latency_last_us: u64,
    /// Failed decode jobs for this access mode.
    pub failed_jobs: u64,
    /// Failed decode jobs caused by structured decode timeouts for this access mode.
    pub timeout_failures: u64,
    /// Failed decode jobs caused by forward-scan budget exhaustion for this access mode.
    pub budget_exhausted_failures: u64,
    /// Decode requests for this access mode that required a seek.
    pub seeked_frames: u64,
    /// Decode results that used keyframe-before exact seek semantics.
    pub keyframe_seek_strategy_frames: u64,
    /// Decode results that used bounded any-frame low-latency seek semantics.
    pub bounded_any_seek_strategy_frames: u64,
    /// Largest forward session reuse window reported by this access mode.
    pub forward_reuse_frame_window_max: u64,
    /// Largest forward scan budget reported by this access mode.
    pub forward_decode_budget_frames_max: u64,
    /// Largest bounded-any seek window reported by this access mode.
    pub any_seek_window_ms_max: u64,
    /// Decode results that reused an existing access-mode-local session.
    pub session_reused_frames: u64,
    /// Decode results that opened or replaced the access-mode-local session.
    pub session_opened_frames: u64,
    /// Decode results produced by continuing forward in an existing session without seeking.
    pub forward_reused_frames: u64,
    /// Decode results where the media session had observed keyframe seek-index evidence.
    pub seek_index_available_frames: u64,
    /// Decode results whose seek was bounded by a session-local keyframe index anchor.
    pub seek_index_used_frames: u64,
    /// Largest distinct keyframe count observed in the session-local seek index.
    pub seek_index_keyframes_max: u64,
    /// Largest video-packet observation count used to build session-local seek-index evidence.
    pub seek_index_observed_packets_max: u64,
    /// Decode results whose keyframe index was seeded from container/probe metadata.
    pub seek_index_probe_backed_frames: u64,
    /// Decode results whose keyframe index was learned from packets decoded in-session.
    pub seek_index_session_observed_frames: u64,
    /// Decode results that reported active hardware decode at the media boundary.
    pub hardware_decode_active_frames: u64,
    /// Decode results that reported zero-copy decoded-frame residency.
    pub zero_copy_active_frames: u64,
    /// Decode results whose media frame residency was GPU texture backed.
    pub gpu_texture_resident_frames: u64,
    /// Decode results whose source decoder surface was NV12.
    pub decoded_nv12_surface_frames: u64,
    /// Decode results whose source decoder surface was P010.
    pub decoded_p010_surface_frames: u64,
    /// Decode results whose native decoded-frame renderer import was ready.
    pub renderer_import_ready_frames: u64,
    /// Decode results blocked because hardware texture residency is not connected.
    pub hardware_decode_texture_residency_blocker_frames: u64,
    /// Decode requests that preferred GPU-resident native frames.
    pub hardware_decode_prefer_gpu_requested_frames: u64,
    /// Decode requests that required GPU-resident native frames.
    pub hardware_decode_require_gpu_requested_frames: u64,
    /// Decode requests where no hardware-resident path was requested.
    pub hardware_decode_auto_requested_frames: u64,
    /// Decode requests that selected CPU RGBA because hardware was not requested.
    pub hardware_decode_cpu_not_requested_frames: u64,
    /// Decode requests that selected CPU RGBA because hardware residency is unavailable.
    pub hardware_decode_cpu_unavailable_frames: u64,
    /// Decode requests that selected CPU RGBA because the access mode cannot use hardware decode.
    pub hardware_decode_access_mode_unsupported_frames: u64,
    /// Decode requests blocked at a backend that cannot return native GPU residency.
    pub hardware_decode_backend_unavailable_frames: u64,
    /// Decode requests blocked at a CPU RGBA backend boundary.
    pub hardware_decode_backend_boundary_frames: u64,
    /// Decode requests blocked because renderer import is unavailable.
    pub hardware_decode_renderer_import_unavailable_frames: u64,
    /// Decode requests that selected GPU-resident native decode.
    pub hardware_decode_gpu_resident_native_frames: u64,
    /// Decode requests with a D3D11VA backend candidate.
    pub hardware_decode_candidate_d3d11va_frames: u64,
    /// Decode requests with a VideoToolbox backend candidate.
    pub hardware_decode_candidate_videotoolbox_frames: u64,
    /// Decode requests with a VA-API backend candidate.
    pub hardware_decode_candidate_vaapi_frames: u64,
    /// Decode requests with a CUDA/NVDEC backend candidate.
    pub hardware_decode_candidate_cuda_frames: u64,
    /// Decode requests where a backend candidate exists but no decoder adapter is connected.
    pub hardware_decode_adapter_unavailable_frames: u64,
    /// Total decoded frames consumed by this access mode.
    pub decoded_frame_count: u64,
    /// Largest decoded-frame count consumed by one request in this access mode.
    pub max_decoded_frame_count: u64,
    /// Aggregated media-layer stage timings for this access mode.
    pub stage_durations: PreviewDecodeStageDurations,
    /// Stage timings from the slowest frame in this access mode.
    pub max_frame_stage_durations: PreviewDecodeStageDurations,
    /// Queue wait from the same slowest frame in this access mode.
    pub max_frame_queue_wait_us: u64,
    /// Dominant bottleneck from the same slowest frame in this access mode.
    pub max_frame_bottleneck: AppUiPreviewDecodeBottleneck,
}

impl AppUiPreviewDecodeAccessModeProfile {
    fn mode_local_evidence_frames(self) -> u64 {
        self.frames.saturating_sub(self.cache_hit_frames)
    }

    fn record(&mut self, diagnostics: PreviewDecodeDiagnostics, queue_wait_us: u64) {
        self.frames = self.frames.saturating_add(1);
        match diagnostics.path {
            PreviewDecodePath::InProcessFfmpegCpuRgba => {
                self.in_process_cpu_rgba_frames = self.in_process_cpu_rgba_frames.saturating_add(1);
            }
            PreviewDecodePath::ExternalFfmpegCpuRgba => {
                self.external_ffmpeg_cpu_rgba_frames =
                    self.external_ffmpeg_cpu_rgba_frames.saturating_add(1);
            }
            PreviewDecodePath::PlaybackSessionRingHit => {
                self.playback_session_ring_hit_frames =
                    self.playback_session_ring_hit_frames.saturating_add(1);
            }
            PreviewDecodePath::PreviewCacheHit => {
                self.cache_hit_frames = self.cache_hit_frames.saturating_add(1);
            }
        }
        self.total_duration_us = self.total_duration_us.saturating_add(diagnostics.elapsed_us);
        self.latency_buckets.record(diagnostics.elapsed_us);
        if diagnostics.elapsed_us >= self.max_duration_us {
            self.max_duration_us = diagnostics.elapsed_us;
            self.max_frame_stage_durations = diagnostics.stage_durations;
            self.max_frame_queue_wait_us = queue_wait_us;
            self.max_frame_bottleneck =
                classify_preview_decode_bottleneck(diagnostics.stage_durations, queue_wait_us);
        }
        self.last_duration_us = diagnostics.elapsed_us;
        if diagnostics.seek_performed {
            self.seeked_frames = self.seeked_frames.saturating_add(1);
        }
        match diagnostics.seek_strategy {
            PreviewDecodeSeekStrategy::KeyframeBefore => {
                self.keyframe_seek_strategy_frames =
                    self.keyframe_seek_strategy_frames.saturating_add(1);
            }
            PreviewDecodeSeekStrategy::BoundedAnyFrame => {
                self.bounded_any_seek_strategy_frames =
                    self.bounded_any_seek_strategy_frames.saturating_add(1);
            }
        }
        self.forward_reuse_frame_window_max = self
            .forward_reuse_frame_window_max
            .max(diagnostics.forward_reuse_frame_window.max(0) as u64);
        self.forward_decode_budget_frames_max = self
            .forward_decode_budget_frames_max
            .max(u64::from(diagnostics.forward_decode_budget_frames));
        self.any_seek_window_ms_max =
            self.any_seek_window_ms_max.max(diagnostics.any_seek_window_ms);
        if diagnostics.session_reused {
            self.session_reused_frames = self.session_reused_frames.saturating_add(1);
        } else {
            self.session_opened_frames = self.session_opened_frames.saturating_add(1);
        }
        if diagnostics.forward_reused {
            self.forward_reused_frames = self.forward_reused_frames.saturating_add(1);
        }
        if diagnostics.seek_index_available {
            self.seek_index_available_frames = self.seek_index_available_frames.saturating_add(1);
        }
        if diagnostics.seek_index_used {
            self.seek_index_used_frames = self.seek_index_used_frames.saturating_add(1);
        }
        self.seek_index_keyframes_max =
            self.seek_index_keyframes_max.max(u64::from(diagnostics.seek_index_keyframes));
        self.seek_index_observed_packets_max = self
            .seek_index_observed_packets_max
            .max(u64::from(diagnostics.seek_index_observed_packets));
        match diagnostics.seek_index_source {
            PreviewSeekIndexSource::None => {}
            PreviewSeekIndexSource::ProbeBacked => {
                self.seek_index_probe_backed_frames =
                    self.seek_index_probe_backed_frames.saturating_add(1);
            }
            PreviewSeekIndexSource::SessionObserved => {
                self.seek_index_session_observed_frames =
                    self.seek_index_session_observed_frames.saturating_add(1);
            }
        }
        if diagnostics.hardware_decode_active {
            self.hardware_decode_active_frames =
                self.hardware_decode_active_frames.saturating_add(1);
        }
        if diagnostics.zero_copy_active {
            self.zero_copy_active_frames = self.zero_copy_active_frames.saturating_add(1);
        }
        if diagnostics.decoded_frame_residency == DecodedFrameResidency::GpuTexture {
            self.gpu_texture_resident_frames = self.gpu_texture_resident_frames.saturating_add(1);
        }
        match diagnostics.decoded_surface_format {
            DecodedVideoSurfaceFormat::Nv12 => {
                self.decoded_nv12_surface_frames =
                    self.decoded_nv12_surface_frames.saturating_add(1);
            }
            DecodedVideoSurfaceFormat::P010 => {
                self.decoded_p010_surface_frames =
                    self.decoded_p010_surface_frames.saturating_add(1);
            }
            _ => {}
        }
        if diagnostics.renderer_import_ready {
            self.renderer_import_ready_frames = self.renderer_import_ready_frames.saturating_add(1);
        }
        if diagnostics.hardware_decode_blocker
            == PreviewHardwareDecodeBlocker::TextureResidencyNotConnected
        {
            self.hardware_decode_texture_residency_blocker_frames =
                self.hardware_decode_texture_residency_blocker_frames.saturating_add(1);
        }
        match diagnostics.hardware_decode_request {
            PreviewHardwareDecodeRequest::Auto => {
                self.hardware_decode_auto_requested_frames =
                    self.hardware_decode_auto_requested_frames.saturating_add(1);
            }
            PreviewHardwareDecodeRequest::PreferGpuResident => {
                self.hardware_decode_prefer_gpu_requested_frames =
                    self.hardware_decode_prefer_gpu_requested_frames.saturating_add(1);
            }
            PreviewHardwareDecodeRequest::RequireGpuResident => {
                self.hardware_decode_require_gpu_requested_frames =
                    self.hardware_decode_require_gpu_requested_frames.saturating_add(1);
            }
        }
        match diagnostics.hardware_decode_decision {
            PreviewHardwareDecodeDecision::CpuRgbaNotRequested => {
                self.hardware_decode_cpu_not_requested_frames =
                    self.hardware_decode_cpu_not_requested_frames.saturating_add(1);
            }
            PreviewHardwareDecodeDecision::CpuRgbaHardwareUnavailable => {
                self.hardware_decode_cpu_unavailable_frames =
                    self.hardware_decode_cpu_unavailable_frames.saturating_add(1);
            }
            PreviewHardwareDecodeDecision::CpuRgbaAccessModeUnsupported => {
                self.hardware_decode_access_mode_unsupported_frames =
                    self.hardware_decode_access_mode_unsupported_frames.saturating_add(1);
            }
            PreviewHardwareDecodeDecision::CpuRgbaBackendUnavailable => {
                self.hardware_decode_backend_unavailable_frames =
                    self.hardware_decode_backend_unavailable_frames.saturating_add(1);
            }
            PreviewHardwareDecodeDecision::CpuRgbaRendererImportUnavailable => {
                self.hardware_decode_renderer_import_unavailable_frames =
                    self.hardware_decode_renderer_import_unavailable_frames.saturating_add(1);
            }
            PreviewHardwareDecodeDecision::CpuRgbaBackendBoundary => {
                self.hardware_decode_backend_boundary_frames =
                    self.hardware_decode_backend_boundary_frames.saturating_add(1);
            }
            PreviewHardwareDecodeDecision::GpuResidentNative => {
                self.hardware_decode_gpu_resident_native_frames =
                    self.hardware_decode_gpu_resident_native_frames.saturating_add(1);
            }
        }
        match diagnostics.hardware_decode_candidate_backend {
            Some(HwAccelBackend::D3D11VA) => {
                self.hardware_decode_candidate_d3d11va_frames =
                    self.hardware_decode_candidate_d3d11va_frames.saturating_add(1);
            }
            Some(HwAccelBackend::VideoToolbox) => {
                self.hardware_decode_candidate_videotoolbox_frames =
                    self.hardware_decode_candidate_videotoolbox_frames.saturating_add(1);
            }
            Some(HwAccelBackend::Vaapi) => {
                self.hardware_decode_candidate_vaapi_frames =
                    self.hardware_decode_candidate_vaapi_frames.saturating_add(1);
            }
            Some(HwAccelBackend::Cuda) => {
                self.hardware_decode_candidate_cuda_frames =
                    self.hardware_decode_candidate_cuda_frames.saturating_add(1);
            }
            Some(HwAccelBackend::None) | None => {}
        }
        if diagnostics.hardware_decode_candidate_backend.is_some()
            && !diagnostics.hardware_decode_adapter_available
        {
            self.hardware_decode_adapter_unavailable_frames =
                self.hardware_decode_adapter_unavailable_frames.saturating_add(1);
        }
        let decoded_frame_count = u64::from(diagnostics.decoded_frame_count);
        self.decoded_frame_count = self.decoded_frame_count.saturating_add(decoded_frame_count);
        self.max_decoded_frame_count = self.max_decoded_frame_count.max(decoded_frame_count);
        self.stage_durations.accumulate(diagnostics.stage_durations);
    }

    fn record_queue_wait(&mut self, queue_wait_us: u64) {
        self.queue_wait_total_us = self.queue_wait_total_us.saturating_add(queue_wait_us);
        self.queue_wait_max_us = self.queue_wait_max_us.max(queue_wait_us);
        self.queue_wait_last_us = queue_wait_us;
        self.queue_wait_buckets.record(queue_wait_us);
    }

    fn record_cancel(
        &mut self,
        reason: MediaPreviewCancelReason,
        elapsed_us: u64,
        return_latency_us: u64,
    ) {
        self.canceled_jobs = self.canceled_jobs.saturating_add(1);
        match reason {
            MediaPreviewCancelReason::Shutdown => {
                self.canceled_shutdown_jobs = self.canceled_shutdown_jobs.saturating_add(1);
            }
            MediaPreviewCancelReason::Obsolete => {
                self.canceled_obsolete_jobs = self.canceled_obsolete_jobs.saturating_add(1);
            }
            MediaPreviewCancelReason::PrefetchDeadline => {
                self.canceled_prefetch_deadline_jobs =
                    self.canceled_prefetch_deadline_jobs.saturating_add(1);
            }
            MediaPreviewCancelReason::PlaybackDeadline => {
                self.canceled_playback_deadline_jobs =
                    self.canceled_playback_deadline_jobs.saturating_add(1);
            }
            MediaPreviewCancelReason::PrefetchPreemptedByCurrent => {
                self.canceled_prefetch_preempted_jobs =
                    self.canceled_prefetch_preempted_jobs.saturating_add(1);
            }
            MediaPreviewCancelReason::StillPreemptedByRealtimeCurrent => {
                self.canceled_still_preempted_jobs =
                    self.canceled_still_preempted_jobs.saturating_add(1);
            }
            MediaPreviewCancelReason::Unknown => {
                self.canceled_unknown_jobs = self.canceled_unknown_jobs.saturating_add(1);
            }
        }
        self.canceled_total_duration_us =
            self.canceled_total_duration_us.saturating_add(elapsed_us);
        self.canceled_max_duration_us = self.canceled_max_duration_us.max(elapsed_us);
        self.canceled_last_duration_us = elapsed_us;
        self.canceled_return_latency_total_us =
            self.canceled_return_latency_total_us.saturating_add(return_latency_us);
        self.canceled_return_latency_max_us =
            self.canceled_return_latency_max_us.max(return_latency_us);
        self.canceled_return_latency_last_us = return_latency_us;
    }

    fn record_failure(&mut self, reason: MediaPreviewFailureReason) {
        self.failed_jobs = self.failed_jobs.saturating_add(1);
        match reason {
            MediaPreviewFailureReason::Timeout => {
                self.timeout_failures = self.timeout_failures.saturating_add(1);
            }
            MediaPreviewFailureReason::ForwardDecodeBudgetExhausted => {
                self.budget_exhausted_failures = self.budget_exhausted_failures.saturating_add(1);
            }
            MediaPreviewFailureReason::DecodeError => {}
        }
    }
}

/// Preview decode profiles split by access mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct AppUiPreviewDecodeAccessModeProfiles {
    /// Sustained playback and forward-prefetch decode profile.
    pub playback_cursor: AppUiPreviewDecodeAccessModeProfile,
    /// Latest-wins interactive scrub decode profile.
    pub scrub_cursor: AppUiPreviewDecodeAccessModeProfile,
    /// Deterministic still-frame/random-access decode profile.
    pub random_access_still: AppUiPreviewDecodeAccessModeProfile,
}

impl AppUiPreviewDecodeAccessModeProfiles {
    fn profile_for(
        self,
        access_mode: PreviewDecodeAccessMode,
    ) -> AppUiPreviewDecodeAccessModeProfile {
        match access_mode {
            PreviewDecodeAccessMode::PlaybackCursor => self.playback_cursor,
            PreviewDecodeAccessMode::ScrubCursor => self.scrub_cursor,
            PreviewDecodeAccessMode::RandomAccessStillFrame => self.random_access_still,
        }
    }

    fn record(&mut self, diagnostics: PreviewDecodeDiagnostics, queue_wait_us: u64) {
        match diagnostics.access_mode {
            PreviewDecodeAccessMode::PlaybackCursor => {
                self.playback_cursor.record(diagnostics, queue_wait_us);
            }
            PreviewDecodeAccessMode::ScrubCursor => {
                self.scrub_cursor.record(diagnostics, queue_wait_us);
            }
            PreviewDecodeAccessMode::RandomAccessStillFrame => {
                self.random_access_still.record(diagnostics, queue_wait_us);
            }
        }
    }

    fn record_queue_wait(&mut self, access_mode: PreviewDecodeAccessMode, queue_wait_us: u64) {
        match access_mode {
            PreviewDecodeAccessMode::PlaybackCursor => {
                self.playback_cursor.record_queue_wait(queue_wait_us);
            }
            PreviewDecodeAccessMode::ScrubCursor => {
                self.scrub_cursor.record_queue_wait(queue_wait_us);
            }
            PreviewDecodeAccessMode::RandomAccessStillFrame => {
                self.random_access_still.record_queue_wait(queue_wait_us);
            }
        }
    }

    fn record_cancel(
        &mut self,
        access_mode: PreviewDecodeAccessMode,
        reason: MediaPreviewCancelReason,
        elapsed_us: u64,
        return_latency_us: u64,
    ) {
        match access_mode {
            PreviewDecodeAccessMode::PlaybackCursor => {
                self.playback_cursor.record_cancel(reason, elapsed_us, return_latency_us);
            }
            PreviewDecodeAccessMode::ScrubCursor => {
                self.scrub_cursor.record_cancel(reason, elapsed_us, return_latency_us);
            }
            PreviewDecodeAccessMode::RandomAccessStillFrame => {
                self.random_access_still.record_cancel(reason, elapsed_us, return_latency_us);
            }
        }
    }

    fn record_failure(
        &mut self,
        access_mode: PreviewDecodeAccessMode,
        reason: MediaPreviewFailureReason,
    ) {
        match access_mode {
            PreviewDecodeAccessMode::PlaybackCursor => {
                self.playback_cursor.record_failure(reason);
            }
            PreviewDecodeAccessMode::ScrubCursor => {
                self.scrub_cursor.record_failure(reason);
            }
            PreviewDecodeAccessMode::RandomAccessStillFrame => {
                self.random_access_still.record_failure(reason);
            }
        }
    }

    fn slowest_access_mode(self) -> Option<PreviewDecodeAccessMode> {
        self.named_profiles()
            .into_iter()
            .filter(|(_, profile)| profile.max_duration_us > 0)
            .max_by_key(|(_, profile)| profile.max_duration_us)
            .map(|(access_mode, _)| access_mode)
    }

    fn named_profiles(self) -> [(PreviewDecodeAccessMode, AppUiPreviewDecodeAccessModeProfile); 3] {
        [
            (
                PreviewDecodeAccessMode::PlaybackCursor,
                self.playback_cursor,
            ),
            (PreviewDecodeAccessMode::ScrubCursor, self.scrub_cursor),
            (
                PreviewDecodeAccessMode::RandomAccessStillFrame,
                self.random_access_still,
            ),
        ]
    }
}

/// Stable preview decode performance summary for perf JSONL and diagnostics tooling.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct AppUiPreviewDecodePerformanceSummary {
    /// Coordinated CPU budget used for preview workers and FFmpeg decoder threads.
    pub cpu_budget: PreviewDecodeCpuBudget,
    /// Successful preview decode/cache results.
    pub decode_successes: u64,
    /// Failed preview decode results.
    pub decode_failures: u64,
    /// Failed preview decode results caused by structured decode timeouts.
    pub decode_timeout_failures: u64,
    /// Failed preview decode results caused by access-mode forward-scan budget exhaustion.
    pub decode_budget_exhausted_failures: u64,
    /// Canceled preview decode jobs.
    pub canceled_jobs: u64,
    /// Canceled preview decode jobs caused by shutdown.
    pub canceled_shutdown_jobs: u64,
    /// Canceled preview decode jobs caused by obsolete pending work.
    pub canceled_obsolete_jobs: u64,
    /// Prefetch decode jobs canceled by their deadline.
    pub canceled_prefetch_deadline_jobs: u64,
    /// Current playback decode jobs canceled because their display deadline was missed.
    pub canceled_playback_deadline_jobs: u64,
    /// Prefetch decode jobs canceled because visible current-frame work was pending.
    pub canceled_prefetch_preempted_jobs: u64,
    /// Still-frame decode jobs canceled because realtime current-frame work was pending.
    pub canceled_still_preempted_jobs: u64,
    /// Canceled preview decode jobs with no structured reason.
    pub canceled_unknown_jobs: u64,
    /// Total worker execution time spent in canceled decode jobs.
    pub canceled_total_duration_us: u64,
    /// Slowest worker execution time for a canceled decode job.
    pub canceled_max_duration_us: u64,
    /// Most recent worker execution time for a canceled decode job.
    pub canceled_last_duration_us: u64,
    /// Total latency after canceled decode jobs first observed cancellation.
    pub canceled_return_latency_total_us: u64,
    /// Slowest latency after a canceled decode job first observed cancellation.
    pub canceled_return_latency_max_us: u64,
    /// Most recent latency after a canceled decode job first observed cancellation.
    pub canceled_return_latency_last_us: u64,
    /// Successful decodes served from the in-process FFmpeg CPU RGBA path.
    pub in_process_cpu_rgba_frames: u64,
    /// Successful decodes served from the external ffmpeg CPU RGBA path.
    pub external_ffmpeg_cpu_rgba_frames: u64,
    /// Successful playback decodes served from the playback session-local ring.
    pub playback_session_ring_hit_frames: u64,
    /// Successful decodes served from preview cache.
    pub cache_hit_frames: u64,
    /// Decode results requested through the playback cursor access contract.
    pub playback_cursor_frames: u64,
    /// Decode results requested through the scrub cursor access contract.
    pub scrub_cursor_frames: u64,
    /// Decode results requested through the random-access still-frame contract.
    pub random_access_still_frames: u64,
    /// Canceled decode jobs requested through the playback cursor access contract.
    pub canceled_playback_cursor_jobs: u64,
    /// Canceled decode jobs requested through the scrub cursor access contract.
    pub canceled_scrub_cursor_jobs: u64,
    /// Canceled decode jobs requested through the random-access still-frame contract.
    pub canceled_random_access_still_jobs: u64,
    /// Maximum end-to-end decode duration.
    pub max_duration_us: u64,
    /// Most recent end-to-end decode duration.
    pub last_duration_us: u64,
    /// Total end-to-end decode duration.
    pub total_duration_us: u64,
    /// Slow-frame budget applied by the report.
    pub slow_frame_budget_us: u64,
    /// Total time decoded jobs spent waiting in the preview worker queue.
    pub queue_wait_total_us: u64,
    /// Slowest decoded job queue wait.
    pub queue_wait_max_us: u64,
    /// Most recent decoded job queue wait.
    pub queue_wait_last_us: u64,
    /// Slowest current-frame decode queue wait.
    pub current_queue_wait_max_us: u64,
    /// Slowest prefetch decode queue wait.
    pub prefetch_queue_wait_max_us: u64,
    /// Media preview jobs accepted by the worker queue.
    pub enqueued_jobs: u64,
    /// Playback prefetch passes skipped while visible current-frame work was pending.
    pub prefetch_skipped_current_pending: u64,
    /// Playback prefetch passes skipped while current-frame worker work was queued or running.
    pub prefetch_skipped_worker_busy: u64,
    /// Playback prefetch passes skipped because queued/running prefetch already covered the window.
    pub prefetch_skipped_prefetch_backlog: u64,
    /// Jobs dropped because the bounded worker queue was full.
    pub queue_full_drops: u64,
    /// Jobs rejected by the worker queue for invalid priority/access-mode pairs.
    pub queue_invalid_access_mode_drops: u64,
    /// Queued prefetch jobs evicted so current-frame decode can run.
    pub queue_evicted_prefetch_jobs: u64,
    /// Queued still-frame jobs evicted so real-time current work can run.
    pub queue_evicted_still_jobs: u64,
    /// User escape-path cancellations requested by transport, close, or quit.
    pub interactive_cancel_requests: u64,
    /// Scheduler pending requests canceled by user escape-path cancellations.
    pub interactive_cancel_scheduler_requests: u64,
    /// Worker-queue jobs cleared by user escape-path cancellations.
    pub interactive_cancel_queued_jobs: u64,
    /// Queued jobs removed because their scheduler-side pending request was canceled.
    pub queue_canceled_jobs: u64,
    /// Playback buffering releases caused by a stalled realtime current-frame request.
    pub playback_current_stalled_expirations: u64,
    /// Obsolete queued jobs removed before scheduling current-frame decode.
    pub queue_pruned_obsolete_jobs: u64,
    /// Queued prefetch jobs promoted after the same key became current-frame work.
    pub queue_promoted_current_jobs: u64,
    /// Jobs dropped because preview workers were unavailable.
    pub worker_disconnected_drops: u64,
    /// Current worker transport queue depth grouped by scheduling contract.
    pub worker_queue: MediaPreviewJobQueueDiagnostics,
    /// Current preview worker in-flight decode activity grouped by scheduling contract.
    pub worker_activity: PreviewWorkerActivityDiagnostics,
    /// Decode requests that required a seek.
    pub seeked_frames: u64,
    /// Total decoded frames consumed before frame selection.
    pub decoded_frame_count: u64,
    /// Maximum decoded frames consumed by one request.
    pub max_decoded_frame_count: u64,
    /// Aggregated media-layer decode stage timings.
    pub stage_durations: PreviewDecodeStageDurations,
    /// Stage timings from the slowest decode frame.
    pub max_frame_stage_durations: PreviewDecodeStageDurations,
    /// Queue wait from the same slowest decode frame.
    pub max_frame_queue_wait_us: u64,
    /// Decode profile split by playback, scrub, and random-access still modes.
    pub access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles,
    /// Access mode that produced the slowest successful decode frame.
    pub slowest_access_mode: Option<PreviewDecodeAccessMode>,
    /// Dominant stage inferred from the slowest successful decode frame.
    pub primary_bottleneck: AppUiPreviewDecodeBottleneck,
    /// Scheduler-side access-mode/drop/stale diagnostics captured with decode evidence.
    pub scheduler: MediaPreviewSchedulerDiagnostics,
    /// Playback-clock deadline and forward-prefetch scheduling contract.
    pub playback_schedule: AppUiPreviewPlaybackScheduleDiagnostics,
}

/// Dominant preview decode bottleneck inferred from stage diagnostics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub enum AppUiPreviewDecodeBottleneck {
    /// No decode evidence was captured.
    #[default]
    None,
    /// Opening or reconfiguring the decode session dominated.
    SessionOpen,
    /// Waiting in the preview decode worker queue dominated.
    QueueWait,
    /// Cache lookup dominated.
    CacheLookup,
    /// Seek and decoder flush dominated.
    Seek,
    /// Packet demux/decode dominated.
    PacketDecode,
    /// FFmpeg software scale or CPU RGBA copy dominated.
    CpuRgbaBoundary,
    /// External ffmpeg process wait dominated.
    ExternalProcess,
}

/// Schema version for preview decode performance reports.
pub const APP_UI_PREVIEW_DECODE_PERFORMANCE_REPORT_SCHEMA_VERSION: u32 = 25;

/// Default preview slow-frame budget: one frame should complete in tens of ms.
pub const APP_UI_PREVIEW_DECODE_DEFAULT_SLOW_FRAME_BUDGET_US: u64 = 50_000;

/// Versioned preview decode performance report for UI, telemetry, and perf artifacts.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AppUiPreviewDecodePerformanceReport {
    /// Report schema version.
    pub schema_version: u32,
    /// Applied report profile.
    pub profile: String,
    /// Overall preview decode performance verdict.
    pub verdict: AppUiPreviewDecodePerformanceVerdict,
    /// Access modes this report profile required to be sampled.
    pub required_access_modes: Vec<PreviewDecodeAccessMode>,
    /// Structured decode performance summary used as report evidence.
    pub summary: Option<AppUiPreviewDecodePerformanceSummary>,
    /// Structured checks by preview decode area.
    pub checks: Vec<AppUiPreviewDecodePerformanceCheck>,
    /// Prioritized machine-readable root causes.
    pub root_causes: Vec<AppUiPreviewDecodePerformanceRootCause>,
    /// Suggested engineering or operator actions.
    pub actions: Vec<AppUiPreviewDecodePerformanceAction>,
}

/// Stable preview render performance summary for post-decode viewer work.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct AppUiPreviewRenderPerformanceSummary {
    /// Viewer render requests with timing evidence.
    pub timed_frames: u64,
    /// Maximum post-decode render duration.
    pub max_duration_us: u64,
    /// Most recent post-decode render duration.
    pub last_duration_us: u64,
    /// Total post-decode render duration.
    pub total_duration_us: u64,
    /// Slow-frame budget applied by the report.
    pub slow_frame_budget_us: u64,
    /// Aggregated post-decode render stage timings.
    pub stage_durations: AppUiPreviewRenderStageDurations,
    /// Stage timings from the slowest post-decode render frame.
    pub max_frame_stage_durations: AppUiPreviewRenderStageDurations,
    /// Dominant post-decode render bottleneck inferred from the slowest-frame timings.
    pub primary_bottleneck: AppUiPreviewRenderBottleneck,
}

/// Dominant post-decode preview render bottleneck.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub enum AppUiPreviewRenderBottleneck {
    /// No post-decode render evidence was captured.
    #[default]
    None,
    /// Sequence/plan/media readiness resolution dominated.
    Resolve,
    /// Final viewer/external frame cache lookup dominated.
    FinalCacheLookup,
    /// Working-frame preparation dominated.
    WorkingPreparation,
    /// CPU timeline compositing and property/effect work dominated.
    CpuComposite,
    /// CPU output/color boundary dominated.
    CpuOutputBoundary,
    /// Final raster frame packaging dominated.
    FramePackaging,
}

/// Schema version for preview render performance reports.
pub const APP_UI_PREVIEW_RENDER_PERFORMANCE_REPORT_SCHEMA_VERSION: u32 = 1;

/// Default post-decode viewer render budget: one frame should complete in tens of ms.
pub const APP_UI_PREVIEW_RENDER_DEFAULT_SLOW_FRAME_BUDGET_US: u64 = 50_000;

/// Versioned preview render performance report for UI, telemetry, and perf artifacts.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AppUiPreviewRenderPerformanceReport {
    /// Report schema version.
    pub schema_version: u32,
    /// Applied report profile.
    pub profile: String,
    /// Overall post-decode render performance verdict.
    pub verdict: AppUiPreviewRenderPerformanceVerdict,
    /// Structured render performance summary used as report evidence.
    pub summary: Option<AppUiPreviewRenderPerformanceSummary>,
    /// Structured checks by preview render area.
    pub checks: Vec<AppUiPreviewRenderPerformanceCheck>,
    /// Prioritized machine-readable root causes.
    pub root_causes: Vec<AppUiPreviewRenderPerformanceRootCause>,
    /// Suggested engineering or operator actions.
    pub actions: Vec<AppUiPreviewRenderPerformanceAction>,
}

/// Overall post-decode preview render performance verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum AppUiPreviewRenderPerformanceVerdict {
    /// Preview render met the applied performance budget.
    Pass,
    /// Preview render violated the budget or had no evidence.
    Fail,
}

/// Preview render performance diagnostic area.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum AppUiPreviewRenderPerformanceArea {
    /// Evidence capture and summary availability.
    CaptureIntegrity,
    /// End-to-end post-decode render latency budget.
    LatencyBudget,
    /// Sequence/plan/media readiness resolution.
    Resolve,
    /// Final viewer/external frame cache lookup.
    FinalCacheLookup,
    /// Working-frame preparation before CPU composition.
    WorkingPreparation,
    /// CPU timeline compositing and property/effect work.
    CpuComposite,
    /// CPU output/color boundary.
    CpuOutputBoundary,
    /// Final raster frame packaging.
    FramePackaging,
}

/// Preview render performance check severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum AppUiPreviewRenderPerformanceSeverity {
    /// Check passed.
    Pass,
    /// Check failed.
    Fail,
}

/// One preview render performance check.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AppUiPreviewRenderPerformanceCheck {
    /// Diagnostic area for this check.
    pub area: AppUiPreviewRenderPerformanceArea,
    /// Stable check code.
    pub code: &'static str,
    /// Check severity.
    pub severity: AppUiPreviewRenderPerformanceSeverity,
    /// Observed value.
    pub observed: u64,
    /// Optional target or threshold.
    pub limit: Option<u64>,
}

/// One preview render performance root cause.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AppUiPreviewRenderPerformanceRootCause {
    /// Diagnostic area for this root cause.
    pub area: AppUiPreviewRenderPerformanceArea,
    /// Stable root-cause code.
    pub code: &'static str,
    /// Root-cause severity.
    pub severity: AppUiPreviewRenderPerformanceSeverity,
    /// Compact evidence string.
    pub evidence: String,
}

/// One preview render performance action.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AppUiPreviewRenderPerformanceAction {
    /// Diagnostic area for this action.
    pub area: AppUiPreviewRenderPerformanceArea,
    /// Stable action code.
    pub code: &'static str,
    /// Human-readable action.
    pub description: &'static str,
}

/// Overall preview decode performance verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum AppUiPreviewDecodePerformanceVerdict {
    /// Preview decode met the applied performance budget.
    Pass,
    /// Preview decode has warning evidence but no hard budget failure.
    Warn,
    /// Preview decode violated the applied performance budget or had no evidence.
    Fail,
}

/// Preview decode performance diagnostic area.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum AppUiPreviewDecodePerformanceArea {
    /// Evidence capture and summary availability.
    CaptureIntegrity,
    /// End-to-end decode latency budget.
    LatencyBudget,
    /// Playback, scrub, and still-frame access-mode-specific decode budgets.
    AccessMode,
    /// Random access, seeking, and GOP pressure.
    RandomAccess,
    /// Codec packet/decode work.
    CodecDecode,
    /// CPU RGBA software scale/copy boundary.
    CpuRgbaBoundary,
    /// Proxy/cache readiness.
    ProxyCache,
    /// Preview decode worker queue scheduling.
    Scheduling,
    /// External process decode path.
    ExternalProcess,
}

/// Preview decode performance check severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum AppUiPreviewDecodePerformanceSeverity {
    /// Check passed.
    Pass,
    /// Check produced warning evidence.
    Warn,
    /// Check failed.
    Fail,
}

/// One preview decode performance check.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AppUiPreviewDecodePerformanceCheck {
    /// Diagnostic area for this check.
    pub area: AppUiPreviewDecodePerformanceArea,
    /// Stable check code.
    pub code: &'static str,
    /// Check severity.
    pub severity: AppUiPreviewDecodePerformanceSeverity,
    /// Observed value.
    pub observed: u64,
    /// Optional target or threshold.
    pub limit: Option<u64>,
}

/// One preview decode performance root cause.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AppUiPreviewDecodePerformanceRootCause {
    /// Diagnostic area for this root cause.
    pub area: AppUiPreviewDecodePerformanceArea,
    /// Stable root-cause code.
    pub code: &'static str,
    /// Root-cause severity.
    pub severity: AppUiPreviewDecodePerformanceSeverity,
    /// Compact evidence string.
    pub evidence: String,
}

/// One preview decode performance action.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AppUiPreviewDecodePerformanceAction {
    /// Diagnostic area for this action.
    pub area: AppUiPreviewDecodePerformanceArea,
    /// Stable action code.
    pub code: &'static str,
    /// Human-readable action.
    pub description: &'static str,
}

impl AppUiPreviewDecodePerformanceSummary {
    /// Build the versioned preview decode performance report for this summary.
    pub fn performance_report(
        self,
        profile: impl Into<String>,
    ) -> AppUiPreviewDecodePerformanceReport {
        build_preview_decode_performance_report(
            Some(self),
            profile,
            APP_UI_PREVIEW_DECODE_DEFAULT_SLOW_FRAME_BUDGET_US,
        )
    }
}

impl AppUiPreviewRenderPerformanceSummary {
    /// Build the versioned preview render performance report for this summary.
    pub fn performance_report(
        self,
        profile: impl Into<String>,
    ) -> AppUiPreviewRenderPerformanceReport {
        build_preview_render_performance_report(
            Some(self),
            profile,
            APP_UI_PREVIEW_RENDER_DEFAULT_SLOW_FRAME_BUDGET_US,
        )
    }
}

/// Build a versioned preview render performance report from an optional summary.
pub fn build_preview_render_performance_report(
    summary: Option<AppUiPreviewRenderPerformanceSummary>,
    profile: impl Into<String>,
    slow_frame_budget_us: u64,
) -> AppUiPreviewRenderPerformanceReport {
    let mut checks = Vec::new();
    let mut root_causes = Vec::new();
    let mut actions = Vec::new();

    push_render_bool_check(
        &mut checks,
        AppUiPreviewRenderPerformanceArea::CaptureIntegrity,
        "preview_render_evidence_present",
        summary.map(|summary| summary.timed_frames > 0).unwrap_or(false),
    );

    if let Some(mut summary) = summary {
        summary.slow_frame_budget_us = slow_frame_budget_us;
        summary.primary_bottleneck =
            classify_preview_render_bottleneck(summary.max_frame_stage_durations);
        push_render_max_check(
            &mut checks,
            AppUiPreviewRenderPerformanceArea::LatencyBudget,
            "preview_render_max_frame_us",
            summary.max_duration_us,
            slow_frame_budget_us,
        );
        push_preview_render_root_causes_and_actions(summary, &mut root_causes, &mut actions);

        let verdict = preview_render_verdict(&checks);
        return AppUiPreviewRenderPerformanceReport {
            schema_version: APP_UI_PREVIEW_RENDER_PERFORMANCE_REPORT_SCHEMA_VERSION,
            profile: profile.into(),
            verdict,
            summary: Some(summary),
            checks,
            root_causes,
            actions,
        };
    }

    push_render_root_cause_with_action(
        &mut root_causes,
        &mut actions,
        AppUiPreviewRenderPerformanceArea::CaptureIntegrity,
        "missing_preview_render_evidence",
        "preview_render_evidence_present=false".to_owned(),
        "capture_preview_render_stage_durations",
        "Ensure viewer preview records post-decode render stage timings from the real playback path.",
    );

    let verdict = preview_render_verdict(&checks);
    AppUiPreviewRenderPerformanceReport {
        schema_version: APP_UI_PREVIEW_RENDER_PERFORMANCE_REPORT_SCHEMA_VERSION,
        profile: profile.into(),
        verdict,
        summary: None,
        checks,
        root_causes,
        actions,
    }
}

/// Build a versioned preview decode performance report from an optional summary.
pub fn build_preview_decode_performance_report(
    summary: Option<AppUiPreviewDecodePerformanceSummary>,
    profile: impl Into<String>,
    slow_frame_budget_us: u64,
) -> AppUiPreviewDecodePerformanceReport {
    build_preview_decode_performance_report_with_required_access_modes(
        summary,
        profile,
        slow_frame_budget_us,
        &[],
    )
}

/// Build a preview decode report with an explicit access-mode coverage contract.
///
/// Perf smokes use this when a scenario is only valid if selected access modes
/// actually reached the media decode boundary. General UI diagnostics should
/// use [`build_preview_decode_performance_report`] so idle profiles do not fail
/// merely because they did not exercise every mode.
pub fn build_preview_decode_performance_report_with_required_access_modes(
    summary: Option<AppUiPreviewDecodePerformanceSummary>,
    profile: impl Into<String>,
    slow_frame_budget_us: u64,
    required_access_modes: &[PreviewDecodeAccessMode],
) -> AppUiPreviewDecodePerformanceReport {
    let mut checks = Vec::new();
    let mut root_causes = Vec::new();
    let mut actions = Vec::new();
    let required_access_modes = required_access_modes.to_vec();

    push_decode_bool_check(
        &mut checks,
        AppUiPreviewDecodePerformanceArea::CaptureIntegrity,
        "preview_decode_evidence_present",
        summary
            .map(|summary| {
                summary
                    .decode_successes
                    .saturating_add(summary.decode_failures)
                    .saturating_add(summary.canceled_jobs)
                    .saturating_add(summary.playback_current_stalled_expirations)
                    > 0
            })
            .unwrap_or(false),
    );

    if let Some(mut summary) = summary {
        summary.slow_frame_budget_us = slow_frame_budget_us;
        summary.primary_bottleneck = classify_preview_decode_bottleneck(
            summary.max_frame_stage_durations,
            summary.max_frame_queue_wait_us,
        );
        push_decode_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::LatencyBudget,
            "preview_decode_max_frame_us",
            summary.max_duration_us,
            slow_frame_budget_us,
        );
        push_preview_decode_access_mode_coverage_checks(
            &mut checks,
            summary,
            &required_access_modes,
        );
        push_preview_decode_access_mode_checks(&mut checks, summary, slow_frame_budget_us);
        push_decode_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::LatencyBudget,
            "preview_decode_timeout_failures",
            summary.decode_timeout_failures,
            0,
        );
        push_decode_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::RandomAccess,
            "preview_decode_forward_budget_exhausted_failures",
            summary.decode_budget_exhausted_failures,
            0,
        );
        push_decode_warn_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::RandomAccess,
            "preview_decode_max_decoded_frame_count",
            summary.max_decoded_frame_count,
            1,
        );
        push_decode_warn_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::RandomAccess,
            "preview_decode_seeked_frames",
            summary.seeked_frames,
            0,
        );
        push_decode_warn_min_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::ProxyCache,
            "preview_decode_cache_hit_frames",
            summary
                .cache_hit_frames
                .saturating_add(summary.playback_session_ring_hit_frames),
            1,
        );
        push_decode_warn_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_queue_wait_max_us",
            summary.queue_wait_max_us,
            slow_frame_budget_us,
        );
        push_decode_warn_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_expired_playback_current_queue",
            (summary.worker_queue.queued_expired_playback_current_jobs as u64)
                .saturating_add(summary.worker_queue.dropped_expired_playback_current_jobs),
            0,
        );
        push_decode_warn_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_playback_buffering_stall_expirations",
            summary.playback_current_stalled_expirations,
            0,
        );
        push_decode_warn_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_playback_sustained_pressure_events",
            summary.playback_schedule.sustained_pressure_events,
            0,
        );
        push_decode_warn_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_prefetch_deadline_cancellations",
            summary.canceled_prefetch_deadline_jobs,
            0,
        );
        push_decode_warn_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_prefetch_preempted_by_current_cancellations",
            summary.canceled_prefetch_preempted_jobs,
            0,
        );
        push_decode_warn_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_playback_deadline_invalid_frame_rate",
            summary.playback_schedule.current_deadline_missing_frame_rate,
            0,
        );
        push_decode_warn_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_prefetch_window_invalid_frame_rate",
            summary.playback_schedule.forward_prefetch_invalid_frame_rate,
            0,
        );
        push_decode_warn_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_still_preempted_by_realtime_cancellations",
            summary.canceled_still_preempted_jobs,
            0,
        );
        push_decode_warn_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_canceled_max_frame_us",
            summary.canceled_max_duration_us,
            slow_frame_budget_us,
        );
        push_decode_warn_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_cancel_return_latency_max_us",
            summary.canceled_return_latency_max_us,
            slow_frame_budget_us,
        );
        push_decode_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_worker_queue_full_drops",
            summary.queue_full_drops,
            0,
        );
        push_decode_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_queue_invalid_access_mode_drops",
            summary.queue_invalid_access_mode_drops,
            0,
        );
        push_decode_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_worker_disconnected_drops",
            summary.worker_disconnected_drops,
            0,
        );
        push_decode_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_invalid_access_mode_requests",
            summary.scheduler.dropped_invalid_access_mode_requests,
            0,
        );

        push_preview_decode_root_causes_and_actions(
            summary,
            &required_access_modes,
            &mut root_causes,
            &mut actions,
        );

        let verdict = preview_decode_verdict(&checks);
        return AppUiPreviewDecodePerformanceReport {
            schema_version: APP_UI_PREVIEW_DECODE_PERFORMANCE_REPORT_SCHEMA_VERSION,
            profile: profile.into(),
            verdict,
            required_access_modes,
            summary: Some(summary),
            checks,
            root_causes,
            actions,
        };
    }

    push_decode_root_cause_with_action(
        &mut root_causes,
        &mut actions,
        AppUiPreviewDecodePerformanceArea::CaptureIntegrity,
        "missing_preview_decode_evidence",
        "preview_decode_evidence_present=false".to_owned(),
        "capture_preview_decode_diagnostics",
        "Ensure preview media jobs record decode diagnostics from the real playback path.",
        AppUiPreviewDecodePerformanceSeverity::Fail,
    );

    let verdict = preview_decode_verdict(&checks);
    AppUiPreviewDecodePerformanceReport {
        schema_version: APP_UI_PREVIEW_DECODE_PERFORMANCE_REPORT_SCHEMA_VERSION,
        profile: profile.into(),
        verdict,
        required_access_modes,
        summary: None,
        checks,
        root_causes,
        actions,
    }
}

fn preview_decode_verdict(
    checks: &[AppUiPreviewDecodePerformanceCheck],
) -> AppUiPreviewDecodePerformanceVerdict {
    if checks
        .iter()
        .any(|check| check.severity == AppUiPreviewDecodePerformanceSeverity::Fail)
    {
        AppUiPreviewDecodePerformanceVerdict::Fail
    } else if checks
        .iter()
        .any(|check| check.severity == AppUiPreviewDecodePerformanceSeverity::Warn)
    {
        AppUiPreviewDecodePerformanceVerdict::Warn
    } else {
        AppUiPreviewDecodePerformanceVerdict::Pass
    }
}

fn preview_render_verdict(
    checks: &[AppUiPreviewRenderPerformanceCheck],
) -> AppUiPreviewRenderPerformanceVerdict {
    if checks
        .iter()
        .any(|check| check.severity == AppUiPreviewRenderPerformanceSeverity::Fail)
    {
        AppUiPreviewRenderPerformanceVerdict::Fail
    } else {
        AppUiPreviewRenderPerformanceVerdict::Pass
    }
}

fn push_render_max_check(
    checks: &mut Vec<AppUiPreviewRenderPerformanceCheck>,
    area: AppUiPreviewRenderPerformanceArea,
    code: &'static str,
    observed: u64,
    limit: u64,
) {
    checks.push(AppUiPreviewRenderPerformanceCheck {
        area,
        code,
        severity: if observed > limit {
            AppUiPreviewRenderPerformanceSeverity::Fail
        } else {
            AppUiPreviewRenderPerformanceSeverity::Pass
        },
        observed,
        limit: Some(limit),
    });
}

fn push_render_bool_check(
    checks: &mut Vec<AppUiPreviewRenderPerformanceCheck>,
    area: AppUiPreviewRenderPerformanceArea,
    code: &'static str,
    passed: bool,
) {
    checks.push(AppUiPreviewRenderPerformanceCheck {
        area,
        code,
        severity: if passed {
            AppUiPreviewRenderPerformanceSeverity::Pass
        } else {
            AppUiPreviewRenderPerformanceSeverity::Fail
        },
        observed: if passed { 1 } else { 0 },
        limit: Some(1),
    });
}

fn push_decode_max_check(
    checks: &mut Vec<AppUiPreviewDecodePerformanceCheck>,
    area: AppUiPreviewDecodePerformanceArea,
    code: &'static str,
    observed: u64,
    limit: u64,
) {
    checks.push(AppUiPreviewDecodePerformanceCheck {
        area,
        code,
        severity: if observed > limit {
            AppUiPreviewDecodePerformanceSeverity::Fail
        } else {
            AppUiPreviewDecodePerformanceSeverity::Pass
        },
        observed,
        limit: Some(limit),
    });
}

fn push_decode_warn_max_check(
    checks: &mut Vec<AppUiPreviewDecodePerformanceCheck>,
    area: AppUiPreviewDecodePerformanceArea,
    code: &'static str,
    observed: u64,
    limit: u64,
) {
    checks.push(AppUiPreviewDecodePerformanceCheck {
        area,
        code,
        severity: if observed > limit {
            AppUiPreviewDecodePerformanceSeverity::Warn
        } else {
            AppUiPreviewDecodePerformanceSeverity::Pass
        },
        observed,
        limit: Some(limit),
    });
}

fn push_decode_warn_min_check(
    checks: &mut Vec<AppUiPreviewDecodePerformanceCheck>,
    area: AppUiPreviewDecodePerformanceArea,
    code: &'static str,
    observed: u64,
    limit: u64,
) {
    checks.push(AppUiPreviewDecodePerformanceCheck {
        area,
        code,
        severity: if observed < limit {
            AppUiPreviewDecodePerformanceSeverity::Warn
        } else {
            AppUiPreviewDecodePerformanceSeverity::Pass
        },
        observed,
        limit: Some(limit),
    });
}

fn push_decode_bool_check(
    checks: &mut Vec<AppUiPreviewDecodePerformanceCheck>,
    area: AppUiPreviewDecodePerformanceArea,
    code: &'static str,
    passed: bool,
) {
    checks.push(AppUiPreviewDecodePerformanceCheck {
        area,
        code,
        severity: if passed {
            AppUiPreviewDecodePerformanceSeverity::Pass
        } else {
            AppUiPreviewDecodePerformanceSeverity::Fail
        },
        observed: if passed { 1 } else { 0 },
        limit: Some(1),
    });
}

fn push_preview_decode_access_mode_checks(
    checks: &mut Vec<AppUiPreviewDecodePerformanceCheck>,
    summary: AppUiPreviewDecodePerformanceSummary,
    slow_frame_budget_us: u64,
) {
    for (access_mode, profile) in summary.access_mode_profiles.named_profiles() {
        if profile.frames == 0 && profile.queue_wait_max_us == 0 {
            continue;
        }
        if profile.frames > 0 {
            let p95_upper_bound_us = profile.latency_buckets.estimated_p95_upper_bound_us();
            checks.push(AppUiPreviewDecodePerformanceCheck {
                area: AppUiPreviewDecodePerformanceArea::AccessMode,
                code: preview_decode_access_mode_budget_code(access_mode),
                severity: if profile.max_duration_us > slow_frame_budget_us {
                    AppUiPreviewDecodePerformanceSeverity::Fail
                } else {
                    AppUiPreviewDecodePerformanceSeverity::Pass
                },
                observed: profile.max_duration_us,
                limit: Some(slow_frame_budget_us),
            });
            checks.push(AppUiPreviewDecodePerformanceCheck {
                area: AppUiPreviewDecodePerformanceArea::AccessMode,
                code: preview_decode_access_mode_p95_budget_code(access_mode),
                severity: if p95_upper_bound_us > slow_frame_budget_us {
                    AppUiPreviewDecodePerformanceSeverity::Fail
                } else {
                    AppUiPreviewDecodePerformanceSeverity::Pass
                },
                observed: p95_upper_bound_us,
                limit: Some(slow_frame_budget_us),
            });
            if access_mode == PreviewDecodeAccessMode::ScrubCursor {
                checks.push(AppUiPreviewDecodePerformanceCheck {
                    area: AppUiPreviewDecodePerformanceArea::AccessMode,
                    code: "preview_decode_scrub_cursor_bounded_any_seek_strategy",
                    severity: if profile.bounded_any_seek_strategy_frames == profile.frames {
                        AppUiPreviewDecodePerformanceSeverity::Pass
                    } else {
                        AppUiPreviewDecodePerformanceSeverity::Fail
                    },
                    observed: profile.bounded_any_seek_strategy_frames,
                    limit: Some(profile.frames),
                });
                checks.push(AppUiPreviewDecodePerformanceCheck {
                    area: AppUiPreviewDecodePerformanceArea::AccessMode,
                    code: "preview_decode_scrub_cursor_any_seek_window_ms",
                    severity: if profile.any_seek_window_ms_max > 0 {
                        AppUiPreviewDecodePerformanceSeverity::Pass
                    } else {
                        AppUiPreviewDecodePerformanceSeverity::Fail
                    },
                    observed: profile.any_seek_window_ms_max,
                    limit: Some(1),
                });
            }
        }
        let queue_wait_p95_upper_bound_us =
            profile.queue_wait_buckets.estimated_p95_upper_bound_us();
        checks.push(AppUiPreviewDecodePerformanceCheck {
            area: AppUiPreviewDecodePerformanceArea::AccessMode,
            code: preview_decode_access_mode_queue_wait_budget_code(access_mode),
            severity: if profile.queue_wait_max_us > slow_frame_budget_us {
                AppUiPreviewDecodePerformanceSeverity::Warn
            } else {
                AppUiPreviewDecodePerformanceSeverity::Pass
            },
            observed: profile.queue_wait_max_us,
            limit: Some(slow_frame_budget_us),
        });
        checks.push(AppUiPreviewDecodePerformanceCheck {
            area: AppUiPreviewDecodePerformanceArea::AccessMode,
            code: preview_decode_access_mode_queue_wait_p95_budget_code(access_mode),
            severity: if queue_wait_p95_upper_bound_us > slow_frame_budget_us {
                AppUiPreviewDecodePerformanceSeverity::Warn
            } else {
                AppUiPreviewDecodePerformanceSeverity::Pass
            },
            observed: queue_wait_p95_upper_bound_us,
            limit: Some(slow_frame_budget_us),
        });
    }
}

fn push_preview_decode_access_mode_coverage_checks(
    checks: &mut Vec<AppUiPreviewDecodePerformanceCheck>,
    summary: AppUiPreviewDecodePerformanceSummary,
    required_access_modes: &[PreviewDecodeAccessMode],
) {
    for access_mode in required_access_modes {
        let profile = summary.access_mode_profiles.profile_for(*access_mode);
        push_decode_bool_check(
            checks,
            AppUiPreviewDecodePerformanceArea::CaptureIntegrity,
            preview_decode_access_mode_coverage_code(*access_mode),
            profile.frames > 0,
        );
        if profile.frames > 0 {
            push_decode_bool_check(
                checks,
                AppUiPreviewDecodePerformanceArea::CaptureIntegrity,
                preview_decode_access_mode_local_coverage_code(*access_mode),
                profile.mode_local_evidence_frames() > 0,
            );
        }
    }
}

fn preview_decode_access_mode_budget_code(access_mode: PreviewDecodeAccessMode) -> &'static str {
    match access_mode {
        PreviewDecodeAccessMode::PlaybackCursor => "preview_decode_playback_cursor_max_frame_us",
        PreviewDecodeAccessMode::ScrubCursor => "preview_decode_scrub_cursor_max_frame_us",
        PreviewDecodeAccessMode::RandomAccessStillFrame => {
            "preview_decode_random_access_still_max_frame_us"
        }
    }
}

fn preview_decode_access_mode_queue_wait_budget_code(
    access_mode: PreviewDecodeAccessMode,
) -> &'static str {
    match access_mode {
        PreviewDecodeAccessMode::PlaybackCursor => {
            "preview_decode_playback_cursor_queue_wait_max_us"
        }
        PreviewDecodeAccessMode::ScrubCursor => "preview_decode_scrub_cursor_queue_wait_max_us",
        PreviewDecodeAccessMode::RandomAccessStillFrame => {
            "preview_decode_random_access_still_queue_wait_max_us"
        }
    }
}

fn preview_decode_access_mode_p95_budget_code(
    access_mode: PreviewDecodeAccessMode,
) -> &'static str {
    match access_mode {
        PreviewDecodeAccessMode::PlaybackCursor => "preview_decode_playback_cursor_p95_frame_us",
        PreviewDecodeAccessMode::ScrubCursor => "preview_decode_scrub_cursor_p95_frame_us",
        PreviewDecodeAccessMode::RandomAccessStillFrame => {
            "preview_decode_random_access_still_p95_frame_us"
        }
    }
}

fn preview_decode_access_mode_queue_wait_p95_budget_code(
    access_mode: PreviewDecodeAccessMode,
) -> &'static str {
    match access_mode {
        PreviewDecodeAccessMode::PlaybackCursor => {
            "preview_decode_playback_cursor_queue_wait_p95_us"
        }
        PreviewDecodeAccessMode::ScrubCursor => "preview_decode_scrub_cursor_queue_wait_p95_us",
        PreviewDecodeAccessMode::RandomAccessStillFrame => {
            "preview_decode_random_access_still_queue_wait_p95_us"
        }
    }
}

fn preview_decode_access_mode_coverage_code(access_mode: PreviewDecodeAccessMode) -> &'static str {
    match access_mode {
        PreviewDecodeAccessMode::PlaybackCursor => "preview_decode_playback_cursor_sampled",
        PreviewDecodeAccessMode::ScrubCursor => "preview_decode_scrub_cursor_sampled",
        PreviewDecodeAccessMode::RandomAccessStillFrame => {
            "preview_decode_random_access_still_sampled"
        }
    }
}

fn preview_decode_access_mode_local_coverage_code(
    access_mode: PreviewDecodeAccessMode,
) -> &'static str {
    match access_mode {
        PreviewDecodeAccessMode::PlaybackCursor => {
            "preview_decode_playback_cursor_mode_local_sampled"
        }
        PreviewDecodeAccessMode::ScrubCursor => "preview_decode_scrub_cursor_mode_local_sampled",
        PreviewDecodeAccessMode::RandomAccessStillFrame => {
            "preview_decode_random_access_still_mode_local_sampled"
        }
    }
}

fn push_preview_decode_root_causes_and_actions(
    summary: AppUiPreviewDecodePerformanceSummary,
    required_access_modes: &[PreviewDecodeAccessMode],
    root_causes: &mut Vec<AppUiPreviewDecodePerformanceRootCause>,
    actions: &mut Vec<AppUiPreviewDecodePerformanceAction>,
) {
    for access_mode in required_access_modes {
        let profile = summary.access_mode_profiles.profile_for(*access_mode);
        if profile.frames > 0 {
            continue;
        }
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::CaptureIntegrity,
            "preview_decode_required_access_mode_missing",
            format!(
                "access_mode={} required_access_modes={}",
                access_mode.as_str(),
                required_access_modes
                    .iter()
                    .map(|mode| mode.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            "exercise_required_preview_access_modes",
            "Drive this perf profile through every required preview access mode before treating the report as representative.",
            AppUiPreviewDecodePerformanceSeverity::Fail,
        );
    }
    for access_mode in required_access_modes {
        let profile = summary.access_mode_profiles.profile_for(*access_mode);
        if profile.frames == 0 || profile.mode_local_evidence_frames() > 0 {
            continue;
        }
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::CaptureIntegrity,
            "preview_decode_required_access_mode_cache_only",
            format!(
                "access_mode={} frames={} cache_hit_frames={} mode_local_evidence_frames=0",
                access_mode.as_str(),
                profile.frames,
                profile.cache_hit_frames
            ),
            "exercise_required_preview_access_modes_without_global_cache",
            "Drive required preview access modes through mode-local decode evidence; process-global cache hits alone do not prove the access-mode contract.",
            AppUiPreviewDecodePerformanceSeverity::Fail,
        );
    }

    if summary.max_duration_us > summary.slow_frame_budget_us {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::LatencyBudget,
            "preview_decode_frame_over_budget",
            format!(
                "max_duration_us={} slow_frame_budget_us={} max_frame_queue_wait_us={} primary_bottleneck={:?} slowest_access_mode={}",
                summary.max_duration_us,
                summary.slow_frame_budget_us,
                summary.max_frame_queue_wait_us,
                summary.primary_bottleneck,
                summary
                    .slowest_access_mode
                    .map(PreviewDecodeAccessMode::as_str)
                    .unwrap_or("None")
            ),
            "inspect_preview_decode_stage_durations",
            "Inspect preview decode stage timings before changing color or render code.",
            AppUiPreviewDecodePerformanceSeverity::Fail,
        );
    }

    let scrub_profile = summary.access_mode_profiles.scrub_cursor;
    if scrub_profile.frames > 0
        && scrub_profile.bounded_any_seek_strategy_frames < scrub_profile.frames
    {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::AccessMode,
            "preview_decode_scrub_cursor_not_using_low_latency_seek",
            format!(
                "scrub_frames={} bounded_any_seek_strategy_frames={} keyframe_seek_strategy_frames={}",
                scrub_profile.frames,
                scrub_profile.bounded_any_seek_strategy_frames,
                scrub_profile.keyframe_seek_strategy_frames
            ),
            "route_scrub_decode_through_bounded_any_seek",
            "Ensure ScrubCursor decode uses the low-latency bounded-any seek strategy instead of playback/still exact seek semantics.",
            AppUiPreviewDecodePerformanceSeverity::Fail,
        );
    }
    if scrub_profile.frames > 0 && scrub_profile.any_seek_window_ms_max == 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::AccessMode,
            "preview_decode_scrub_cursor_missing_any_seek_window",
            format!(
                "scrub_frames={} bounded_any_seek_strategy_frames={} any_seek_window_ms_max=0 forward_reuse_frame_window_max={} forward_decode_budget_frames_max={}",
                scrub_profile.frames,
                scrub_profile.bounded_any_seek_strategy_frames,
                scrub_profile.forward_reuse_frame_window_max,
                scrub_profile.forward_decode_budget_frames_max
            ),
            "restore_scrub_bounded_any_seek_window",
            "Ensure ScrubCursor decode keeps a non-zero bounded-any seek window so active scrubbing does not fall back to exact still-frame seeking.",
            AppUiPreviewDecodePerformanceSeverity::Fail,
        );
    }
    if scrub_profile.frames > 0
        && scrub_profile.max_duration_us > summary.slow_frame_budget_us
        && scrub_profile.seeked_frames > 0
        && scrub_profile.seek_index_available_frames == 0
    {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::AccessMode,
            "preview_decode_scrub_cursor_without_seek_index_evidence",
            format!(
                "scrub_frames={} seeked_frames={} max_duration_us={} slow_frame_budget_us={} seek_index_available_frames=0 seek_index_used_frames={} seek_index_keyframes_max={} seek_index_observed_packets_max={} seek_index_probe_backed_frames={} seek_index_session_observed_frames={}",
                scrub_profile.frames,
                scrub_profile.seeked_frames,
                scrub_profile.max_duration_us,
                summary.slow_frame_budget_us,
                scrub_profile.seek_index_used_frames,
                scrub_profile.seek_index_keyframes_max,
                scrub_profile.seek_index_observed_packets_max,
                scrub_profile.seek_index_probe_backed_frames,
                scrub_profile.seek_index_session_observed_frames
            ),
            "build_preview_seek_index_evidence",
            "Collect keyframe/GOP seek evidence for scrub sessions before widening seek windows or adding decode workers.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        );
    }

    for (access_mode, profile) in summary.access_mode_profiles.named_profiles() {
        if profile.frames == 0 || profile.max_duration_us <= summary.slow_frame_budget_us {
            continue;
        }
        let p95_upper_bound_us = profile.latency_buckets.estimated_p95_upper_bound_us();
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::AccessMode,
            "preview_decode_access_mode_over_budget",
            format!(
                "access_mode={} frames={} max_duration_us={} p95_upper_bound_us={} total_duration_us={} queue_wait_max_us={} queue_wait_total_us={} max_frame_queue_wait_us={} max_frame_bottleneck={:?} seeked_frames={} keyframe_seek_strategy_frames={} bounded_any_seek_strategy_frames={} forward_reuse_frame_window_max={} forward_decode_budget_frames_max={} any_seek_window_ms_max={} session_reused_frames={} session_opened_frames={} forward_reused_frames={} seek_index_available_frames={} seek_index_used_frames={} seek_index_keyframes_max={} seek_index_observed_packets_max={} seek_index_probe_backed_frames={} seek_index_session_observed_frames={} hardware_decode_active_frames={} zero_copy_active_frames={} gpu_texture_resident_frames={} decoded_nv12_surface_frames={} decoded_p010_surface_frames={} renderer_import_ready_frames={} hardware_decode_texture_residency_blocker_frames={} hardware_decode_auto_requested_frames={} hardware_decode_prefer_gpu_requested_frames={} hardware_decode_require_gpu_requested_frames={} hardware_decode_cpu_not_requested_frames={} hardware_decode_cpu_unavailable_frames={} hardware_decode_access_mode_unsupported_frames={} hardware_decode_backend_unavailable_frames={} hardware_decode_backend_boundary_frames={} hardware_decode_renderer_import_unavailable_frames={} hardware_decode_gpu_resident_native_frames={} hardware_decode_candidate_d3d11va_frames={} hardware_decode_candidate_videotoolbox_frames={} hardware_decode_candidate_vaapi_frames={} hardware_decode_candidate_cuda_frames={} hardware_decode_adapter_unavailable_frames={} decoded_frame_count={} max_decoded_frame_count={} session_open_us={} cache_lookup_us={} seek_us={} packet_decode_us={} swscale_us={} rgba_copy_us={} external_process_us={} cache_hit_frames={} playback_session_ring_hit_frames={} latency_buckets={:?}",
                access_mode.as_str(),
                profile.frames,
                profile.max_duration_us,
                p95_upper_bound_us,
                profile.total_duration_us,
                profile.queue_wait_max_us,
                profile.queue_wait_total_us,
                profile.max_frame_queue_wait_us,
                profile.max_frame_bottleneck,
                profile.seeked_frames,
                profile.keyframe_seek_strategy_frames,
                profile.bounded_any_seek_strategy_frames,
                profile.forward_reuse_frame_window_max,
                profile.forward_decode_budget_frames_max,
                profile.any_seek_window_ms_max,
                profile.session_reused_frames,
                profile.session_opened_frames,
                profile.forward_reused_frames,
                profile.seek_index_available_frames,
                profile.seek_index_used_frames,
                profile.seek_index_keyframes_max,
                profile.seek_index_observed_packets_max,
                profile.seek_index_probe_backed_frames,
                profile.seek_index_session_observed_frames,
                profile.hardware_decode_active_frames,
                profile.zero_copy_active_frames,
                profile.gpu_texture_resident_frames,
                profile.decoded_nv12_surface_frames,
                profile.decoded_p010_surface_frames,
                profile.renderer_import_ready_frames,
                profile.hardware_decode_texture_residency_blocker_frames,
                profile.hardware_decode_auto_requested_frames,
                profile.hardware_decode_prefer_gpu_requested_frames,
                profile.hardware_decode_require_gpu_requested_frames,
                profile.hardware_decode_cpu_not_requested_frames,
                profile.hardware_decode_cpu_unavailable_frames,
                profile.hardware_decode_access_mode_unsupported_frames,
                profile.hardware_decode_backend_unavailable_frames,
                profile.hardware_decode_backend_boundary_frames,
                profile.hardware_decode_renderer_import_unavailable_frames,
                profile.hardware_decode_gpu_resident_native_frames,
                profile.hardware_decode_candidate_d3d11va_frames,
                profile.hardware_decode_candidate_videotoolbox_frames,
                profile.hardware_decode_candidate_vaapi_frames,
                profile.hardware_decode_candidate_cuda_frames,
                profile.hardware_decode_adapter_unavailable_frames,
                profile.decoded_frame_count,
                profile.max_decoded_frame_count,
                profile.max_frame_stage_durations.session_open_us,
                profile.max_frame_stage_durations.cache_lookup_us,
                profile.max_frame_stage_durations.seek_us,
                profile.max_frame_stage_durations.packet_decode_us,
                profile.max_frame_stage_durations.swscale_us,
                profile.max_frame_stage_durations.rgba_copy_us,
                profile.max_frame_stage_durations.external_process_us,
                profile.cache_hit_frames,
                profile.playback_session_ring_hit_frames,
                profile.latency_buckets
            ),
            "inspect_preview_decode_access_mode_profile",
            "Inspect the per-access-mode decode profile before changing global decode concurrency or color/render code.",
            AppUiPreviewDecodePerformanceSeverity::Fail,
        );
    }

    for (access_mode, profile) in summary.access_mode_profiles.named_profiles() {
        if profile.queue_wait_max_us <= summary.slow_frame_budget_us {
            continue;
        }
        let queue_wait_p95_upper_bound_us =
            profile.queue_wait_buckets.estimated_p95_upper_bound_us();
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::AccessMode,
            "preview_decode_access_mode_queue_wait_bound",
            format!(
                "access_mode={} queue_wait_max_us={} queue_wait_p95_upper_bound_us={} queue_wait_total_us={} queue_wait_last_us={} frames={} slow_frame_budget_us={} queue_wait_buckets={:?}",
                access_mode.as_str(),
                profile.queue_wait_max_us,
                queue_wait_p95_upper_bound_us,
                profile.queue_wait_total_us,
                profile.queue_wait_last_us,
                profile.frames,
                summary.slow_frame_budget_us,
                profile.queue_wait_buckets
            ),
            "inspect_preview_access_mode_queue",
            "Inspect per-access-mode worker lane pressure so playback, scrub, and still-frame requests cannot hide each other's queue latency.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        );
    }

    let playback_profile = summary.access_mode_profiles.playback_cursor;
    if playback_profile.frames > 1 && playback_profile.session_reused_frames == 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::AccessMode,
            "preview_decode_playback_session_not_reused",
            format!(
                "access_mode=PlaybackCursor frames={} session_opened_frames={} session_reused_frames=0 max_duration_us={} session_open_us={}",
                playback_profile.frames,
                playback_profile.session_opened_frames,
                playback_profile.max_duration_us,
                playback_profile.max_frame_stage_durations.session_open_us
            ),
            "preserve_playback_decode_session",
            "Keep the playback cursor decode session stable across adjacent playback frames before increasing worker count.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        );
    }
    let playback_source_decode_frames = playback_profile
        .in_process_cpu_rgba_frames
        .saturating_add(playback_profile.external_ffmpeg_cpu_rgba_frames);
    if playback_source_decode_frames > 0
        && playback_profile.forward_reused_frames == 0
        && playback_profile.playback_session_ring_hit_frames == 0
        && playback_profile.cache_hit_frames == 0
    {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::AccessMode,
            "preview_decode_playback_without_locality",
            format!(
                "access_mode=PlaybackCursor source_decode_frames={} forward_reused_frames=0 playback_session_ring_hit_frames=0 cache_hit_frames=0 seeked_frames={} decoded_frame_count={} max_decoded_frame_count={}",
                playback_source_decode_frames,
                playback_profile.seeked_frames,
                playback_profile.decoded_frame_count,
                playback_profile.max_decoded_frame_count
            ),
            "improve_playback_decoder_residency",
            "Inspect playback cursor sequencing, proxy readiness, and decoder residency because playback is behaving like repeated random access.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        );
    }

    if summary.worker_queue.queued_expired_playback_current_jobs > 0
        || summary.worker_queue.dropped_expired_playback_current_jobs > 0
    {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_expired_playback_current_queue",
            format!(
                "queued_expired_playback_current_jobs={} dropped_expired_playback_current_jobs={} queued_jobs={} queued_current_jobs={} queued_playback_cursor_jobs={} queued_prefetch_jobs={} in_flight_jobs={} in_flight_playback_cursor_jobs={} canceled_playback_deadline_jobs={} current_deadline_assignments={} current_decode_decisions={} current_drop_late_decisions={} current_proxy_or_hardware_recommended_decisions={}",
                summary.worker_queue.queued_expired_playback_current_jobs,
                summary.worker_queue.dropped_expired_playback_current_jobs,
                summary.worker_queue.queued_jobs,
                summary.worker_queue.queued_current_jobs,
                summary.worker_queue.queued_playback_cursor_jobs,
                summary.worker_queue.queued_prefetch_jobs,
                summary.worker_activity.in_flight_jobs,
                summary.worker_activity.in_flight_playback_cursor_jobs,
                summary.canceled_playback_deadline_jobs,
                summary.playback_schedule.current_deadline_assignments,
                summary.playback_schedule.current_decode_decisions,
                summary.playback_schedule.current_drop_late_decisions,
                summary
                    .playback_schedule
                    .current_proxy_or_hardware_recommended_decisions
            ),
            "drop_expired_playback_queue_work",
            "Drop or reprioritize expired playback-current work before it waits in the worker queue; playback must make clock-driven decode/drop/proxy decisions instead of decoding stale visible frames.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        );
    }

    if summary.playback_current_stalled_expirations > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_playback_buffering_stall_expirations",
            format!(
                "playback_current_stalled_expirations={} queue_canceled_jobs={} scheduler_canceled_requests={} queued_jobs={} queued_current_jobs={} in_flight_jobs={} in_flight_current_jobs={} canceled_jobs={} canceled_obsolete_jobs={} canceled_playback_deadline_jobs={}",
                summary.playback_current_stalled_expirations,
                summary.queue_canceled_jobs,
                summary.scheduler.canceled_requests,
                summary.worker_queue.queued_jobs,
                summary.worker_queue.queued_current_jobs,
                summary.worker_activity.in_flight_jobs,
                summary.worker_activity.in_flight_current_jobs,
                summary.canceled_jobs,
                summary.canceled_obsolete_jobs,
                summary.canceled_playback_deadline_jobs
            ),
            "diagnose_preview_current_frame_stalls",
            "Inspect realtime current-frame decode residency, worker queue pressure, and cooperative cancellation because playback buffering had to be released without a current preview frame.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        );
    }

    if summary.playback_schedule.sustained_pressure_events > 0
        || summary.playback_schedule.sustained_pressure_active
    {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_playback_sustained_pressure",
            format!(
                "sustained_pressure_active={} sustained_pressure_events={} sustained_pressure_recoveries={} current_late_streak={} current_drop_late_decisions={} current_proxy_or_hardware_recommended_decisions={} prefetch_skipped_sustained_pressure={} prefetch_skipped_current_pending={} prefetch_skipped_worker_busy={} queued_current_jobs={} queued_prefetch_jobs={} in_flight_current_jobs={} in_flight_prefetch_jobs={}",
                summary.playback_schedule.sustained_pressure_active,
                summary.playback_schedule.sustained_pressure_events,
                summary.playback_schedule.sustained_pressure_recoveries,
                summary.playback_schedule.current_late_streak,
                summary.playback_schedule.current_drop_late_decisions,
                summary
                    .playback_schedule
                    .current_proxy_or_hardware_recommended_decisions,
                summary
                    .playback_schedule
                    .prefetch_skipped_sustained_pressure,
                summary.prefetch_skipped_current_pending,
                summary.prefetch_skipped_worker_busy,
                summary.worker_queue.queued_current_jobs,
                summary.worker_queue.queued_prefetch_jobs,
                summary.worker_activity.in_flight_current_jobs,
                summary.worker_activity.in_flight_prefetch_jobs
            ),
            "recover_playback_scheduler_pressure",
            "Suppress forward prefetch while sustained playback pressure is active, then resume only after a current playback frame succeeds; use proxy or hardware decode when late frames continue.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        );
    }

    match summary.primary_bottleneck {
        AppUiPreviewDecodeBottleneck::QueueWait => push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_queue_wait_bound",
            format!(
            "queue_wait_max_us={} current_queue_wait_max_us={} prefetch_queue_wait_max_us={} enqueued_jobs={} prefetch_skipped_current_pending={} prefetch_skipped_worker_busy={} prefetch_skipped_prefetch_backlog={} queued_jobs={} queued_current_jobs={} queued_prefetch_jobs={} queued_playback_cursor_jobs={} queued_expired_playback_current_jobs={} queued_scrub_cursor_jobs={} queued_random_access_still_jobs={} queued_any_lane_eligible_jobs={} queued_playback_lane_eligible_jobs={} queued_scrub_lane_eligible_jobs={} queued_still_lane_eligible_jobs={} queued_interactive_lane_eligible_jobs={} in_flight_jobs={} in_flight_current_jobs={} in_flight_prefetch_jobs={} in_flight_playback_cursor_jobs={} in_flight_scrub_cursor_jobs={} in_flight_random_access_still_jobs={} in_flight_playback_lane_jobs={} in_flight_scrub_lane_jobs={} in_flight_still_lane_jobs={} in_flight_interactive_lane_jobs={} in_flight_cross_lane_current_jobs={} queue_full_drops={} queue_evicted_prefetch_jobs={} queue_evicted_still_jobs={} interactive_cancel_requests={} interactive_cancel_scheduler_requests={} interactive_cancel_queued_jobs={} queue_canceled_jobs={} queue_pruned_obsolete_jobs={} queue_promoted_current_jobs={}",
                summary.queue_wait_max_us,
                summary.current_queue_wait_max_us,
                summary.prefetch_queue_wait_max_us,
                summary.enqueued_jobs,
                summary.prefetch_skipped_current_pending,
                summary.prefetch_skipped_worker_busy,
                summary.prefetch_skipped_prefetch_backlog,
                summary.worker_queue.queued_jobs,
                summary.worker_queue.queued_current_jobs,
                summary.worker_queue.queued_prefetch_jobs,
                summary.worker_queue.queued_playback_cursor_jobs,
                summary.worker_queue.queued_expired_playback_current_jobs,
                summary.worker_queue.queued_scrub_cursor_jobs,
                summary.worker_queue.queued_random_access_still_jobs,
                summary.worker_queue.queued_any_lane_eligible_jobs,
                summary.worker_queue.queued_playback_lane_eligible_jobs,
                summary.worker_queue.queued_scrub_lane_eligible_jobs,
                summary.worker_queue.queued_still_lane_eligible_jobs,
                summary.worker_queue.queued_interactive_lane_eligible_jobs,
                summary.worker_activity.in_flight_jobs,
                summary.worker_activity.in_flight_current_jobs,
                summary.worker_activity.in_flight_prefetch_jobs,
                summary.worker_activity.in_flight_playback_cursor_jobs,
                summary.worker_activity.in_flight_scrub_cursor_jobs,
                summary.worker_activity.in_flight_random_access_still_jobs,
                summary.worker_activity.in_flight_playback_lane_jobs,
                summary.worker_activity.in_flight_scrub_lane_jobs,
                summary.worker_activity.in_flight_still_lane_jobs,
                summary.worker_activity.in_flight_interactive_lane_jobs,
                summary.worker_activity.in_flight_cross_lane_current_jobs,
                summary.queue_full_drops,
                summary.queue_evicted_prefetch_jobs,
                summary.queue_evicted_still_jobs,
                summary.interactive_cancel_requests,
                summary.interactive_cancel_scheduler_requests,
                summary.interactive_cancel_queued_jobs,
                summary.queue_canceled_jobs,
                summary.queue_pruned_obsolete_jobs,
                summary.queue_promoted_current_jobs
            ),
            "prioritize_current_preview_decode",
            "Reduce worker queue wait by canceling stale prefetch work or adding a cancellable decode session.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        ),
        AppUiPreviewDecodeBottleneck::PacketDecode => push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::CodecDecode,
            "preview_decode_codec_or_gop_bound",
            format!(
                "packet_decode_us={} decoded_frame_count={} max_decoded_frame_count={}",
                summary.max_frame_stage_durations.packet_decode_us,
                summary.decoded_frame_count,
                summary.max_decoded_frame_count
            ),
            "enable_proxy_or_hardware_decode",
            "Prefer fresh proxy playback or implement hardware decode residency for this source.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        ),
        AppUiPreviewDecodeBottleneck::Seek => push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::RandomAccess,
            "preview_decode_seek_bound",
            format!(
                "seek_us={} seeked_frames={}",
                summary.max_frame_stage_durations.seek_us, summary.seeked_frames
            ),
            "generate_proxy_or_improve_random_access",
            "Generate playback proxies or improve random-access/indexing strategy for this media.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        ),
        AppUiPreviewDecodeBottleneck::CpuRgbaBoundary => push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::CpuRgbaBoundary,
            "preview_decode_cpu_rgba_boundary_bound",
            format!(
                "swscale_us={} rgba_copy_us={}",
                summary.max_frame_stage_durations.swscale_us,
                summary.max_frame_stage_durations.rgba_copy_us
            ),
            "remove_cpu_rgba_decode_boundary",
            "Move toward high-bit-depth or GPU-resident decode frames instead of CPU RGBA8 preview payloads.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        ),
        AppUiPreviewDecodeBottleneck::ExternalProcess => push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::ExternalProcess,
            "preview_decode_external_process_bound",
            format!(
                "external_process_us={}",
                summary.max_frame_stage_durations.external_process_us
            ),
            "avoid_external_ffmpeg_preview_path",
            "Use in-process decode or a real hardware-resident adapter instead of rawvideo over stdout.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        ),
        AppUiPreviewDecodeBottleneck::SessionOpen => push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::CodecDecode,
            "preview_decode_session_open_bound",
            format!(
                "session_open_us={}",
                summary.max_frame_stage_durations.session_open_us
            ),
            "preserve_decode_session_locality",
            "Keep decode sessions alive across adjacent playback requests and avoid path/size churn.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        ),
        AppUiPreviewDecodeBottleneck::CacheLookup | AppUiPreviewDecodeBottleneck::None => {}
    }

    if summary.cache_hit_frames == 0
        && summary
            .in_process_cpu_rgba_frames
            .saturating_add(summary.external_ffmpeg_cpu_rgba_frames)
            > 0
        && summary.playback_session_ring_hit_frames == 0
    {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::ProxyCache,
            "preview_decode_source_path_without_cache_hits",
            format!(
                "source_decode_frames={} cache_hit_frames=0",
                summary
                    .in_process_cpu_rgba_frames
                    .saturating_add(summary.external_ffmpeg_cpu_rgba_frames)
            ),
            "warm_preview_cache_or_proxy",
            "Warm preview cache or generate fresh playback proxies before interactive playback.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        );
    }

    if summary.canceled_prefetch_deadline_jobs > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_prefetch_deadline_cancellations",
            format!(
                "canceled_prefetch_deadline_jobs={} playback_prefetch_deadline_jobs={} scrub_prefetch_deadline_jobs={} random_access_still_prefetch_deadline_jobs={}",
                summary.canceled_prefetch_deadline_jobs,
                summary
                    .access_mode_profiles
                    .playback_cursor
                    .canceled_prefetch_deadline_jobs,
                summary
                    .access_mode_profiles
                    .scrub_cursor
                    .canceled_prefetch_deadline_jobs,
                summary
                    .access_mode_profiles
                    .random_access_still
                    .canceled_prefetch_deadline_jobs
            ),
            "tune_preview_prefetch_deadline_or_proxy",
            "Inspect prefetch cancellation pressure, proxy readiness, and playback decode locality before increasing decode concurrency.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        );
    }
    if summary.playback_schedule.current_deadline_missing_frame_rate > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_playback_deadline_invalid_frame_rate",
            format!(
                "current_deadline_missing_frame_rate={} current_deadline_assignments={} last_current_deadline_budget_us={:?}",
                summary
                    .playback_schedule
                    .current_deadline_missing_frame_rate,
                summary.playback_schedule.current_deadline_assignments,
                summary.playback_schedule.last_current_deadline_budget_us
            ),
            "fix_sequence_playback_frame_rate_contract",
            "Ensure playback sequences expose a valid frame rate so current-frame decode receives a display deadline.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        );
    }
    if summary.playback_schedule.forward_prefetch_invalid_frame_rate > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_prefetch_window_invalid_frame_rate",
            format!(
                "forward_prefetch_invalid_frame_rate={} forward_prefetch_window_evaluations={} last_forward_prefetch_window_frames={:?} forward_prefetch_horizon_us={} forward_prefetch_min_frames={} forward_prefetch_max_frames={}",
                summary
                    .playback_schedule
                    .forward_prefetch_invalid_frame_rate,
                summary
                    .playback_schedule
                    .forward_prefetch_window_evaluations,
                summary
                    .playback_schedule
                    .last_forward_prefetch_window_frames,
                summary.playback_schedule.forward_prefetch_horizon_us,
                summary.playback_schedule.forward_prefetch_min_frames,
                summary.playback_schedule.forward_prefetch_max_frames
            ),
            "fix_sequence_prefetch_frame_rate_contract",
            "Ensure playback prefetch derives its window from a valid sequence frame rate instead of silently disabling cache warming.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        );
    }
    if summary.canceled_playback_deadline_jobs > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::AccessMode,
            "preview_decode_playback_deadline_cancellations",
            format!(
                "canceled_playback_deadline_jobs={} playback_cursor_deadline_jobs={} playback_frames={} playback_queue_wait_max_us={} playback_max_duration_us={} current_decode_decisions={} current_drop_late_decisions={} current_proxy_or_hardware_recommended_decisions={}",
                summary.canceled_playback_deadline_jobs,
                summary
                    .access_mode_profiles
                    .playback_cursor
                    .canceled_playback_deadline_jobs,
                summary.playback_cursor_frames,
                summary.access_mode_profiles.playback_cursor.queue_wait_max_us,
                summary.access_mode_profiles.playback_cursor.max_duration_us,
                summary.playback_schedule.current_decode_decisions,
                summary.playback_schedule.current_drop_late_decisions,
                summary
                    .playback_schedule
                    .current_proxy_or_hardware_recommended_decisions
            ),
            "drop_late_playback_frames_or_use_proxy_hardware_decode",
            "Playback current frames that miss their display deadline should be dropped or served by proxy/hardware decode rather than decoded after they are obsolete.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        );
    }
    if summary.canceled_prefetch_preempted_jobs > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_prefetch_preempted_by_current",
            format!(
                "canceled_prefetch_preempted_jobs={} playback_prefetch_preempted_jobs={} scrub_prefetch_preempted_jobs={} random_access_still_prefetch_preempted_jobs={} queued_current_jobs={} queued_prefetch_jobs={} in_flight_jobs={} in_flight_prefetch_jobs={} in_flight_playback_cursor_jobs={}",
                summary.canceled_prefetch_preempted_jobs,
                summary
                    .access_mode_profiles
                    .playback_cursor
                    .canceled_prefetch_preempted_jobs,
                summary
                    .access_mode_profiles
                    .scrub_cursor
                    .canceled_prefetch_preempted_jobs,
                summary
                    .access_mode_profiles
                    .random_access_still
                    .canceled_prefetch_preempted_jobs,
                summary.worker_queue.queued_current_jobs,
                summary.worker_queue.queued_prefetch_jobs,
                summary.worker_activity.in_flight_jobs,
                summary.worker_activity.in_flight_prefetch_jobs,
                summary.worker_activity.in_flight_playback_cursor_jobs
            ),
            "reduce_speculative_prefetch_pressure",
            "Throttle playback prefetch when visible current-frame work is waiting; prefetch should yield before it consumes the only interactive decode opportunity.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        );
    }
    if summary.canceled_still_preempted_jobs > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_still_preempted_by_realtime_current",
            format!(
                "canceled_still_preempted_jobs={} random_access_still_preempted_jobs={} queued_current_jobs={} queued_scrub_cursor_jobs={} queued_playback_cursor_jobs={} queued_random_access_still_jobs={} in_flight_jobs={} in_flight_current_jobs={} in_flight_scrub_cursor_jobs={} in_flight_playback_cursor_jobs={} in_flight_random_access_still_jobs={}",
                summary.canceled_still_preempted_jobs,
                summary
                    .access_mode_profiles
                    .random_access_still
                    .canceled_still_preempted_jobs,
                summary.worker_queue.queued_current_jobs,
                summary.worker_queue.queued_scrub_cursor_jobs,
                summary.worker_queue.queued_playback_cursor_jobs,
                summary.worker_queue.queued_random_access_still_jobs,
                summary.worker_activity.in_flight_jobs,
                summary.worker_activity.in_flight_current_jobs,
                summary.worker_activity.in_flight_scrub_cursor_jobs,
                summary.worker_activity.in_flight_playback_cursor_jobs,
                summary.worker_activity.in_flight_random_access_still_jobs
            ),
            "preempt_still_decode_for_realtime_preview",
            "Keep random-access still decode from occupying the only interactive lane when playback or scrub current-frame work is waiting.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        );
    }
    if summary.decode_timeout_failures > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::LatencyBudget,
            "preview_decode_timeout_failures",
            format!(
                "decode_timeout_failures={} playback_timeout_failures={} scrub_timeout_failures={} random_access_still_timeout_failures={}",
                summary.decode_timeout_failures,
                summary.access_mode_profiles.playback_cursor.timeout_failures,
                summary.access_mode_profiles.scrub_cursor.timeout_failures,
                summary
                    .access_mode_profiles
                    .random_access_still
                    .timeout_failures
            ),
            "inspect_access_mode_decode_timeout_budget",
            "Inspect access-mode decode strategy, hardware decode residency, proxy readiness, and timeout budget before widening worker concurrency.",
            AppUiPreviewDecodePerformanceSeverity::Fail,
        );
    }
    if summary.decode_budget_exhausted_failures > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::RandomAccess,
            "preview_decode_forward_budget_exhausted",
            format!(
                "decode_budget_exhausted_failures={} playback_budget_exhausted_failures={} scrub_budget_exhausted_failures={} random_access_still_budget_exhausted_failures={} playback_forward_decode_budget_frames_max={} scrub_forward_decode_budget_frames_max={} random_access_still_forward_decode_budget_frames_max={}",
                summary.decode_budget_exhausted_failures,
                summary.access_mode_profiles.playback_cursor.budget_exhausted_failures,
                summary.access_mode_profiles.scrub_cursor.budget_exhausted_failures,
                summary
                    .access_mode_profiles
                    .random_access_still
                    .budget_exhausted_failures,
                summary
                    .access_mode_profiles
                    .playback_cursor
                    .forward_decode_budget_frames_max,
                summary
                    .access_mode_profiles
                    .scrub_cursor
                    .forward_decode_budget_frames_max,
                summary
                    .access_mode_profiles
                    .random_access_still
                    .forward_decode_budget_frames_max
            ),
            "inspect_access_mode_forward_decode_budget",
            "Inspect GOP length, proxy readiness, hardware decode residency, and access-mode forward decode budgets before widening CPU fallback work.",
            AppUiPreviewDecodePerformanceSeverity::Fail,
        );
    }
    if summary.canceled_max_duration_us > summary.slow_frame_budget_us {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_slow_cancellation",
            format!(
                "canceled_max_duration_us={} canceled_last_duration_us={} canceled_jobs={} playback_canceled_max_duration_us={} scrub_canceled_max_duration_us={} random_access_still_canceled_max_duration_us={}",
                summary.canceled_max_duration_us,
                summary.canceled_last_duration_us,
                summary.canceled_jobs,
                summary.access_mode_profiles.playback_cursor.canceled_max_duration_us,
                summary.access_mode_profiles.scrub_cursor.canceled_max_duration_us,
                summary
                    .access_mode_profiles
                    .random_access_still
                    .canceled_max_duration_us
            ),
            "inspect_preview_decode_cancellation_points",
            "Inspect FFmpeg open/seek/decode/copy cancellation points and avoid uncancellable external process work on realtime preview lanes.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        );
    }
    if summary.canceled_return_latency_max_us > summary.slow_frame_budget_us {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_slow_cancel_return",
            format!(
                "canceled_return_latency_max_us={} canceled_return_latency_last_us={} canceled_jobs={} playback_cancel_return_latency_max_us={} scrub_cancel_return_latency_max_us={} random_access_still_cancel_return_latency_max_us={}",
                summary.canceled_return_latency_max_us,
                summary.canceled_return_latency_last_us,
                summary.canceled_jobs,
                summary
                    .access_mode_profiles
                    .playback_cursor
                    .canceled_return_latency_max_us,
                summary
                    .access_mode_profiles
                    .scrub_cursor
                    .canceled_return_latency_max_us,
                summary
                    .access_mode_profiles
                    .random_access_still
                    .canceled_return_latency_max_us
            ),
            "shorten_preview_decode_cancel_cleanup",
            "Inspect cleanup after cooperative cancellation is observed; canceled realtime decode jobs must return promptly after the first cancel checkpoint.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        );
    }
    if summary.canceled_obsolete_jobs > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_obsolete_cancellations",
            format!(
                "canceled_obsolete_jobs={} canceled_jobs={} playback_obsolete_jobs={} scrub_obsolete_jobs={} random_access_still_obsolete_jobs={}",
                summary.canceled_obsolete_jobs,
                summary.canceled_jobs,
                summary.access_mode_profiles.playback_cursor.canceled_obsolete_jobs,
                summary.access_mode_profiles.scrub_cursor.canceled_obsolete_jobs,
                summary
                    .access_mode_profiles
                    .random_access_still
                    .canceled_obsolete_jobs
            ),
            "coalesce_obsolete_preview_requests",
            "Coalesce preview requests before decode when UI state changes faster than workers can consume jobs.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        );
    }
    if summary.canceled_unknown_jobs > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_unknown_cancellations",
            format!(
                "canceled_unknown_jobs={} playback_unknown_jobs={} scrub_unknown_jobs={} random_access_still_unknown_jobs={}",
                summary.canceled_unknown_jobs,
                summary.access_mode_profiles.playback_cursor.canceled_unknown_jobs,
                summary.access_mode_profiles.scrub_cursor.canceled_unknown_jobs,
                summary.access_mode_profiles.random_access_still.canceled_unknown_jobs
            ),
            "preserve_preview_cancel_reason",
            "Ensure app-level cancellation predicates record a structured reason before returning canceled decode outcomes.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        );
    }

    if summary.queue_full_drops > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_worker_queue_full_drops",
            format!(
                "queue_full_drops={} enqueued_jobs={} prefetch_skipped_current_pending={} prefetch_skipped_worker_busy={} prefetch_skipped_prefetch_backlog={} queued_jobs={} queued_current_jobs={} queued_prefetch_jobs={} queued_playback_cursor_jobs={} queued_expired_playback_current_jobs={} queued_scrub_cursor_jobs={} queued_random_access_still_jobs={} queued_any_lane_eligible_jobs={} queued_playback_lane_eligible_jobs={} queued_scrub_lane_eligible_jobs={} queued_still_lane_eligible_jobs={} queued_interactive_lane_eligible_jobs={} in_flight_jobs={} in_flight_current_jobs={} in_flight_prefetch_jobs={} in_flight_playback_cursor_jobs={} in_flight_scrub_cursor_jobs={} in_flight_random_access_still_jobs={} queue_evicted_prefetch_jobs={} queue_evicted_still_jobs={} interactive_cancel_requests={} interactive_cancel_scheduler_requests={} interactive_cancel_queued_jobs={} queue_canceled_jobs={} queue_pruned_obsolete_jobs={} queue_promoted_current_jobs={} scheduler_dropped_pending_window_requests={} scheduler_evicted_still_requests={}",
                summary.queue_full_drops,
                summary.enqueued_jobs,
                summary.prefetch_skipped_current_pending,
                summary.prefetch_skipped_worker_busy,
                summary.prefetch_skipped_prefetch_backlog,
                summary.worker_queue.queued_jobs,
                summary.worker_queue.queued_current_jobs,
                summary.worker_queue.queued_prefetch_jobs,
                summary.worker_queue.queued_playback_cursor_jobs,
                summary.worker_queue.queued_expired_playback_current_jobs,
                summary.worker_queue.queued_scrub_cursor_jobs,
                summary.worker_queue.queued_random_access_still_jobs,
                summary.worker_queue.queued_any_lane_eligible_jobs,
                summary.worker_queue.queued_playback_lane_eligible_jobs,
                summary.worker_queue.queued_scrub_lane_eligible_jobs,
                summary.worker_queue.queued_still_lane_eligible_jobs,
                summary.worker_queue.queued_interactive_lane_eligible_jobs,
                summary.worker_activity.in_flight_jobs,
                summary.worker_activity.in_flight_current_jobs,
                summary.worker_activity.in_flight_prefetch_jobs,
                summary.worker_activity.in_flight_playback_cursor_jobs,
                summary.worker_activity.in_flight_scrub_cursor_jobs,
                summary.worker_activity.in_flight_random_access_still_jobs,
                summary.queue_evicted_prefetch_jobs,
                summary.queue_evicted_still_jobs,
                summary.interactive_cancel_requests,
                summary.interactive_cancel_scheduler_requests,
                summary.interactive_cancel_queued_jobs,
                summary.queue_canceled_jobs,
                summary.queue_pruned_obsolete_jobs,
                summary.queue_promoted_current_jobs,
                summary.scheduler.dropped_pending_window_requests,
                summary.scheduler.evicted_still_requests
            ),
            "reduce_preview_worker_transport_backpressure",
            "Fix preview worker transport backpressure so scheduler-accepted current-frame work cannot be dropped after admission.",
            AppUiPreviewDecodePerformanceSeverity::Fail,
        );
    }
    if summary.queue_invalid_access_mode_drops > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_queue_invalid_access_mode_drop",
            format!(
                "queue_invalid_access_mode_drops={} enqueued_jobs={} scheduler_dropped_invalid_access_mode_requests={}",
                summary.queue_invalid_access_mode_drops,
                summary.enqueued_jobs,
                summary.scheduler.dropped_invalid_access_mode_requests
            ),
            "fix_preview_access_mode_admission",
            "Ensure invalid priority/access-mode pairs are rejected before worker-queue transport.",
            AppUiPreviewDecodePerformanceSeverity::Fail,
        );
    }
    if summary.worker_disconnected_drops > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_worker_disconnected_drops",
            format!(
                "worker_disconnected_drops={} enqueued_jobs={} queue_full_drops={}",
                summary.worker_disconnected_drops, summary.enqueued_jobs, summary.queue_full_drops
            ),
            "restore_preview_worker_lifecycle",
            "Ensure preview workers are running before accepting media preview jobs and close the queue only during service shutdown.",
            AppUiPreviewDecodePerformanceSeverity::Fail,
        );
    }

    let scheduler = summary.scheduler;
    if scheduler.dropped_invalid_access_mode_requests > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_invalid_access_mode_request",
            format!(
                "dropped_invalid_access_mode_requests={} scheduled_requests={} pending_requests={}",
                scheduler.dropped_invalid_access_mode_requests,
                scheduler.scheduled_requests,
                scheduler.pending_requests
            ),
            "fix_preview_access_mode_admission",
            "Route speculative media work through PlaybackCursor prefetch only; scrub and still-frame requests must be current-frame work.",
            AppUiPreviewDecodePerformanceSeverity::Fail,
        );
    }
    if scheduler
        .skipped_decode_access_mode_mismatch
        .saturating_add(scheduler.completed_cache_only_access_mode_mismatch)
        .saturating_add(scheduler.completed_stale_access_mode_mismatch)
        > 0
    {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_access_mode_mismatch",
            format!(
                "skipped_decode_access_mode_mismatch={} completed_cache_only_access_mode_mismatch={} completed_stale_access_mode_mismatch={}",
                scheduler.skipped_decode_access_mode_mismatch,
                scheduler.completed_cache_only_access_mode_mismatch,
                scheduler.completed_stale_access_mode_mismatch
            ),
            "inspect_preview_access_mode_transitions",
            "Inspect playback/scrub/still request transitions and ensure older jobs cannot complete newer access-mode work.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        );
    }
    if scheduler
        .skipped_decode_obsolete_generation
        .saturating_add(scheduler.completed_stale_obsolete_generation)
        .saturating_add(scheduler.pruned_obsolete_requests)
        > 0
    {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_obsolete_generation_churn",
            format!(
                "skipped_decode_obsolete_generation={} completed_stale_obsolete_generation={} pruned_obsolete_requests={}",
                scheduler.skipped_decode_obsolete_generation,
                scheduler.completed_stale_obsolete_generation,
                scheduler.pruned_obsolete_requests
            ),
            "reduce_preview_generation_churn",
            "Reduce duplicate preview requests per UI tick or coalesce obsolete generations before they reach workers.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        );
    }
    if scheduler.dropped_pending_window_requests > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_pending_window_backpressure",
            format!(
                "dropped_pending_window_requests={} pending_requests={}",
                scheduler.dropped_pending_window_requests, scheduler.pending_requests
            ),
            "bound_preview_pending_window_by_access_mode",
            "Inspect current/prefetch admission policy and keep visible current-frame work latest-wins.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        );
    }
}

fn push_preview_render_root_causes_and_actions(
    summary: AppUiPreviewRenderPerformanceSummary,
    root_causes: &mut Vec<AppUiPreviewRenderPerformanceRootCause>,
    actions: &mut Vec<AppUiPreviewRenderPerformanceAction>,
) {
    if summary.max_duration_us <= summary.slow_frame_budget_us {
        return;
    }

    push_render_root_cause_with_action(
        root_causes,
        actions,
        AppUiPreviewRenderPerformanceArea::LatencyBudget,
        "preview_render_frame_over_budget",
        format!(
            "max_duration_us={} slow_frame_budget_us={} primary_bottleneck={:?}",
            summary.max_duration_us, summary.slow_frame_budget_us, summary.primary_bottleneck
        ),
        "inspect_preview_render_stage_durations",
        "Inspect post-decode viewer render stage timings before changing decode code.",
    );

    match summary.primary_bottleneck {
        AppUiPreviewRenderBottleneck::Resolve => push_render_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewRenderPerformanceArea::Resolve,
            "preview_render_resolve_bound",
            format!("resolve_us={}", summary.max_frame_stage_durations.resolve_us),
            "profile_preview_plan_resolution",
            "Profile sequence resolution, media-key construction, and readiness checks.",
        ),
        AppUiPreviewRenderBottleneck::FinalCacheLookup => push_render_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewRenderPerformanceArea::FinalCacheLookup,
            "preview_render_final_cache_lookup_bound",
            format!(
                "final_cache_lookup_us={}",
                summary.max_frame_stage_durations.final_cache_lookup_us
            ),
            "profile_viewer_frame_cache",
            "Profile final viewer frame cache lookup and external texture identity checks.",
        ),
        AppUiPreviewRenderBottleneck::WorkingPreparation => push_render_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewRenderPerformanceArea::WorkingPreparation,
            "preview_render_working_prepare_bound",
            format!(
                "working_prepare_us={}",
                summary.max_frame_stage_durations.working_prepare_us
            ),
            "reduce_working_frame_preparation",
            "Reduce working-frame extraction/copy work before timeline compositing.",
        ),
        AppUiPreviewRenderBottleneck::CpuComposite => push_render_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewRenderPerformanceArea::CpuComposite,
            "preview_render_cpu_composite_bound",
            format!(
                "cpu_composite_us={}",
                summary.max_frame_stage_durations.cpu_composite_us
            ),
            "move_preview_composite_to_gpu",
            "Keep common blend, transform, and effect paths on GPU or improve CPU composite tiling.",
        ),
        AppUiPreviewRenderBottleneck::CpuOutputBoundary => push_render_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewRenderPerformanceArea::CpuOutputBoundary,
            "preview_render_cpu_output_boundary_bound",
            format!(
                "cpu_output_boundary_us={}",
                summary.max_frame_stage_durations.cpu_output_boundary_us
            ),
            "move_preview_output_boundary_to_gpu",
            "Route viewer output color/display transforms through the GPU output boundary.",
        ),
        AppUiPreviewRenderBottleneck::FramePackaging => push_render_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewRenderPerformanceArea::FramePackaging,
            "preview_render_frame_packaging_bound",
            format!(
                "frame_packaging_us={}",
                summary.max_frame_stage_durations.frame_packaging_us
            ),
            "avoid_raster_frame_packaging",
            "Prefer GPU-resident viewer frames or reduce final raster hashing/copying.",
        ),
        AppUiPreviewRenderBottleneck::None => {}
    }
}

fn push_render_root_cause_with_action(
    root_causes: &mut Vec<AppUiPreviewRenderPerformanceRootCause>,
    actions: &mut Vec<AppUiPreviewRenderPerformanceAction>,
    area: AppUiPreviewRenderPerformanceArea,
    root_code: &'static str,
    evidence: String,
    action_code: &'static str,
    action_description: &'static str,
) {
    if !root_causes.iter().any(|root| root.code == root_code) {
        root_causes.push(AppUiPreviewRenderPerformanceRootCause {
            area,
            code: root_code,
            severity: AppUiPreviewRenderPerformanceSeverity::Fail,
            evidence,
        });
    }
    if !actions.iter().any(|action| action.code == action_code) {
        actions.push(AppUiPreviewRenderPerformanceAction {
            area,
            code: action_code,
            description: action_description,
        });
    }
}

fn push_decode_root_cause_with_action(
    root_causes: &mut Vec<AppUiPreviewDecodePerformanceRootCause>,
    actions: &mut Vec<AppUiPreviewDecodePerformanceAction>,
    area: AppUiPreviewDecodePerformanceArea,
    root_code: &'static str,
    evidence: String,
    action_code: &'static str,
    action_description: &'static str,
    severity: AppUiPreviewDecodePerformanceSeverity,
) {
    if !root_causes.iter().any(|root| root.code == root_code) {
        root_causes.push(AppUiPreviewDecodePerformanceRootCause {
            area,
            code: root_code,
            severity,
            evidence,
        });
    }
    if !actions.iter().any(|action| action.code == action_code) {
        actions.push(AppUiPreviewDecodePerformanceAction {
            area,
            code: action_code,
            description: action_description,
        });
    }
}

fn classify_preview_decode_bottleneck(
    durations: PreviewDecodeStageDurations,
    queue_wait_us: u64,
) -> AppUiPreviewDecodeBottleneck {
    let candidates = [
        (AppUiPreviewDecodeBottleneck::QueueWait, queue_wait_us),
        (
            AppUiPreviewDecodeBottleneck::SessionOpen,
            durations.session_open_us,
        ),
        (
            AppUiPreviewDecodeBottleneck::CacheLookup,
            durations.cache_lookup_us,
        ),
        (AppUiPreviewDecodeBottleneck::Seek, durations.seek_us),
        (
            AppUiPreviewDecodeBottleneck::PacketDecode,
            durations.packet_decode_us,
        ),
        (
            AppUiPreviewDecodeBottleneck::CpuRgbaBoundary,
            durations.swscale_us.saturating_add(durations.rgba_copy_us),
        ),
        (
            AppUiPreviewDecodeBottleneck::ExternalProcess,
            durations.external_process_us,
        ),
    ];

    candidates
        .into_iter()
        .max_by_key(|(_, duration)| *duration)
        .filter(|(_, duration)| *duration > 0)
        .map(|(bottleneck, _)| bottleneck)
        .unwrap_or(AppUiPreviewDecodeBottleneck::None)
}

fn classify_preview_render_bottleneck(
    durations: AppUiPreviewRenderStageDurations,
) -> AppUiPreviewRenderBottleneck {
    let candidates = [
        (AppUiPreviewRenderBottleneck::Resolve, durations.resolve_us),
        (
            AppUiPreviewRenderBottleneck::FinalCacheLookup,
            durations.final_cache_lookup_us,
        ),
        (
            AppUiPreviewRenderBottleneck::WorkingPreparation,
            durations.working_prepare_us,
        ),
        (
            AppUiPreviewRenderBottleneck::CpuComposite,
            durations.cpu_composite_us,
        ),
        (
            AppUiPreviewRenderBottleneck::CpuOutputBoundary,
            durations.cpu_output_boundary_us,
        ),
        (
            AppUiPreviewRenderBottleneck::FramePackaging,
            durations.frame_packaging_us,
        ),
    ];

    candidates
        .into_iter()
        .max_by_key(|(_, duration)| *duration)
        .filter(|(_, duration)| *duration > 0)
        .map(|(bottleneck, _)| bottleneck)
        .unwrap_or(AppUiPreviewRenderBottleneck::None)
}

/// Schema version for preview color health reports.
pub const APP_UI_PREVIEW_COLOR_HEALTH_REPORT_SCHEMA_VERSION: u32 = 1;

/// Versioned preview color health report for UI, telemetry, and perf artifacts.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AppUiPreviewColorHealthReport {
    /// Report schema version.
    pub schema_version: u32,
    /// Applied report profile.
    pub profile: String,
    /// Overall preview color health verdict.
    pub verdict: AppUiPreviewColorHealthVerdict,
    /// Structured color health summary used as report evidence.
    pub summary: Option<AppUiPreviewColorHealthSummary>,
    /// Structured checks by preview color-pipeline area.
    pub checks: Vec<AppUiPreviewColorHealthCheck>,
    /// Prioritized machine-readable root causes.
    pub root_causes: Vec<AppUiPreviewColorHealthRootCause>,
    /// Suggested engineering or operator actions.
    pub actions: Vec<AppUiPreviewColorHealthAction>,
}

/// Overall preview color health verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum AppUiPreviewColorHealthVerdict {
    /// Preview color path met all fail-closed checks.
    Pass,
    /// Preview color path passed hard checks but has warning evidence.
    Warn,
    /// Preview color path violated a fail-closed check.
    Fail,
}

/// Preview color diagnostic area.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum AppUiPreviewColorHealthArea {
    /// Evidence capture and summary availability.
    CaptureIntegrity,
    /// Input media metadata and policy handling.
    InputColorPolicy,
    /// Renderer color-stage scheduling.
    StageScheduling,
    /// Timeline compositing precision and legacy paths.
    CompositePath,
    /// Explicitly unsupported features (OS ICC, HDR/EDR, GPU compositing).
    UnsupportedFeature,
}

/// Preview color health check severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum AppUiPreviewColorHealthSeverity {
    /// Check passed.
    Pass,
    /// Check produced warning evidence.
    Warn,
    /// Check failed.
    Fail,
}

/// One preview color health check.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AppUiPreviewColorHealthCheck {
    /// Diagnostic area for this check.
    pub area: AppUiPreviewColorHealthArea,
    /// Stable check code.
    pub code: &'static str,
    /// Check severity.
    pub severity: AppUiPreviewColorHealthSeverity,
    /// Observed value.
    pub observed: u64,
    /// Optional target or threshold.
    pub limit: Option<u64>,
}

/// One preview color health root cause.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AppUiPreviewColorHealthRootCause {
    /// Diagnostic area for this root cause.
    pub area: AppUiPreviewColorHealthArea,
    /// Stable root-cause code.
    pub code: &'static str,
    /// Root-cause severity.
    pub severity: AppUiPreviewColorHealthSeverity,
    /// Compact evidence string.
    pub evidence: String,
}

/// One preview color health action.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AppUiPreviewColorHealthAction {
    /// Diagnostic area for this action.
    pub area: AppUiPreviewColorHealthArea,
    /// Stable action code.
    pub code: &'static str,
    /// Human-readable action.
    pub description: &'static str,
}

impl AppUiPreviewColorHealthSummary {
    /// Build the versioned preview color health report for this summary.
    pub fn health_report(self, profile: impl Into<String>) -> AppUiPreviewColorHealthReport {
        build_preview_color_health_report(Some(self), profile)
    }
}

/// Build a versioned preview color health report from an optional summary.
pub fn build_preview_color_health_report(
    summary: Option<AppUiPreviewColorHealthSummary>,
    profile: impl Into<String>,
) -> AppUiPreviewColorHealthReport {
    let mut checks = Vec::new();
    let mut root_causes = Vec::new();
    let mut actions = Vec::new();

    push_preview_bool_check(
        &mut checks,
        AppUiPreviewColorHealthArea::CaptureIntegrity,
        "color_health_present",
        summary.is_some(),
    );

    if let Some(summary) = summary {
        push_preview_bool_check(
            &mut checks,
            AppUiPreviewColorHealthArea::CompositePath,
            color_report_vocab::check::FULLY_FLOAT_LINEAR,
            summary.fully_float_linear,
        );
        push_preview_bool_check(
            &mut checks,
            AppUiPreviewColorHealthArea::StageScheduling,
            color_report_vocab::check::GPU_PATH_READY,
            summary.gpu_path_ready,
        );
        push_preview_max_check(
            &mut checks,
            AppUiPreviewColorHealthArea::StageScheduling,
            color_report_vocab::check::GPU_BLOCKERS,
            summary.gpu_blockers,
            0,
        );
        push_preview_max_check(
            &mut checks,
            AppUiPreviewColorHealthArea::StageScheduling,
            color_report_vocab::check::TRANSFER_STAGES,
            summary.transfer_stages,
            0,
        );
        push_preview_max_check(
            &mut checks,
            AppUiPreviewColorHealthArea::CompositePath,
            color_report_vocab::check::LEGACY_REASON_TOTAL,
            summary.legacy_reason_total,
            0,
        );
        push_preview_max_check(
            &mut checks,
            AppUiPreviewColorHealthArea::InputColorPolicy,
            color_report_vocab::check::POLICY_REJECTIONS,
            summary.policy_rejections,
            0,
        );
        push_preview_max_check(
            &mut checks,
            AppUiPreviewColorHealthArea::StageScheduling,
            color_report_vocab::check::CPU_OUTPUT_FALLBACK_FRAMES,
            summary.cpu_output_fallback_frames,
            0,
        );
        push_preview_max_check(
            &mut checks,
            AppUiPreviewColorHealthArea::StageScheduling,
            color_report_vocab::check::GPU_OUTPUT_BLOCKERS,
            summary.preview_gpu_output_blocker_breakdown.total(),
            0,
        );
        push_preview_max_check(
            &mut checks,
            AppUiPreviewColorHealthArea::UnsupportedFeature,
            "unsupported_feature_count",
            summary.preview_gpu_output_blocker_breakdown.unsupported_features,
            0,
        );
        push_preview_root_causes_and_actions(summary, &mut root_causes, &mut actions);
    } else {
        push_preview_root_cause_with_action(
            &mut root_causes,
            &mut actions,
            AppUiPreviewColorHealthArea::CaptureIntegrity,
            "missing_preview_color_evidence",
            "color_health_present=false".to_owned(),
            "inspect_preview_diagnostics",
            "Ensure preview diagnostics record color summaries from the real preview path.",
        );
    }

    let has_failures = checks
        .iter()
        .any(|check| check.severity == AppUiPreviewColorHealthSeverity::Fail);
    let has_warnings = checks
        .iter()
        .any(|check| check.severity == AppUiPreviewColorHealthSeverity::Warn);
    let verdict = if has_failures {
        AppUiPreviewColorHealthVerdict::Fail
    } else if has_warnings {
        AppUiPreviewColorHealthVerdict::Warn
    } else {
        AppUiPreviewColorHealthVerdict::Pass
    };

    AppUiPreviewColorHealthReport {
        schema_version: APP_UI_PREVIEW_COLOR_HEALTH_REPORT_SCHEMA_VERSION,
        profile: profile.into(),
        verdict,
        summary,
        checks,
        root_causes,
        actions,
    }
}

fn push_preview_max_check(
    checks: &mut Vec<AppUiPreviewColorHealthCheck>,
    area: AppUiPreviewColorHealthArea,
    code: &'static str,
    observed: u64,
    limit: u64,
) {
    checks.push(AppUiPreviewColorHealthCheck {
        area,
        code,
        severity: if observed > limit {
            AppUiPreviewColorHealthSeverity::Fail
        } else {
            AppUiPreviewColorHealthSeverity::Pass
        },
        observed,
        limit: Some(limit),
    });
}

fn push_preview_bool_check(
    checks: &mut Vec<AppUiPreviewColorHealthCheck>,
    area: AppUiPreviewColorHealthArea,
    code: &'static str,
    passed: bool,
) {
    checks.push(AppUiPreviewColorHealthCheck {
        area,
        code,
        severity: if passed {
            AppUiPreviewColorHealthSeverity::Pass
        } else {
            AppUiPreviewColorHealthSeverity::Fail
        },
        observed: if passed { 1 } else { 0 },
        limit: Some(1),
    });
}

fn push_preview_root_causes_and_actions(
    summary: AppUiPreviewColorHealthSummary,
    root_causes: &mut Vec<AppUiPreviewColorHealthRootCause>,
    actions: &mut Vec<AppUiPreviewColorHealthAction>,
) {
    if summary.policy_rejections > 0 {
        push_preview_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewColorHealthArea::InputColorPolicy,
            color_report_vocab::root_cause::INPUT_COLOR_POLICY_REJECTED_SOURCE,
            format!("policy_rejections={}", summary.policy_rejections),
            color_report_vocab::action::INSPECT_ASSET_COLOR_DIAGNOSTICS,
            "Inspect active-sequence media color diagnostics and missing-metadata policy.",
        );
    }
    if summary.gpu_blockers > 0 {
        push_preview_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewColorHealthArea::StageScheduling,
            "preview_gpu_color_stage_blocked",
            format!(
                "gpu_blockers={} shader={} ocio={} wrapper={} pipeline={}",
                summary.gpu_blockers,
                summary.gpu_blocker_breakdown.shader_module_not_prepared,
                summary
                    .gpu_blocker_breakdown
                    .ocio_resource_bind_group_not_prepared,
                summary.gpu_blocker_breakdown.fullscreen_wrapper_not_prepared,
                summary.gpu_blocker_breakdown.render_pipeline_not_prepared
            ),
            color_report_vocab::action::INSPECT_GPU_BLOCKERS,
            "Inspect renderer GPU color blocker breakdown before relying on preview GPU scheduling.",
        );
    }
    if summary.transfer_stages > 0 {
        push_preview_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewColorHealthArea::StageScheduling,
            "preview_transfer_stage_present",
            format!("transfer_stages={}", summary.transfer_stages),
            color_report_vocab::action::REMOVE_TRANSFER_STAGE,
            "Trace why preview color work introduced upload/readback transfer stages.",
        );
    }
    if !summary.fully_float_linear || summary.legacy_reason_total > 0 {
        push_preview_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewColorHealthArea::CompositePath,
            color_report_vocab::root_cause::LEGACY_RGBA8_COMPOSITE_PATH,
            format!(
                "fully_float_linear={} legacy_reason_total={}",
                summary.fully_float_linear, summary.legacy_reason_total
            ),
            color_report_vocab::action::MIGRATE_LEGACY_COMPOSITE_REASON,
            "Use structured legacy RGBA8 reasons to migrate preview composites back to float/linear.",
        );
    }
    if summary.gpu_compositing.cpu_fallback_composites > 0 {
        push_preview_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewColorHealthArea::CompositePath,
            "preview_gpu_compositing_cpu_fallback",
            format!(
                "cpu_fallback_composites={} cpu_composited_pixels={} first_blocker={:?}",
                summary.gpu_compositing.cpu_fallback_composites,
                summary.gpu_compositing.cpu_composited_pixels,
                summary.gpu_compositing.first_blocker
            ),
            "resolve_gpu_compositing_blocker",
            "Inspect GPU compositing blocker and either lower the layer feature to GPU or keep the explicit CPU fallback.",
        );
    }
    if summary.cpu_output_fallback_frames > 0 {
        push_preview_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewColorHealthArea::StageScheduling,
            "preview_cpu_output_fallback",
            format!(
                "cpu_output_fallback_frames={} cpu_output_fallback_pixels={}",
                summary.cpu_output_fallback_frames, summary.cpu_output_fallback_pixels
            ),
            "investigate_cpu_fallback",
            "Inspect why preview output fell back to CPU RGBA8 boundary instead of GPU color path.",
        );
    }
    let blocker_breakdown = summary.preview_gpu_output_blocker_breakdown;
    if !blocker_breakdown.is_empty() {
        push_preview_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewColorHealthArea::StageScheduling,
            "preview_gpu_output_blocked",
            format!(
                "total_blockers={} ocio_config={} ocio_processor={} shader_extraction={} \
                 shader={} ocio_resource={} wrapper={} pipeline={} \
                 surface_contract={} display_color={} hdr={} \
                 frame_resident={} legacy_rgba8_boundary={} cpu_fallback={}",
                blocker_breakdown.total(),
                blocker_breakdown.ocio_config_not_loaded,
                blocker_breakdown.ocio_processor_unavailable,
                blocker_breakdown.ocio_gpu_shader_extraction_failed,
                blocker_breakdown.shader_module_not_prepared,
                blocker_breakdown.ocio_resource_bind_group_not_prepared,
                blocker_breakdown.fullscreen_wrapper_not_prepared,
                blocker_breakdown.render_pipeline_not_prepared,
                blocker_breakdown.surface_contract_mismatch,
                blocker_breakdown.unsupported_display_color_space,
                blocker_breakdown.unsupported_hdr_swapchain_or_edr,
                blocker_breakdown.frame_not_gpu_resident,
                blocker_breakdown.legacy_rgba8_composite_boundary,
                blocker_breakdown.cpu_fallback_requested
            ),
            "inspect_preview_gpu_output_blockers",
            "Inspect preview GPU output blocker breakdown to identify the primary blocker.",
        );
    }
    if blocker_breakdown.unsupported_features > 0 {
        push_preview_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewColorHealthArea::UnsupportedFeature,
            "preview_unsupported_feature",
            format!(
                "unsupported_features={}",
                blocker_breakdown.unsupported_features
            ),
            "document_unsupported_feature",
            "Document the unsupported feature limitations and track for future implementation.",
        );
    }
}

fn push_preview_root_cause_with_action(
    root_causes: &mut Vec<AppUiPreviewColorHealthRootCause>,
    actions: &mut Vec<AppUiPreviewColorHealthAction>,
    area: AppUiPreviewColorHealthArea,
    root_code: &'static str,
    evidence: String,
    action_code: &'static str,
    action_description: &'static str,
) {
    if !root_causes.iter().any(|root| root.code == root_code) {
        root_causes.push(AppUiPreviewColorHealthRootCause {
            area,
            code: root_code,
            severity: AppUiPreviewColorHealthSeverity::Fail,
            evidence,
        });
    }
    if !actions.iter().any(|action| action.code == action_code) {
        actions.push(AppUiPreviewColorHealthAction {
            area,
            code: action_code,
            description: action_description,
        });
    }
}

impl AppUiPreviewColorRejection {
    fn new(
        asset_id: AssetId,
        path: PathBuf,
        resolution: InputColorResolution,
        diagnostic_summary: String,
        diagnostic_issue_summary: VideoColorDiagnosticIssueSummary,
    ) -> Self {
        Self {
            asset_id,
            path,
            missing_metadata_policy: resolution.missing_metadata_policy,
            source: resolution.source,
            override_color_space: resolution.override_color_space,
            detected_color_space: resolution.detected_color_space,
            working_color_space: resolution.working_color_space,
            diagnostic_summary,
            diagnostic_issue_summary,
        }
    }
}

impl AppUiPreviewDiagnostics {
    /// Return structured preview decode performance evidence when decode activity exists.
    pub fn decode_performance_summary(
        self,
        slow_frame_budget_us: u64,
    ) -> Option<AppUiPreviewDecodePerformanceSummary> {
        let decode_successes = self.decode_successes;
        if decode_successes
            .saturating_add(self.decode_failures)
            .saturating_add(self.decode_canceled_jobs)
            .saturating_add(self.playback_current_stalled_expirations)
            == 0
        {
            return None;
        }
        let stage_durations = self.decode_stage_durations;
        let max_frame_stage_durations = self.decode_max_frame_stage_durations;
        let max_frame_queue_wait_us = self.decode_max_frame_queue_wait_us;
        let mut primary_bottleneck = self.decode_max_frame_bottleneck;
        if primary_bottleneck == AppUiPreviewDecodeBottleneck::None {
            primary_bottleneck = classify_preview_decode_bottleneck(
                max_frame_stage_durations,
                max_frame_queue_wait_us,
            );
        }
        Some(AppUiPreviewDecodePerformanceSummary {
            cpu_budget: self.decode_cpu_budget,
            decode_successes,
            decode_failures: self.decode_failures,
            decode_timeout_failures: self.decode_timeout_failures,
            decode_budget_exhausted_failures: self.decode_budget_exhausted_failures,
            canceled_jobs: self.decode_canceled_jobs,
            canceled_shutdown_jobs: self.decode_canceled_shutdown_jobs,
            canceled_obsolete_jobs: self.decode_canceled_obsolete_jobs,
            canceled_prefetch_deadline_jobs: self.decode_canceled_prefetch_deadline_jobs,
            canceled_playback_deadline_jobs: self.decode_canceled_playback_deadline_jobs,
            canceled_prefetch_preempted_jobs: self.decode_canceled_prefetch_preempted_jobs,
            canceled_still_preempted_jobs: self.decode_canceled_still_preempted_jobs,
            canceled_unknown_jobs: self.decode_canceled_unknown_jobs,
            canceled_total_duration_us: self.decode_canceled_total_duration_us,
            canceled_max_duration_us: self.decode_canceled_max_duration_us,
            canceled_last_duration_us: self.decode_canceled_last_duration_us,
            canceled_return_latency_total_us: self.decode_canceled_return_latency_total_us,
            canceled_return_latency_max_us: self.decode_canceled_return_latency_max_us,
            canceled_return_latency_last_us: self.decode_canceled_return_latency_last_us,
            in_process_cpu_rgba_frames: self.decode_in_process_cpu_rgba_frames,
            external_ffmpeg_cpu_rgba_frames: self.decode_external_ffmpeg_cpu_rgba_frames,
            playback_session_ring_hit_frames: self.decode_playback_session_ring_hit_frames,
            cache_hit_frames: self.decode_cache_hit_frames,
            playback_cursor_frames: self.decode_playback_cursor_frames,
            scrub_cursor_frames: self.decode_scrub_cursor_frames,
            random_access_still_frames: self.decode_random_access_still_frames,
            canceled_playback_cursor_jobs: self.decode_canceled_playback_cursor_jobs,
            canceled_scrub_cursor_jobs: self.decode_canceled_scrub_cursor_jobs,
            canceled_random_access_still_jobs: self.decode_canceled_random_access_still_jobs,
            max_duration_us: self.decode_max_duration_us,
            last_duration_us: self.decode_last_duration_us,
            total_duration_us: self.decode_total_duration_us,
            slow_frame_budget_us,
            queue_wait_total_us: self.decode_queue_wait_total_us,
            queue_wait_max_us: self.decode_queue_wait_max_us,
            queue_wait_last_us: self.decode_queue_wait_last_us,
            current_queue_wait_max_us: self.decode_current_queue_wait_max_us,
            prefetch_queue_wait_max_us: self.decode_prefetch_queue_wait_max_us,
            enqueued_jobs: self.enqueued_jobs,
            prefetch_skipped_current_pending: self.prefetch_skipped_current_pending,
            prefetch_skipped_worker_busy: self.prefetch_skipped_worker_busy,
            prefetch_skipped_prefetch_backlog: self.prefetch_skipped_prefetch_backlog,
            queue_full_drops: self.queue_full_drops,
            queue_invalid_access_mode_drops: self.queue_invalid_access_mode_drops,
            queue_evicted_prefetch_jobs: self.queue_evicted_prefetch_jobs,
            queue_evicted_still_jobs: self.queue_evicted_still_jobs,
            interactive_cancel_requests: self.interactive_cancel_requests,
            interactive_cancel_scheduler_requests: self.interactive_cancel_scheduler_requests,
            interactive_cancel_queued_jobs: self.interactive_cancel_queued_jobs,
            queue_canceled_jobs: self.queue_canceled_jobs,
            playback_current_stalled_expirations: self.playback_current_stalled_expirations,
            queue_pruned_obsolete_jobs: self.queue_pruned_obsolete_jobs,
            queue_promoted_current_jobs: self.queue_promoted_current_jobs,
            worker_disconnected_drops: self.worker_disconnected_drops,
            worker_queue: self.worker_queue,
            worker_activity: self.worker_activity,
            seeked_frames: self.decode_seeked_frames,
            decoded_frame_count: self.decode_decoded_frame_count,
            max_decoded_frame_count: self.decode_max_decoded_frame_count,
            stage_durations,
            max_frame_stage_durations,
            max_frame_queue_wait_us,
            access_mode_profiles: self.decode_access_mode_profiles,
            slowest_access_mode: self.decode_access_mode_profiles.slowest_access_mode(),
            primary_bottleneck,
            scheduler: self.scheduler,
            playback_schedule: self.playback_schedule,
        })
    }

    /// Return structured post-decode viewer render performance evidence.
    pub fn render_performance_summary(
        self,
        slow_frame_budget_us: u64,
    ) -> Option<AppUiPreviewRenderPerformanceSummary> {
        if self.render_timed_frames == 0 {
            return None;
        }
        let stage_durations = self.render_stage_durations;
        let max_frame_stage_durations = self.render_max_frame_stage_durations;
        Some(AppUiPreviewRenderPerformanceSummary {
            timed_frames: self.render_timed_frames,
            max_duration_us: self.render_max_duration_us,
            last_duration_us: self.render_last_duration_us,
            total_duration_us: self.render_total_duration_us,
            slow_frame_budget_us,
            stage_durations,
            max_frame_stage_durations,
            primary_bottleneck: classify_preview_render_bottleneck(max_frame_stage_durations),
        })
    }

    /// Return input color-resolution branch counters using the shared timeline model.
    pub fn input_color_resolution_counts(self) -> InputColorResolutionSourceCounts {
        InputColorResolutionSourceCounts {
            override_count: self.input_color_resolution_override,
            data_texture: self.input_color_resolution_data_texture,
            detected_metadata: self.input_color_resolution_detected_metadata,
            missing_assume_rec709: self.input_color_resolution_missing_assume_rec709,
            missing_assume_working: self.input_color_resolution_missing_assume_working,
            missing_rejected: self.input_color_resolution_missing_rejected,
        }
    }

    /// Return the renderer-owned composite color-path summary for preview diagnostics.
    pub fn composite_color_path_summary(self) -> TimelineCompositeColorPathSummary {
        TimelineCompositeDiagnostics {
            elements: self.color_composite_elements,
            float_linear_composites: self.color_composite_float_linear,
            legacy_rgba8_composites: self.color_composite_legacy_rgba8,
            legacy_media_blend_mode: self.color_composite_legacy_media_blend_mode,
            legacy_media_transform: self.color_composite_legacy_media_transform,
            legacy_media_effect: self.color_composite_legacy_media_effect,
            legacy_solid_blend_mode: self.color_composite_legacy_solid_blend_mode,
            legacy_solid_transform: self.color_composite_legacy_solid_transform,
            legacy_solid_effect: self.color_composite_legacy_solid_effect,
            legacy_adjustment_blend_mode: self.color_composite_legacy_adjustment_blend_mode,
            legacy_adjustment_effect: self.color_composite_legacy_adjustment_effect,
            ..TimelineCompositeDiagnostics::default()
        }
        .color_path_summary()
    }

    /// Return color-stage scheduling diagnostics using the shared renderer model.
    pub fn color_stage_diagnostics(self) -> RenderColorStageDiagnostics {
        RenderColorStageDiagnostics {
            total_stages: self.color_stage_total_stages,
            cpu_input_stages: self.color_stage_cpu_input_stages,
            cpu_output_stages: self.color_stage_cpu_output_stages,
            gpu_color_stages: self.color_stage_gpu_color_stages,
            upload_stages: self.color_stage_upload_stages,
            readback_stages: self.color_stage_readback_stages,
            gpu_blockers: self.color_stage_gpu_blockers,
            gpu_blocker_breakdown: RenderColorStageGpuBlockerBreakdown {
                shader_module_not_prepared: self.color_stage_gpu_shader_module_blockers,
                ocio_resource_bind_group_not_prepared: self.color_stage_gpu_ocio_resource_blockers,
                fullscreen_wrapper_not_prepared: self.color_stage_gpu_wrapper_blockers,
                render_pipeline_not_prepared: self.color_stage_gpu_render_pipeline_blockers,
                ocio_config_not_loaded: self.color_stage_gpu_ocio_config_blockers,
                ocio_processor_unavailable: self.color_stage_gpu_ocio_processor_blockers,
                ocio_gpu_shader_extraction_failed: self
                    .color_stage_gpu_ocio_shader_extraction_blockers,
            },
            stage_pixels: self.color_stage_pixels,
        }
    }

    /// Return the stable preview color-path health summary when diagnostics have evidence.
    pub fn color_health_summary(self) -> Option<AppUiPreviewColorHealthSummary> {
        let counts = self.input_color_resolution_counts();
        let stages = self.color_stage_diagnostics();
        let composite = self.composite_color_path_summary();
        if counts.total() == 0
            && stages.total_stages == 0
            && composite.composite_plans() == 0
            && self.color_rgba8_boundary_calls == 0
            && self.cpu_output_fallback_frames == 0
            && self.preview_gpu_output_blocker_breakdown.total() == 0
        {
            return None;
        }

        Some(AppUiPreviewColorHealthSummary {
            composite_plans: composite.composite_plans(),
            detected_metadata: counts.detected_metadata,
            override_count: counts.override_count,
            policy_assumptions: counts.policy_assumptions(),
            data_textures: counts.data_textures(),
            policy_rejections: counts.policy_rejections(),
            explicit_metadata_or_override: counts.explicit_metadata_or_override(),
            cpu_input_stages: stages.cpu_input_stages,
            cpu_output_stages: stages.cpu_output_stages,
            gpu_color_stages: stages.gpu_color_stages,
            gpu_blockers: stages.gpu_blockers,
            gpu_blocker_breakdown: stages.gpu_blocker_breakdown,
            transfer_stages: stages.upload_stages.saturating_add(stages.readback_stages),
            rgba8_boundary_calls: self.color_rgba8_boundary_calls,
            float_linear_composites: composite.float_linear_composites,
            legacy_rgba8_composites: composite.legacy_rgba8_composites,
            legacy_reason_total: composite.legacy_breakdown.total(),
            legacy_breakdown: composite.legacy_breakdown,
            fully_float_linear: composite.is_fully_float_linear()
                && self.color_composite_plans == composite.composite_plans(),
            gpu_path_ready: stages.gpu_blockers == 0
                && stages.upload_stages == 0
                && stages.readback_stages == 0,
            cpu_output_fallback_frames: self.cpu_output_fallback_frames,
            cpu_output_fallback_pixels: self.cpu_output_fallback_pixels,
            preview_gpu_output_blocker_breakdown: self.preview_gpu_output_blocker_breakdown,
            gpu_compositing: self.gpu_compositing,
        })
    }
}

/// Result of asking the preview service for a GPU-output viewer frame candidate.
pub(crate) enum AppUiGpuPreviewFrameState {
    /// The current resolved viewer frame is already backed by a registered external texture.
    Current,
    /// A working-space frame is ready for GPU output-boundary recording.
    Ready(Box<AppUiGpuPreviewFrame>),
    /// Required media is still decoding or rendering.
    Loading,
    /// No viewer preview frame is expected for the current state.
    Unavailable,
}

/// Working-space viewer frame plus the output boundary needed by the app-window GPU path.
pub(crate) struct AppUiGpuPreviewFrame {
    cache_key: ViewerPreviewCacheKey,
    /// Active sequence that produced this frame.
    pub sequence_id: SequenceId,
    /// Timeline frame number.
    pub frame: i64,
    /// Preview frame width in pixels.
    pub width: u32,
    /// Preview frame height in pixels.
    pub height: u32,
    /// Timeline working color space represented by the working input.
    pub working_color_space: ColorSpace,
    /// Working-space input that enters the GPU output boundary.
    pub working_input: AppUiGpuPreviewWorkingInput,
    /// Display/output boundary to execute on the GPU.
    pub boundary: RenderOutputColorBoundary,
    /// Monotonic identifier for this working-frame candidate.
    preview_candidate_id: u64,
}

/// Working-space input for the app-window GPU output path.
pub(crate) enum AppUiGpuPreviewWorkingInput {
    /// Layer stack to composite directly on GPU before the GPU output transform.
    GpuComposite {
        /// Layers in bottom-to-top order.
        layers: Vec<AppUiGpuPreviewCompositeLayer>,
    },
}

/// One app-owned layer for GPU working-space preview compositing.
pub(crate) enum AppUiGpuPreviewCompositeLayer {
    /// Working-space media frame.
    Media {
        /// CPU working frame used as a correctness fallback when GPU input
        /// transform cannot be scheduled for this layer.
        frame: Option<CpuColorFrame>,
        /// Source/input contract for the preferred GPU color path.
        gpu_source: Option<AppUiGpuPreviewMediaSource>,
        /// Layer opacity.
        opacity: f32,
        /// Timeline affine transform.
        transform: [f32; 6],
    },
    /// Full-frame solid color.
    SolidColor {
        /// Solid layer.
        layer: TimelineSolidColorLayer,
    },
}

/// App-window media source contract for GPU input color transforms.
#[derive(Debug, Clone)]
pub(crate) struct AppUiGpuPreviewMediaSource {
    /// CPU decoded encoded RGBA8 source frame.
    pub source: CpuEncodedColorFrame,
    /// Source/import -> timeline working-space transform.
    pub input_transform: RenderInputTransform,
    /// Residency reported by the media decode boundary.
    pub decoder_residency: DecodedFrameResidency,
    /// Native decoder handle family, when the media boundary produced a GPU surface.
    pub decoder_handle_kind: Option<DecodedGpuFrameHandleKind>,
    /// Decoder output surface format before any CPU RGBA conversion.
    pub decoded_surface_format: DecodedVideoSurfaceFormat,
}

impl AppUiGpuPreviewFrame {
    /// Stable external texture key for the resolved plan represented by this frame.
    pub fn external_texture_key(&self) -> String {
        format!(
            "app-ui.viewer.gpu:{}:{}x{}:{:016x}",
            self.sequence_id, self.width, self.height, self.cache_key.plan_signature
        )
    }

    /// Candidate identifier for viewer output attempt correlation.
    pub(crate) fn preview_candidate_id(&self) -> u64 {
        self.preview_candidate_id
    }
}

enum ResolvedPreviewElement {
    SolidColor(TimelineSolidColorLayer),
    Adjustment(TimelineAdjustmentLayer),
    Media {
        frame: MediaPreviewFrame,
        opacity: f32,
        blend_mode: BlendMode,
        transform: [f32; 6],
        effect_graph: Arc<CompiledEffectGraph>,
        frame_seed: i64,
    },
}

impl ViewerPreviewSource for AppUiPreviewService {
    fn viewer_preview_for_state(&self, state: &AppState) -> ViewerPreviewState {
        self.render_preview(state)
    }

    fn viewer_color_rejection(&self) -> Option<ViewerPreviewColorRejectionModel> {
        self.last_color_rejection().map(|rejection| ViewerPreviewColorRejectionModel {
            asset_id: rejection.asset_id,
            path: rejection.path,
            missing_metadata_policy: rejection.missing_metadata_policy,
            source: rejection.source,
            override_color_space: rejection.override_color_space,
            detected_color_space: rejection.detected_color_space,
            working_color_space: rejection.working_color_space,
            diagnostic_summary: rejection.diagnostic_summary,
            diagnostic_issue_summary: rejection.diagnostic_issue_summary,
        })
    }

    fn viewer_color_pipeline_status(&self) -> Option<ViewerColorPipelineStatus> {
        let diagnostics = self.diagnostics();
        let summary = diagnostics.composite_color_path_summary();
        if summary.composite_plans() == 0
            && diagnostics.cpu_output_fallback_frames == 0
            && diagnostics.preview_gpu_output_blocker_breakdown.total() == 0
        {
            return None;
        }
        if diagnostics.preview_gpu_output_blocker_breakdown.total() > 0 {
            return Some(ViewerColorPipelineStatus::GpuBlocked {
                gpu_blockers: diagnostics.preview_gpu_output_blocker_breakdown.total(),
            });
        }
        if diagnostics.color_stage_gpu_blockers > 0 {
            return Some(ViewerColorPipelineStatus::GpuBlocked {
                gpu_blockers: diagnostics.color_stage_gpu_blockers,
            });
        }
        if diagnostics.cpu_output_fallback_frames > 0 {
            return Some(ViewerColorPipelineStatus::LegacyRgba8 {
                legacy_reasons: diagnostics.cpu_output_fallback_frames,
            });
        }
        if summary.uses_legacy_rgba8() {
            return Some(ViewerColorPipelineStatus::LegacyRgba8 {
                legacy_reasons: summary.legacy_breakdown.total(),
            });
        }
        Some(ViewerColorPipelineStatus::FloatLinear)
    }
}

impl Default for AppUiPreviewService {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for AppUiPreviewService {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[derive(Debug, Clone)]
struct MediaPreviewFrame {
    frame: Option<CpuColorFrame>,
    gpu_source: Option<MediaPreviewGpuSourceFrame>,
    width: u32,
    height: u32,
    signature: u64,
}

impl MediaPreviewFrame {
    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn gpu_source(&self) -> Option<AppUiGpuPreviewMediaSource> {
        self.gpu_source.as_ref().map(|source| AppUiGpuPreviewMediaSource {
            source: source.source.clone(),
            input_transform: source.input_transform.clone(),
            decoder_residency: source.decoder_residency,
            decoder_handle_kind: source.decoder_handle_kind,
            decoded_surface_format: source.decoded_surface_format,
        })
    }

    fn working_frame(&self) -> Result<MediaPreviewWorkingFrame, String> {
        if let Some(frame) = self.frame.as_ref() {
            return Ok(MediaPreviewWorkingFrame {
                frame: frame.clone(),
                color_diagnostics: None,
                stage_diagnostics: RenderColorStageDiagnostics::default(),
            });
        }
        let Some(source) = self.gpu_source.as_ref() else {
            return Err("media preview frame has no CPU working frame or GPU source".to_owned());
        };
        let cached_before = source.working_cache.get().is_some();
        let entry = source
            .working_cache
            .get_or_init(|| {
                execute_cpu_input_stage(&source.source, &source.input_transform)
                    .map(|output| MediaPreviewWorkingFrameCacheEntry {
                        frame: output.result.frame,
                        color_diagnostics: output.result.diagnostics,
                        stage_diagnostics: output.stage_diagnostics,
                    })
                    .map_err(|err| {
                        format!("viewer preview lazy input color transform failed: {err}")
                    })
            })
            .as_ref()
            .map_err(Clone::clone)?;
        Ok(MediaPreviewWorkingFrame {
            frame: entry.frame.clone(),
            color_diagnostics: (!cached_before).then_some(entry.color_diagnostics),
            stage_diagnostics: if cached_before {
                RenderColorStageDiagnostics::default()
            } else {
                entry.stage_diagnostics
            },
        })
    }
}

struct MediaPreviewWorkingFrame {
    frame: CpuColorFrame,
    color_diagnostics: Option<RenderColorTransformDiagnostics>,
    stage_diagnostics: RenderColorStageDiagnostics,
}

#[derive(Debug, Clone)]
struct MediaPreviewGpuSourceFrame {
    source: CpuEncodedColorFrame,
    input_transform: RenderInputTransform,
    decoder_residency: DecodedFrameResidency,
    decoder_handle_kind: Option<DecodedGpuFrameHandleKind>,
    decoded_surface_format: DecodedVideoSurfaceFormat,
    working_cache: Arc<OnceLock<Result<MediaPreviewWorkingFrameCacheEntry, String>>>,
}

impl MediaPreviewGpuSourceFrame {
    #[cfg(test)]
    fn new(source: CpuEncodedColorFrame, input_transform: RenderInputTransform) -> Self {
        Self {
            source,
            input_transform,
            decoder_residency: DecodedFrameResidency::CpuRgba,
            decoder_handle_kind: None,
            decoded_surface_format: DecodedVideoSurfaceFormat::Unknown,
            working_cache: Arc::new(OnceLock::new()),
        }
    }

    fn from_decode_diagnostics(
        source: CpuEncodedColorFrame,
        input_transform: RenderInputTransform,
        diagnostics: PreviewDecodeDiagnostics,
    ) -> Self {
        Self {
            source,
            input_transform,
            decoder_residency: diagnostics.decoded_frame_residency,
            decoder_handle_kind: diagnostics.gpu_frame_handle_kind,
            decoded_surface_format: diagnostics.decoded_surface_format,
            working_cache: Arc::new(OnceLock::new()),
        }
    }
}

#[derive(Debug, Clone)]
struct MediaPreviewWorkingFrameCacheEntry {
    frame: CpuColorFrame,
    color_diagnostics: RenderColorTransformDiagnostics,
    stage_diagnostics: RenderColorStageDiagnostics,
}

#[derive(Debug, Clone)]
struct ScopedViewerFrame {
    sequence_id: SequenceId,
    width: u32,
    height: u32,
    frame: ViewerFrameImage,
}

#[derive(Debug, Clone)]
struct ScopedExternalViewerFrame {
    cache_key: ViewerPreviewCacheKey,
    content: ViewerExternalTextureFrame,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ViewerPreviewCacheKey {
    sequence_id: SequenceId,
    width: u32,
    height: u32,
    plan_signature: u64,
}

struct ViewerPreviewFrameCache {
    capacity: usize,
    entries: HashMap<ViewerPreviewCacheKey, ViewerFrameImage>,
    lru: VecDeque<ViewerPreviewCacheKey>,
}

impl ViewerPreviewFrameCache {
    fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            entries: HashMap::new(),
            lru: VecDeque::new(),
        }
    }

    fn get(&mut self, key: &ViewerPreviewCacheKey) -> Option<ViewerFrameImage> {
        let frame = self.entries.get(key)?.clone();
        self.touch(key);
        Some(frame)
    }

    fn insert(&mut self, key: ViewerPreviewCacheKey, frame: ViewerFrameImage) {
        self.entries.insert(key.clone(), frame);
        self.touch(&key);
        while self.entries.len() > self.capacity {
            let Some(oldest) = self.lru.pop_front() else {
                break;
            };
            if self.entries.remove(&oldest).is_some() {
                break;
            }
        }
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.lru.clear();
    }

    fn touch(&mut self, key: &ViewerPreviewCacheKey) {
        self.lru.retain(|candidate| candidate != key);
        self.lru.push_back(key.clone());
    }
}

struct MediaPreviewCache {
    capacity: usize,
    entries: HashMap<MediaPreviewKey, MediaPreviewFrame>,
    lru: VecDeque<MediaPreviewKey>,
}

struct MediaPreviewFailureCache {
    capacity: usize,
    entries: HashSet<MediaPreviewKey>,
    lru: VecDeque<MediaPreviewKey>,
}

impl MediaPreviewFailureCache {
    fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            entries: HashSet::new(),
            lru: VecDeque::new(),
        }
    }

    fn contains(&mut self, key: &MediaPreviewKey) -> bool {
        let contains = self.entries.contains(key);
        if contains {
            self.touch(key);
        }
        contains
    }

    fn insert(&mut self, key: MediaPreviewKey) {
        self.entries.insert(key.clone());
        self.touch(&key);
        while self.entries.len() > self.capacity {
            let Some(oldest) = self.lru.pop_front() else {
                break;
            };
            if self.entries.remove(&oldest) {
                break;
            }
        }
    }

    fn remove(&mut self, key: &MediaPreviewKey) {
        self.entries.remove(key);
        self.lru.retain(|candidate| candidate != key);
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.lru.clear();
    }

    fn touch(&mut self, key: &MediaPreviewKey) {
        self.lru.retain(|candidate| candidate != key);
        self.lru.push_back(key.clone());
    }
}

impl MediaPreviewCache {
    fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            entries: HashMap::new(),
            lru: VecDeque::new(),
        }
    }

    fn get(&mut self, key: &MediaPreviewKey) -> Option<MediaPreviewFrame> {
        let frame = self.entries.get(key)?.clone();
        self.touch(key);
        Some(frame)
    }

    fn insert(&mut self, key: MediaPreviewKey, frame: MediaPreviewFrame) {
        self.entries.insert(key.clone(), frame);
        self.touch(&key);
        while self.entries.len() > self.capacity {
            let Some(oldest) = self.lru.pop_front() else {
                break;
            };
            if self.entries.remove(&oldest).is_some() {
                break;
            }
        }
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.lru.clear();
    }

    fn touch(&mut self, key: &MediaPreviewKey) {
        self.lru.retain(|candidate| candidate != key);
        self.lru.push_back(key.clone());
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MediaPreviewCancelReason {
    Shutdown,
    Obsolete,
    PrefetchDeadline,
    PlaybackDeadline,
    PrefetchPreemptedByCurrent,
    StillPreemptedByRealtimeCurrent,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MediaPreviewFailureReason {
    Timeout,
    DecodeError,
    ForwardDecodeBudgetExhausted,
}

#[derive(Debug)]
struct MediaPreviewResult {
    key: MediaPreviewKey,
    frame: Option<MediaPreviewFrame>,
    error: Option<String>,
    failure_reason: Option<MediaPreviewFailureReason>,
    generation: u64,
    priority: MediaPreviewRequestPriority,
    access_mode: PreviewDecodeAccessMode,
    queue_wait_us: u64,
    decode_elapsed_us: u64,
    cancel_observed_elapsed_us: Option<u64>,
    canceled: bool,
    cancel_reason: Option<MediaPreviewCancelReason>,
    decode_diagnostics: Option<PreviewDecodeDiagnostics>,
    color_diagnostics: Option<RenderColorTransformDiagnostics>,
    color_stage_diagnostics: Option<RenderColorStageDiagnostics>,
}

#[derive(Default)]
struct PreviewWorkerActivity {
    in_flight_jobs: AtomicUsize,
    in_flight_current_jobs: AtomicUsize,
    in_flight_prefetch_jobs: AtomicUsize,
    in_flight_playback_cursor_jobs: AtomicUsize,
    in_flight_scrub_cursor_jobs: AtomicUsize,
    in_flight_random_access_still_jobs: AtomicUsize,
    in_flight_any_lane_jobs: AtomicUsize,
    in_flight_playback_lane_jobs: AtomicUsize,
    in_flight_scrub_lane_jobs: AtomicUsize,
    in_flight_still_lane_jobs: AtomicUsize,
    in_flight_interactive_lane_jobs: AtomicUsize,
    in_flight_cross_lane_current_jobs: AtomicUsize,
}

impl PreviewWorkerActivity {
    fn begin(
        self: &Arc<Self>,
        lane: MediaPreviewWorkerLane,
        priority: MediaPreviewRequestPriority,
        access_mode: PreviewDecodeAccessMode,
    ) -> PreviewWorkerActivityLease {
        self.in_flight_jobs.fetch_add(1, Ordering::AcqRel);
        match priority {
            MediaPreviewRequestPriority::Current => {
                self.in_flight_current_jobs.fetch_add(1, Ordering::AcqRel);
            }
            MediaPreviewRequestPriority::Prefetch => {
                self.in_flight_prefetch_jobs.fetch_add(1, Ordering::AcqRel);
            }
        }
        match access_mode {
            PreviewDecodeAccessMode::PlaybackCursor => {
                self.in_flight_playback_cursor_jobs.fetch_add(1, Ordering::AcqRel);
            }
            PreviewDecodeAccessMode::ScrubCursor => {
                self.in_flight_scrub_cursor_jobs.fetch_add(1, Ordering::AcqRel);
            }
            PreviewDecodeAccessMode::RandomAccessStillFrame => {
                self.in_flight_random_access_still_jobs.fetch_add(1, Ordering::AcqRel);
            }
        }
        match lane {
            MediaPreviewWorkerLane::Any => {
                self.in_flight_any_lane_jobs.fetch_add(1, Ordering::AcqRel);
            }
            MediaPreviewWorkerLane::Playback => {
                self.in_flight_playback_lane_jobs.fetch_add(1, Ordering::AcqRel);
            }
            MediaPreviewWorkerLane::Scrub => {
                self.in_flight_scrub_lane_jobs.fetch_add(1, Ordering::AcqRel);
            }
            MediaPreviewWorkerLane::Still => {
                self.in_flight_still_lane_jobs.fetch_add(1, Ordering::AcqRel);
            }
            MediaPreviewWorkerLane::Interactive => {
                self.in_flight_interactive_lane_jobs.fetch_add(1, Ordering::AcqRel);
            }
        }
        if priority == MediaPreviewRequestPriority::Current && !lane.accepts(access_mode) {
            self.in_flight_cross_lane_current_jobs.fetch_add(1, Ordering::AcqRel);
        }
        PreviewWorkerActivityLease {
            activity: Arc::clone(self),
            lane,
            priority,
            access_mode,
        }
    }

    fn snapshot(&self) -> PreviewWorkerActivityDiagnostics {
        PreviewWorkerActivityDiagnostics {
            in_flight_jobs: self.in_flight_jobs.load(Ordering::Acquire),
            in_flight_current_jobs: self.in_flight_current_jobs.load(Ordering::Acquire),
            in_flight_prefetch_jobs: self.in_flight_prefetch_jobs.load(Ordering::Acquire),
            in_flight_playback_cursor_jobs: self
                .in_flight_playback_cursor_jobs
                .load(Ordering::Acquire),
            in_flight_scrub_cursor_jobs: self.in_flight_scrub_cursor_jobs.load(Ordering::Acquire),
            in_flight_random_access_still_jobs: self
                .in_flight_random_access_still_jobs
                .load(Ordering::Acquire),
            in_flight_any_lane_jobs: self.in_flight_any_lane_jobs.load(Ordering::Acquire),
            in_flight_playback_lane_jobs: self.in_flight_playback_lane_jobs.load(Ordering::Acquire),
            in_flight_scrub_lane_jobs: self.in_flight_scrub_lane_jobs.load(Ordering::Acquire),
            in_flight_still_lane_jobs: self.in_flight_still_lane_jobs.load(Ordering::Acquire),
            in_flight_interactive_lane_jobs: self
                .in_flight_interactive_lane_jobs
                .load(Ordering::Acquire),
            in_flight_cross_lane_current_jobs: self
                .in_flight_cross_lane_current_jobs
                .load(Ordering::Acquire),
        }
    }
}

struct PreviewWorkerActivityLease {
    activity: Arc<PreviewWorkerActivity>,
    lane: MediaPreviewWorkerLane,
    priority: MediaPreviewRequestPriority,
    access_mode: PreviewDecodeAccessMode,
}

impl Drop for PreviewWorkerActivityLease {
    fn drop(&mut self) {
        self.activity.in_flight_jobs.fetch_sub(1, Ordering::AcqRel);
        match self.priority {
            MediaPreviewRequestPriority::Current => {
                self.activity.in_flight_current_jobs.fetch_sub(1, Ordering::AcqRel);
            }
            MediaPreviewRequestPriority::Prefetch => {
                self.activity.in_flight_prefetch_jobs.fetch_sub(1, Ordering::AcqRel);
            }
        }
        match self.access_mode {
            PreviewDecodeAccessMode::PlaybackCursor => {
                self.activity.in_flight_playback_cursor_jobs.fetch_sub(1, Ordering::AcqRel);
            }
            PreviewDecodeAccessMode::ScrubCursor => {
                self.activity.in_flight_scrub_cursor_jobs.fetch_sub(1, Ordering::AcqRel);
            }
            PreviewDecodeAccessMode::RandomAccessStillFrame => {
                self.activity.in_flight_random_access_still_jobs.fetch_sub(1, Ordering::AcqRel);
            }
        }
        match self.lane {
            MediaPreviewWorkerLane::Any => {
                self.activity.in_flight_any_lane_jobs.fetch_sub(1, Ordering::AcqRel);
            }
            MediaPreviewWorkerLane::Playback => {
                self.activity.in_flight_playback_lane_jobs.fetch_sub(1, Ordering::AcqRel);
            }
            MediaPreviewWorkerLane::Scrub => {
                self.activity.in_flight_scrub_lane_jobs.fetch_sub(1, Ordering::AcqRel);
            }
            MediaPreviewWorkerLane::Still => {
                self.activity.in_flight_still_lane_jobs.fetch_sub(1, Ordering::AcqRel);
            }
            MediaPreviewWorkerLane::Interactive => {
                self.activity.in_flight_interactive_lane_jobs.fetch_sub(1, Ordering::AcqRel);
            }
        }
        if self.priority == MediaPreviewRequestPriority::Current
            && !self.lane.accepts(self.access_mode)
        {
            self.activity.in_flight_cross_lane_current_jobs.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

impl AppUiPreviewService {
    fn schedule_media_prefetches(
        &self,
        state: &AppState,
        sequence: &Sequence,
        frame: i64,
        target_width: u32,
        target_height: u32,
    ) {
        if !state.is_playing() {
            return;
        }
        if self.current_frame_pending.get() {
            bump(&self.metrics.prefetch_skipped_current_pending);
            return;
        }
        if self.playback_sustained_pressure_active() {
            bump(&self.metrics.playback_prefetch_skipped_sustained_pressure);
            return;
        }
        let worker_queue = self.jobs.diagnostics();
        let worker_activity = self.worker_activity.snapshot();
        if worker_queue.queued_current_jobs > 0 || worker_activity.in_flight_current_jobs > 0 {
            bump(&self.metrics.prefetch_skipped_worker_busy);
            return;
        }
        let prefetch_pressure = worker_queue
            .queued_prefetch_jobs
            .saturating_add(worker_activity.in_flight_prefetch_jobs);
        let prefetch_window_frames =
            media_preview_forward_prefetch_window_frames(sequence.settings.frame_rate);
        self.record_playback_forward_prefetch_window(prefetch_window_frames);
        let Some(prefetch_window_frames) = prefetch_window_frames else {
            return;
        };
        let prefetch_slots_available = prefetch_window_frames.saturating_sub(prefetch_pressure);
        if prefetch_slots_available == 0 {
            bump(&self.metrics.prefetch_skipped_prefetch_backlog);
            return;
        }
        let display_snapshot = self.display_snapshot.borrow();
        let Ok(display_color_space) = preview_display_color_space(
            sequence,
            &state.project_settings.color_management,
            display_snapshot.as_ref(),
        ) else {
            return;
        };
        let color_context = sequence.settings.root_preview_color_context(
            &state.project_settings.color_management,
            display_color_space,
        );
        let mut remaining_prefetch_jobs = prefetch_slots_available;
        for offset in 1..=prefetch_window_frames as i64 {
            if remaining_prefetch_jobs == 0 {
                break;
            }
            self.schedule_media_prefetch_for_sequence(
                state,
                sequence,
                frame.saturating_add(offset),
                target_width,
                target_height,
                0,
                color_context.clone(),
                &mut remaining_prefetch_jobs,
            );
        }
    }

    fn schedule_media_prefetch_for_sequence(
        &self,
        state: &AppState,
        sequence: &Sequence,
        frame: i64,
        target_width: u32,
        target_height: u32,
        depth: usize,
        color_context: ColorContext,
        remaining_prefetch_jobs: &mut usize,
    ) {
        if depth > MAX_NESTED_SEQUENCE_RENDER_DEPTH || *remaining_prefetch_jobs == 0 {
            return;
        }
        let evaluation = evaluate_timeline_render_plan(
            sequence,
            TimelineEvaluationRequest::preview(
                frame.max(0),
                normalize_preview_resolution_scale(sequence.settings.preview.resolution_scale),
            ),
        );

        for element in evaluation.elements {
            if *remaining_prefetch_jobs == 0 {
                break;
            }
            match element {
                TimelineRenderPlanElement::Media(media) => {
                    let Some((key, source_secs)) = self.media_preview_key_for_asset(
                        state,
                        &media.asset_id,
                        media.color_space_override,
                        media.source_frame,
                        media.source_secs,
                        target_width,
                        target_height,
                        &color_context,
                        false,
                        false,
                    ) else {
                        continue;
                    };
                    if self.cached_media_frame(&key).is_none() && !self.failed_media_key(&key) {
                        let enqueued = self.request_media_preview(
                            key,
                            source_secs,
                            MediaPreviewRequestPriority::Prefetch,
                            PreviewDecodeAccessMode::PlaybackCursor,
                            None,
                            PreviewDecodeAdaptiveHints::default(),
                        );
                        if enqueued {
                            *remaining_prefetch_jobs = (*remaining_prefetch_jobs).saturating_sub(1);
                        }
                    }
                }
                TimelineRenderPlanElement::NestedSequence(nested) => {
                    if let Some(nested_sequence) = state.sequence_by_id(nested.sequence_id) {
                        let (nested_width, nested_height) =
                            preview_dimensions_for_sequence(nested_sequence);
                        let nested_context = nested_sequence
                            .settings
                            .nested_render_color_context(color_context.clone());
                        self.schedule_media_prefetch_for_sequence(
                            state,
                            nested_sequence,
                            nested.source_frame,
                            nested_width,
                            nested_height,
                            depth + 1,
                            nested_context,
                            remaining_prefetch_jobs,
                        );
                    }
                }
                TimelineRenderPlanElement::SolidColor(_)
                | TimelineRenderPlanElement::Adjustment(_) => {}
            }
        }
    }

    fn media_frame_for_plan(
        &self,
        state: &AppState,
        asset_id: &AssetId,
        color_space_override: Option<ColorSpace>,
        source_frame: i64,
        source_secs: f64,
        target_width: u32,
        target_height: u32,
        color_context: &ColorContext,
        sequence_frame_rate: Rational,
    ) -> Option<MediaPreviewFrame> {
        let access_mode = media_preview_access_mode_for_intent(media_preview_viewer_access_intent(
            state.is_playing(),
            state.last_timeline_seek_source,
        ));
        let (key, source_secs) = self.media_preview_key_for_asset(
            state,
            asset_id,
            color_space_override,
            source_frame,
            source_secs,
            target_width,
            target_height,
            color_context,
            true,
            access_mode == PreviewDecodeAccessMode::PlaybackCursor,
        )?;
        if let Some(frame) = self.cached_media_frame(&key) {
            return Some(frame);
        }
        if self.failed_media_key(&key) {
            return None;
        }
        self.current_frame_pending.set(true);
        let adaptive_hints = self.preview_decode_adaptive_hints(access_mode, &key);
        self.request_media_preview(
            key,
            source_secs,
            MediaPreviewRequestPriority::Current,
            access_mode,
            media_preview_playback_current_deadline_budget_us(sequence_frame_rate),
            adaptive_hints,
        );
        None
    }

    fn cached_media_frame(&self, key: &MediaPreviewKey) -> Option<MediaPreviewFrame> {
        let frame = self.media_cache.borrow_mut().get(key);
        if frame.is_some() {
            bump(&self.metrics.media_cache_hits);
        } else {
            bump(&self.metrics.media_cache_misses);
        }
        frame
    }

    fn failed_media_key(&self, key: &MediaPreviewKey) -> bool {
        let failed = self.media_failures.borrow_mut().contains(key);
        if failed {
            bump(&self.metrics.media_failure_hits);
        }
        failed
    }

    fn media_preview_key_for_asset(
        &self,
        state: &AppState,
        asset_id: &AssetId,
        color_space_override: Option<ColorSpace>,
        source_frame: i64,
        source_secs: f64,
        target_width: u32,
        target_height: u32,
        color_context: &ColorContext,
        record_color_rejection: bool,
        request_missing_proxy_generation: bool,
    ) -> Option<(MediaPreviewKey, f64)> {
        let library = state.asset_library.as_ref()?;
        let asset = match library.get_asset(*asset_id) {
            Ok(Some(asset)) if asset.kind == AssetKind::Video => asset,
            Ok(_) => return None,
            Err(err) => {
                tracing::debug!(asset_id = %asset_id, "viewer preview asset lookup failed: {err}");
                return None;
            }
        };
        let proxy_config = state.proxy_config();
        let resolved_path = resolve_preview_media_decode_path(
            state.project_settings.proxy_enabled && state.is_asset_proxy_mode(*asset_id),
            &asset.path,
            &proxy_config,
        )?;
        match resolved_path.resolution {
            PreviewMediaDecodePathResolution::Proxy => bump(&self.metrics.media_proxy_path_hits),
            PreviewMediaDecodePathResolution::ProxyMissing => {
                bump(&self.metrics.media_proxy_path_misses);
            }
            PreviewMediaDecodePathResolution::ProxyStale => {
                bump(&self.metrics.media_proxy_path_stale);
            }
            PreviewMediaDecodePathResolution::Source => {
                bump(&self.metrics.media_proxy_path_bypasses);
            }
        }
        self.maybe_request_preview_proxy_generation(
            request_missing_proxy_generation,
            state,
            *asset_id,
            &asset.path,
            &proxy_config,
            &resolved_path,
        );

        let detected_color_space = asset
            .media_info
            .video_streams
            .first()
            .and_then(|video| video.detected_color_space);
        let input_color_resolution = resolve_preview_input_color_space(
            color_space_override,
            asset.interpretation,
            detected_color_space,
            color_context,
        );
        self.record_input_color_resolution(input_color_resolution.source);
        let input_color_space = match input_color_resolution.color_space {
            Some(color_space) => color_space,
            None => {
                let diagnostic = asset
                    .media_info
                    .primary_video()
                    .map(VideoColorDiagnostic::from_stream)
                    .unwrap_or_else(|| VideoColorDiagnostic {
                        detected_color_space: None,
                        interpretation: mondrian_media::DetectedColorInterpretation {
                            color_space: None,
                            confidence: mondrian_media::VideoColorInterpretationConfidence::None,
                            source: mondrian_media::VideoColorSpaceSource::MissingMetadata,
                            method: mondrian_media::VideoColorDetectionMethod::MissingMetadata,
                            evidence: Vec::new(),
                            warnings: Vec::new(),
                            user_overridable: true,
                        },
                        source: mondrian_media::VideoColorSpaceSource::MissingMetadata,
                        method: mondrian_media::VideoColorDetectionMethod::MissingMetadata,
                        metadata: None,
                        metadata_hints: Vec::new(),
                        hdr_metadata: Vec::new(),
                    });
                let diagnostic_summary = diagnostic.summary();
                let diagnostic_issue_summary = diagnostic.issue_summary();
                if record_color_rejection {
                    self.record_color_rejection(AppUiPreviewColorRejection::new(
                        *asset_id,
                        asset.path.clone(),
                        input_color_resolution,
                        diagnostic_summary.clone(),
                        diagnostic_issue_summary,
                    ));
                }
                tracing::warn!(
                    asset_id = %asset_id,
                    path = %asset.path.display(),
                    missing_metadata_policy = ?color_context.missing_metadata_policy,
                    color_resolution_source = ?input_color_resolution.source,
                    override_color_space = ?input_color_resolution.override_color_space,
                    detected_color_space = ?input_color_resolution.detected_color_space,
                    working_color_space = ?input_color_resolution.working_color_space,
                    color_diagnostic = %diagnostic_summary,
                    color_diagnostic_issue_summary = ?diagnostic_issue_summary,
                    "viewer preview rejected media with missing color metadata"
                );
                return None;
            }
        };
        Some((
            MediaPreviewKey {
                asset_id: *asset_id,
                path: resolved_path.path,
                fingerprint: Some(resolved_path.fingerprint),
                source_frame: source_frame.max(0),
                source_micros: source_micros(source_secs),
                target_width,
                target_height,
                input_color_space,
                working_color_space: color_context.working_color_space,
                tone_map: color_context.tone_map,
                engine: color_context.engine.clone(),
            },
            source_secs.max(0.0),
        ))
    }

    fn maybe_request_preview_proxy_generation(
        &self,
        request_missing_proxy_generation: bool,
        state: &AppState,
        asset_id: AssetId,
        source_path: &Path,
        proxy_config: &mondrian_media::ProxyConfig,
        resolved_path: &PreviewMediaDecodePath,
    ) {
        if !should_request_preview_proxy_generation(
            request_missing_proxy_generation,
            state.project_settings.proxy_enabled,
            state.is_asset_proxy_mode(asset_id),
            resolved_path.resolution,
        ) {
            return;
        }

        let request_key = PreviewProxyGenerationRequestKey {
            asset_id,
            source_fingerprint: resolved_path.fingerprint,
            resolution: resolved_path.resolution,
        };
        if !self.requested_proxy_generations.borrow_mut().insert(request_key) {
            bump(&self.metrics.media_proxy_generation_request_dedupes);
            return;
        }

        bump(&self.metrics.media_proxy_generation_requests);
        request_proxy_generation(asset_id, source_path.to_path_buf(), proxy_config.clone());
    }

    fn preview_decode_adaptive_hints(
        &self,
        access_mode: PreviewDecodeAccessMode,
        key: &MediaPreviewKey,
    ) -> PreviewDecodeAdaptiveHints {
        if access_mode != PreviewDecodeAccessMode::ScrubCursor {
            return PreviewDecodeAdaptiveHints::default();
        }
        let hints = self.scrub_adaptation.borrow_mut().observe_request(key);
        match hints.scrub_class {
            PreviewScrubAdaptiveClass::Normal => {
                bump(&self.metrics.scrub_adaptive_normal_requests);
            }
            PreviewScrubAdaptiveClass::HotRegion => {
                bump(&self.metrics.scrub_adaptive_hot_region_requests);
            }
            PreviewScrubAdaptiveClass::SlowLatency => {
                bump(&self.metrics.scrub_adaptive_slow_latency_requests);
            }
            PreviewScrubAdaptiveClass::Recovery => {
                bump(&self.metrics.scrub_adaptive_recovery_requests);
            }
        }
        hints
    }

    fn request_media_preview(
        &self,
        key: MediaPreviewKey,
        source_secs: f64,
        priority: MediaPreviewRequestPriority,
        access_mode: PreviewDecodeAccessMode,
        playback_current_deadline_budget_us: Option<u64>,
        adaptive_hints: PreviewDecodeAdaptiveHints,
    ) -> bool {
        let generation = self.current_generation.get();
        let is_current_playback = priority == MediaPreviewRequestPriority::Current
            && access_mode == PreviewDecodeAccessMode::PlaybackCursor;
        let should_enqueue_job = match self.scheduler.request(
            key.clone(),
            generation,
            priority,
            access_mode,
        ) {
            MediaPreviewRequestStatus::Scheduled { evicted_prefetch, evicted_still } => {
                if is_current_playback {
                    self.record_playback_current_deadline_budget(
                        playback_current_deadline_budget_us,
                    );
                }
                if let Some(evicted_key) = evicted_prefetch {
                    let canceled = self.jobs.cancel_key(&evicted_key) as u64;
                    add_cell(&self.metrics.queue_canceled_jobs, canceled);
                }
                if let Some(evicted_key) = evicted_still {
                    let canceled = self.jobs.cancel_key(&evicted_key) as u64;
                    add_cell(&self.metrics.queue_canceled_jobs, canceled);
                }
                true
            }
            MediaPreviewRequestStatus::AlreadyPending { access_mode_changed } => {
                let enqueued_at = Instant::now();
                let queued_update = self.jobs.promote(
                    &key,
                    priority,
                    access_mode,
                    generation,
                    source_secs,
                    enqueued_at,
                    media_preview_job_deadline_at(
                        priority,
                        access_mode,
                        enqueued_at,
                        playback_current_deadline_budget_us,
                    ),
                    adaptive_hints,
                );
                if queued_update.priority_promoted {
                    bump(&self.metrics.queue_promoted_current_jobs);
                }
                if is_current_playback {
                    self.record_playback_current_deadline_budget(
                        playback_current_deadline_budget_us,
                    );
                }
                access_mode_changed && !queued_update.updated
            }
            MediaPreviewRequestStatus::DroppedBackpressure => {
                tracing::trace!(
                    asset_id = %key.asset_id,
                    source_frame = key.source_frame,
                    "viewer preview request dropped by backpressure"
                );
                return false;
            }
            MediaPreviewRequestStatus::DroppedInvalidAccessMode => {
                tracing::warn!(
                    asset_id = %key.asset_id,
                    source_frame = key.source_frame,
                    priority = ?priority,
                    access_mode = access_mode.as_str(),
                    "viewer preview request dropped because priority/access-mode pair is invalid"
                );
                return false;
            }
        };
        if !should_enqueue_job {
            return false;
        }
        let enqueued_at = Instant::now();
        let job = MediaPreviewJob {
            key: key.clone(),
            source_secs,
            generation,
            priority,
            access_mode,
            adaptive_hints,
            enqueued_at,
            deadline_at: media_preview_job_deadline_at(
                priority,
                access_mode,
                enqueued_at,
                playback_current_deadline_budget_us,
            ),
        };
        if priority == MediaPreviewRequestPriority::Current {
            let pruned = self.jobs.prune_obsolete_jobs(generation) as u64;
            add_cell(&self.metrics.queue_pruned_obsolete_jobs, pruned);
        }
        match self.jobs.enqueue(job) {
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch, evicted_still } => {
                bump(&self.metrics.enqueued_jobs);
                if let Some(evicted_key) = evicted_prefetch {
                    bump(&self.metrics.queue_evicted_prefetch_jobs);
                    let canceled = self.jobs.cancel_key(&evicted_key) as u64;
                    add_cell(&self.metrics.queue_canceled_jobs, canceled);
                    self.scheduler.cancel(&evicted_key);
                }
                if let Some(evicted_key) = evicted_still {
                    bump(&self.metrics.queue_evicted_still_jobs);
                    let canceled = self.jobs.cancel_key(&evicted_key) as u64;
                    add_cell(&self.metrics.queue_canceled_jobs, canceled);
                    self.scheduler.cancel(&evicted_key);
                }
                true
            }
            MediaPreviewJobEnqueueStatus::DroppedFull => {
                bump(&self.metrics.queue_full_drops);
                let canceled = self.jobs.cancel_key(&key) as u64;
                add_cell(&self.metrics.queue_canceled_jobs, canceled);
                self.scheduler.cancel(&key);
                tracing::trace!(
                    asset_id = %key.asset_id,
                    source_frame = key.source_frame,
                    "viewer preview queue full; dropping media preview request"
                );
                false
            }
            MediaPreviewJobEnqueueStatus::DroppedInvalidAccessMode => {
                bump(&self.metrics.queue_invalid_access_mode_drops);
                self.scheduler.cancel(&key);
                tracing::warn!(
                    asset_id = %key.asset_id,
                    source_frame = key.source_frame,
                    priority = ?priority,
                    access_mode = access_mode.as_str(),
                    "viewer preview queue rejected invalid priority/access-mode pair"
                );
                false
            }
            MediaPreviewJobEnqueueStatus::Closed => {
                bump(&self.metrics.worker_disconnected_drops);
                let canceled = self.jobs.cancel_key(&key) as u64;
                add_cell(&self.metrics.queue_canceled_jobs, canceled);
                self.scheduler.cancel(&key);
                tracing::debug!("viewer preview worker unavailable");
                false
            }
        }
    }

    fn record_color_rejection(&self, rejection: AppUiPreviewColorRejection) {
        self.last_color_rejection.replace(Some(rejection));
    }
}

fn resolve_preview_input_color_space(
    override_color_space: Option<ColorSpace>,
    asset_interpretation: AssetMediaInterpretation,
    detected_color_space: Option<ColorSpace>,
    color_context: &ColorContext,
) -> InputColorResolution {
    color_context.missing_metadata_policy.resolve_asset_input_decision(
        override_color_space,
        asset_interpretation,
        detected_color_space,
        color_context.working_color_space,
    )
}

/// Collect preview input color-resolution source counts for one timeline frame.
///
/// This uses the same preview-intent render-plan evaluation as the viewer. It
/// intentionally returns source counts only; output/display differences are
/// handled by the preview color context and must not change input
/// interpretation source categories.
pub fn preview_input_color_resolution_counts_for_frame(
    sequence: &Sequence,
    sequences: &[Sequence],
    asset_color_spaces: &HashMap<AssetId, ColorSpace>,
    asset_interpretations: &HashMap<AssetId, AssetMediaInterpretation>,
    project_color_management: &mondrian_core::ProjectColorManagement,
    display_color_space: ColorSpace,
    frame: i64,
) -> Result<InputColorResolutionSourceCounts, String> {
    let color_context = sequence
        .settings
        .root_preview_color_context(project_color_management, display_color_space);
    preview_sequence_input_color_resolution_counts(
        sequence,
        sequences,
        asset_color_spaces,
        asset_interpretations,
        frame,
        color_context,
        0,
    )
}

fn preview_sequence_input_color_resolution_counts(
    sequence: &Sequence,
    sequences: &[Sequence],
    asset_color_spaces: &HashMap<AssetId, ColorSpace>,
    asset_interpretations: &HashMap<AssetId, AssetMediaInterpretation>,
    frame: i64,
    color_context: ColorContext,
    depth: usize,
) -> Result<InputColorResolutionSourceCounts, String> {
    if depth > MAX_NESTED_SEQUENCE_RENDER_DEPTH {
        return Err("预览序列嵌套层级过深，已停止统计输入色彩解析".to_string());
    }

    let render_plan = evaluate_timeline_render_plan(
        sequence,
        TimelineEvaluationRequest::preview(
            frame,
            normalize_preview_resolution_scale(sequence.settings.preview.resolution_scale),
        ),
    );
    let mut counts = InputColorResolutionSourceCounts::default();
    for element in render_plan.elements {
        match element {
            TimelineRenderPlanElement::Media(media) => {
                let detected_color_space = asset_color_spaces.get(&media.asset_id).copied();
                let asset_interpretation =
                    asset_interpretations.get(&media.asset_id).copied().unwrap_or_default();
                let resolution = resolve_preview_input_color_space(
                    media.color_space_override,
                    asset_interpretation,
                    detected_color_space,
                    &color_context,
                );
                counts.record(resolution.source);
            }
            TimelineRenderPlanElement::NestedSequence(nested) => {
                let Some(nested_sequence) =
                    sequences.iter().find(|sequence| sequence.id == nested.sequence_id)
                else {
                    return Err(format!("嵌套序列不存在: {}", nested.sequence_id));
                };
                let nested_context =
                    nested_sequence.settings.nested_render_color_context(color_context.clone());
                let nested_counts = preview_sequence_input_color_resolution_counts(
                    nested_sequence,
                    sequences,
                    asset_color_spaces,
                    asset_interpretations,
                    nested.source_frame,
                    nested_context,
                    depth + 1,
                )?;
                counts.accumulate(nested_counts);
            }
            TimelineRenderPlanElement::Adjustment(_) | TimelineRenderPlanElement::SolidColor(_) => {
            }
        }
    }
    Ok(counts)
}

#[derive(Default)]
struct AppUiPreviewMetrics {
    render_requests: Cell<u64>,
    ready_frames: Cell<u64>,
    loading_frames: Cell<u64>,
    stale_frames: Cell<u64>,
    unavailable_frames: Cell<u64>,
    playback_current_stalled_expirations: Cell<u64>,
    gpu_preview_candidate_requests: Cell<u64>,
    gpu_preview_candidate_ready: Cell<u64>,
    gpu_preview_candidate_current: Cell<u64>,
    gpu_preview_candidate_loading: Cell<u64>,
    gpu_preview_candidate_unavailable: Cell<u64>,
    gpu_preview_candidate_pixels: Cell<u64>,
    gpu_preview_external_frames_registered: Cell<u64>,
    gpu_preview_external_frames_rejected: Cell<u64>,
    gpu_preview_external_frames_cleared: Cell<u64>,
    input_color_resolution_override: Cell<u64>,
    input_color_resolution_detected_metadata: Cell<u64>,
    input_color_resolution_missing_assume_rec709: Cell<u64>,
    input_color_resolution_missing_assume_working: Cell<u64>,
    input_color_resolution_missing_rejected: Cell<u64>,
    input_color_resolution_data_texture: Cell<u64>,
    media_proxy_path_hits: Cell<u64>,
    media_proxy_path_misses: Cell<u64>,
    media_proxy_path_stale: Cell<u64>,
    media_proxy_path_bypasses: Cell<u64>,
    media_proxy_generation_requests: Cell<u64>,
    media_proxy_generation_request_dedupes: Cell<u64>,
    scrub_adaptive_normal_requests: Cell<u64>,
    scrub_adaptive_hot_region_requests: Cell<u64>,
    scrub_adaptive_slow_latency_requests: Cell<u64>,
    scrub_adaptive_recovery_requests: Cell<u64>,
    viewer_frame_cache_hits: Cell<u64>,
    viewer_frame_cache_misses: Cell<u64>,
    media_cache_hits: Cell<u64>,
    media_cache_misses: Cell<u64>,
    media_failure_hits: Cell<u64>,
    decode_successes: Cell<u64>,
    decode_failures: Cell<u64>,
    decode_timeout_failures: Cell<u64>,
    decode_budget_exhausted_failures: Cell<u64>,
    decode_canceled_jobs: Cell<u64>,
    decode_canceled_shutdown_jobs: Cell<u64>,
    decode_canceled_obsolete_jobs: Cell<u64>,
    decode_canceled_prefetch_deadline_jobs: Cell<u64>,
    decode_canceled_playback_deadline_jobs: Cell<u64>,
    decode_canceled_prefetch_preempted_jobs: Cell<u64>,
    decode_canceled_still_preempted_jobs: Cell<u64>,
    decode_canceled_unknown_jobs: Cell<u64>,
    decode_canceled_total_duration_us: Cell<u64>,
    decode_canceled_max_duration_us: Cell<u64>,
    decode_canceled_last_duration_us: Cell<u64>,
    decode_canceled_return_latency_total_us: Cell<u64>,
    decode_canceled_return_latency_max_us: Cell<u64>,
    decode_canceled_return_latency_last_us: Cell<u64>,
    decode_in_process_cpu_rgba_frames: Cell<u64>,
    decode_external_ffmpeg_cpu_rgba_frames: Cell<u64>,
    decode_playback_session_ring_hit_frames: Cell<u64>,
    decode_cache_hit_frames: Cell<u64>,
    decode_playback_cursor_frames: Cell<u64>,
    decode_scrub_cursor_frames: Cell<u64>,
    decode_random_access_still_frames: Cell<u64>,
    decode_canceled_playback_cursor_jobs: Cell<u64>,
    decode_canceled_scrub_cursor_jobs: Cell<u64>,
    decode_canceled_random_access_still_jobs: Cell<u64>,
    decode_total_duration_us: Cell<u64>,
    decode_max_duration_us: Cell<u64>,
    decode_last_duration_us: Cell<u64>,
    decode_queue_wait_total_us: Cell<u64>,
    decode_queue_wait_max_us: Cell<u64>,
    decode_queue_wait_last_us: Cell<u64>,
    decode_current_queue_wait_max_us: Cell<u64>,
    decode_prefetch_queue_wait_max_us: Cell<u64>,
    decode_seeked_frames: Cell<u64>,
    decode_decoded_frame_count: Cell<u64>,
    decode_max_decoded_frame_count: Cell<u64>,
    decode_threading_none_frames: Cell<u64>,
    decode_threading_frame_frames: Cell<u64>,
    decode_threading_slice_frames: Cell<u64>,
    decode_last_threading_count: Cell<u64>,
    decode_max_threading_count: Cell<u64>,
    decode_stage_durations: Cell<PreviewDecodeStageDurations>,
    decode_max_frame_stage_durations: Cell<PreviewDecodeStageDurations>,
    decode_max_frame_queue_wait_us: Cell<u64>,
    decode_max_frame_bottleneck: Cell<AppUiPreviewDecodeBottleneck>,
    decode_access_mode_profiles: Cell<AppUiPreviewDecodeAccessModeProfiles>,
    render_timed_frames: Cell<u64>,
    render_total_duration_us: Cell<u64>,
    render_max_duration_us: Cell<u64>,
    render_last_duration_us: Cell<u64>,
    render_stage_durations: Cell<AppUiPreviewRenderStageDurations>,
    render_max_frame_stage_durations: Cell<AppUiPreviewRenderStageDurations>,
    completion_poll_calls: Cell<u64>,
    completion_poll_results: Cell<u64>,
    completion_poll_total_duration_us: Cell<u64>,
    completion_poll_max_duration_us: Cell<u64>,
    completion_poll_last_duration_us: Cell<u64>,
    completion_poll_max_results_per_poll: Cell<u64>,
    completion_poll_count_budget_exhaustions: Cell<u64>,
    completion_poll_time_budget_exhaustions: Cell<u64>,
    enqueued_jobs: Cell<u64>,
    prefetch_skipped_current_pending: Cell<u64>,
    prefetch_skipped_worker_busy: Cell<u64>,
    prefetch_skipped_prefetch_backlog: Cell<u64>,
    queue_full_drops: Cell<u64>,
    queue_invalid_access_mode_drops: Cell<u64>,
    queue_evicted_prefetch_jobs: Cell<u64>,
    queue_evicted_still_jobs: Cell<u64>,
    interactive_cancel_requests: Cell<u64>,
    interactive_cancel_scheduler_requests: Cell<u64>,
    interactive_cancel_queued_jobs: Cell<u64>,
    queue_canceled_jobs: Cell<u64>,
    queue_pruned_obsolete_jobs: Cell<u64>,
    queue_promoted_current_jobs: Cell<u64>,
    worker_disconnected_drops: Cell<u64>,
    playback_current_deadline_budget_us: Cell<Option<u64>>,
    playback_current_deadline_assignments: Cell<u64>,
    playback_current_deadline_missing_frame_rate: Cell<u64>,
    playback_current_decode_decisions: Cell<u64>,
    playback_current_drop_late_decisions: Cell<u64>,
    playback_current_proxy_or_hardware_recommended_decisions: Cell<u64>,
    playback_current_late_streak: Cell<u64>,
    playback_sustained_pressure_events: Cell<u64>,
    playback_sustained_pressure_recoveries: Cell<u64>,
    playback_prefetch_skipped_sustained_pressure: Cell<u64>,
    playback_forward_prefetch_window_frames: Cell<Option<usize>>,
    playback_forward_prefetch_window_evaluations: Cell<u64>,
    playback_forward_prefetch_invalid_frame_rate: Cell<u64>,
    color_input_transform_calls: Cell<u64>,
    color_input_transform_pixels: Cell<u64>,
    color_output_transform_calls: Cell<u64>,
    color_output_transform_pixels: Cell<u64>,
    color_rgba8_boundary_calls: Cell<u64>,
    color_stage_plans: Cell<u64>,
    color_stage_total_stages: Cell<u64>,
    color_stage_cpu_input_stages: Cell<u64>,
    color_stage_cpu_output_stages: Cell<u64>,
    color_stage_gpu_color_stages: Cell<u64>,
    color_stage_upload_stages: Cell<u64>,
    color_stage_readback_stages: Cell<u64>,
    color_stage_gpu_blockers: Cell<u64>,
    color_stage_gpu_shader_module_blockers: Cell<u64>,
    color_stage_gpu_ocio_resource_blockers: Cell<u64>,
    color_stage_gpu_wrapper_blockers: Cell<u64>,
    color_stage_gpu_render_pipeline_blockers: Cell<u64>,
    color_stage_gpu_ocio_config_blockers: Cell<u64>,
    color_stage_gpu_ocio_processor_blockers: Cell<u64>,
    color_stage_gpu_ocio_shader_extraction_blockers: Cell<u64>,
    color_stage_pixels: Cell<u64>,
    color_composite_plans: Cell<u64>,
    color_composite_elements: Cell<u64>,
    color_composite_float_linear: Cell<u64>,
    color_composite_legacy_rgba8: Cell<u64>,
    color_composite_legacy_media_blend_mode: Cell<u64>,
    color_composite_legacy_media_transform: Cell<u64>,
    color_composite_legacy_media_effect: Cell<u64>,
    color_composite_legacy_solid_blend_mode: Cell<u64>,
    color_composite_legacy_solid_transform: Cell<u64>,
    color_composite_legacy_solid_effect: Cell<u64>,
    color_composite_legacy_adjustment_blend_mode: Cell<u64>,
    color_composite_legacy_adjustment_effect: Cell<u64>,
    cpu_output_fallback_frames: Cell<u64>,
    cpu_output_fallback_pixels: Cell<u64>,
    preview_gpu_output_blocker_frames: Cell<u64>,
    preview_gpu_output_blocker_breakdown:
        RefCell<crate::app_ui::preview_gpu_output_blocker::PreviewGpuOutputBlockerBreakdown>,
    gpu_compositing: RefCell<mondrian_renderer::GpuCompositingDiagnostics>,
}

fn bump(counter: &Cell<u64>) {
    counter.set(counter.get().saturating_add(1));
}

fn add_cell(counter: &Cell<u64>, delta: u64) {
    counter.set(counter.get().saturating_add(delta));
}

fn app_duration_us(duration: Duration) -> u64 {
    duration.as_micros().min(u128::from(u64::MAX)) as u64
}

fn viewer_preview_cache_key_for_resolved_plan(
    sequence_id: SequenceId,
    width: u32,
    height: u32,
    elements: &[ResolvedPreviewElement],
    color_context: &ColorContext,
) -> ViewerPreviewCacheKey {
    let mut hasher = DefaultHasher::new();
    color_context.working_color_space.hash(&mut hasher);
    color_context.output_color_space.hash(&mut hasher);
    color_context.tone_map.hash(&mut hasher);
    color_context.engine.hash(&mut hasher);
    color_context.display_management.hash(&mut hasher);
    color_context.ocio_display.hash(&mut hasher);
    color_context.ocio_view.hash(&mut hasher);
    mondrian_core::ocio_config_generation().hash(&mut hasher);
    elements.len().hash(&mut hasher);
    for element in elements {
        match element {
            ResolvedPreviewElement::SolidColor(solid) => {
                0u8.hash(&mut hasher);
                hash_color(solid.color, &mut hasher);
                solid.opacity.to_bits().hash(&mut hasher);
                solid.blend_mode.hash(&mut hasher);
                hash_transform(solid.transform, &mut hasher);
                hash_effect_graph_signature(&solid.effect_graph, solid.frame_seed, &mut hasher);
            }
            ResolvedPreviewElement::Adjustment(adjustment) => {
                1u8.hash(&mut hasher);
                adjustment.opacity.to_bits().hash(&mut hasher);
                adjustment.blend_mode.hash(&mut hasher);
                hash_effect_graph_signature(
                    &adjustment.effect_graph,
                    adjustment.frame_seed,
                    &mut hasher,
                );
            }
            ResolvedPreviewElement::Media {
                frame,
                opacity,
                blend_mode,
                transform,
                effect_graph,
                frame_seed,
            } => {
                2u8.hash(&mut hasher);
                frame.signature.hash(&mut hasher);
                frame.width().hash(&mut hasher);
                frame.height().hash(&mut hasher);
                opacity.to_bits().hash(&mut hasher);
                blend_mode.hash(&mut hasher);
                hash_transform(*transform, &mut hasher);
                hash_effect_graph_signature(effect_graph, *frame_seed, &mut hasher);
            }
        }
    }
    ViewerPreviewCacheKey {
        sequence_id,
        width,
        height,
        plan_signature: hasher.finish(),
    }
}

fn hash_color(color: mondrian_core::Color, hasher: &mut impl Hasher) {
    color.r.to_bits().hash(hasher);
    color.g.to_bits().hash(hasher);
    color.b.to_bits().hash(hasher);
    color.a.to_bits().hash(hasher);
}

fn hash_transform(transform: [f32; 6], hasher: &mut impl Hasher) {
    for value in transform {
        value.to_bits().hash(hasher);
    }
}

fn hash_effect_graph_signature(
    graph: &CompiledEffectGraph,
    frame_seed: i64,
    hasher: &mut impl Hasher,
) {
    graph.signature_hash.hash(hasher);
    graph.output_cache_policy.hash(hasher);
    if graph.output_cache_policy == EffectCachePolicy::FrameDependent {
        frame_seed.hash(hasher);
    }
}

fn media_preview_frame_signature(key: &MediaPreviewKey) -> u64 {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    hasher.finish()
}

fn nested_preview_frame_signature(
    sequence_id: SequenceId,
    frame: i64,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> u64 {
    let mut hasher = DefaultHasher::new();
    sequence_id.hash(&mut hasher);
    frame.hash(&mut hasher);
    width.hash(&mut hasher);
    height.hash(&mut hasher);
    rgba.hash(&mut hasher);
    hasher.finish()
}

fn preview_dimensions_for_sequence(sequence: &Sequence) -> (u32, u32) {
    let resolution = sequence.settings.resolution;
    let scale = normalize_preview_resolution_scale(sequence.settings.preview.resolution_scale);
    let width = ((resolution.width as f32 * scale).round() as u32).max(1);
    let height = ((resolution.height as f32 * scale).round() as u32).max(1);
    (width, height)
}

fn preview_display_color_space(
    sequence: &Sequence,
    project_cm: &mondrian_core::ProjectColorManagement,
    display_snapshot: Option<&DisplayOutputSnapshot>,
) -> Result<ColorSpace, crate::app_ui::preview_gpu_output_blocker::PreviewGpuOutputBlocker> {
    let sequence_output = sequence.settings.color_management.output_color_space;
    let display_management = if sequence.settings.color_management.inherit {
        &project_cm.display_management
    } else {
        &sequence.settings.color_management.display_management
    };
    let profile_space = match display_management.monitor_profile {
        mondrian_core::MonitorProfileReference::IccProfile { .. } => {
            preview_icc_display_color_space(display_snapshot)?
        }
        ref monitor => monitor.managed_color_space(sequence_output).unwrap_or(sequence_output),
    };

    Ok(
        match display_management.viewer_mode.resolve(profile_space) {
            mondrian_core::ResolvedViewerDisplayMode::Sdr => {
                if profile_space.is_hdr() {
                    ColorSpace::Rec709
                } else {
                    profile_space
                }
            }
            mondrian_core::ResolvedViewerDisplayMode::HdrPq => ColorSpace::Rec2100Pq,
            mondrian_core::ResolvedViewerDisplayMode::HdrHlg => ColorSpace::Rec2100Hlg,
        },
    )
}

fn preview_icc_display_color_space(
    display_snapshot: Option<&DisplayOutputSnapshot>,
) -> Result<ColorSpace, crate::app_ui::preview_gpu_output_blocker::PreviewGpuOutputBlocker> {
    let Some(snapshot) = display_snapshot else {
        return Err(
            crate::app_ui::preview_gpu_output_blocker::PreviewGpuOutputBlocker::UnsupportedFeature {
                feature: "icc_preview_color_space_resolution".to_owned(),
                reason: "MonitorProfileReference::IccProfile requires the display output contract to provide a resolved monitor color space before preview scheduling".to_owned(),
            },
        );
    };

    if !snapshot.is_valid() {
        return Err(super::display_probe_impl::preview_blockers_from_snapshot(snapshot)
            .into_iter()
            .next()
            .unwrap_or_else(|| {
                crate::app_ui::preview_gpu_output_blocker::PreviewGpuOutputBlocker::UnsupportedFeature {
                    feature: "display_output_contract_invalid".to_owned(),
                    reason: "display output contract is invalid for ICC preview scheduling"
                        .to_owned(),
                }
            }));
    }

    match snapshot.monitor_profile_status {
        MonitorProfileStatus::ManagedColorSpace { color_space, .. } => Ok(color_space),
        ref status => Err(
            crate::app_ui::preview_gpu_output_blocker::PreviewGpuOutputBlocker::UnsupportedFeature {
                feature: "icc_preview_color_space_resolution".to_owned(),
                reason: format!(
                    "display output contract did not resolve ICC profile to a managed color space: {status}"
                ),
            },
        ),
    }
}

struct PreviewCompositeOutput {
    rgba: Vec<u8>,
    composite_diagnostics: TimelineCompositeDiagnostics,
    color_diagnostics: RenderColorTransformDiagnostics,
    color_stage_diagnostics: RenderColorStageDiagnostics,
    render_stage_durations: AppUiPreviewRenderStageDurations,
}

struct PreviewWorkingCompositeOutput {
    frame: CpuColorFrame,
    boundary: RenderOutputColorBoundary,
    composite_diagnostics: TimelineCompositeDiagnostics,
    input_color_diagnostics: Vec<RenderColorTransformDiagnostics>,
    input_color_stage_diagnostics: RenderColorStageDiagnostics,
    render_stage_durations: AppUiPreviewRenderStageDurations,
}

enum PreviewWorkingElement {
    SolidColor(TimelineSolidColorLayer),
    Adjustment(TimelineAdjustmentLayer),
    Media {
        frame_index: usize,
        opacity: f32,
        blend_mode: BlendMode,
        transform: [f32; 6],
        effect_graph: Arc<CompiledEffectGraph>,
        frame_seed: i64,
    },
}

fn composite_resolved_preview_working(
    width: u32,
    height: u32,
    resolved: &[ResolvedPreviewElement],
    color_context: &ColorContext,
    scratch: &mut TimelineCompositeScratch,
) -> Result<PreviewWorkingCompositeOutput, String> {
    let working_prepare_started_at = Instant::now();
    let mut working_frames = Vec::new();
    let mut working_elements = Vec::with_capacity(resolved.len());
    let mut input_color_diagnostics = Vec::new();
    let mut input_color_stage_diagnostics = RenderColorStageDiagnostics::default();

    for element in resolved {
        match element {
            ResolvedPreviewElement::SolidColor(layer) => {
                working_elements.push(PreviewWorkingElement::SolidColor(layer.clone()));
            }
            ResolvedPreviewElement::Adjustment(layer) => {
                working_elements.push(PreviewWorkingElement::Adjustment(layer.clone()));
            }
            ResolvedPreviewElement::Media {
                frame,
                opacity,
                blend_mode,
                transform,
                effect_graph,
                frame_seed,
            } => {
                let working = frame.working_frame()?;
                if let Some(diagnostics) = working.color_diagnostics {
                    input_color_diagnostics.push(diagnostics);
                }
                input_color_stage_diagnostics.accumulate(working.stage_diagnostics);
                let frame_index = working_frames.len();
                working_frames.push(working.frame);
                working_elements.push(PreviewWorkingElement::Media {
                    frame_index,
                    opacity: *opacity,
                    blend_mode: *blend_mode,
                    transform: *transform,
                    effect_graph: Arc::clone(effect_graph),
                    frame_seed: *frame_seed,
                });
            }
        }
    }

    let elements: Vec<_> = working_elements
        .iter()
        .map(|element| match element {
            PreviewWorkingElement::SolidColor(layer) => {
                TimelineCompositeElement::SolidColor(layer.clone())
            }
            PreviewWorkingElement::Adjustment(layer) => {
                TimelineCompositeElement::Adjustment(layer.clone())
            }
            PreviewWorkingElement::Media {
                frame_index,
                opacity,
                blend_mode,
                transform,
                effect_graph,
                frame_seed,
            } => TimelineCompositeElement::Media(TimelineMediaLayer {
                frame: &working_frames[*frame_index],
                opacity: *opacity,
                blend_mode: *blend_mode,
                transform: *transform,
                effect_graph: Arc::clone(effect_graph),
                frame_seed: *frame_seed,
            }),
        })
        .collect();
    let working_prepare_us = app_duration_us(working_prepare_started_at.elapsed());
    let cpu_composite_started_at = Instant::now();
    let composite = composite_timeline_elements_color_frame_with_diagnostics(
        width,
        height,
        &elements,
        TimelineCompositeOptions::default(),
        color_context.working_color_space,
        scratch,
    );
    let cpu_composite_us = app_duration_us(cpu_composite_started_at.elapsed());
    let boundary = output_boundary_from_color_context(color_context);
    Ok(PreviewWorkingCompositeOutput {
        frame: composite.frame,
        boundary,
        composite_diagnostics: composite.diagnostics,
        input_color_diagnostics,
        input_color_stage_diagnostics,
        render_stage_durations: AppUiPreviewRenderStageDurations {
            working_prepare_us,
            cpu_composite_us,
            ..AppUiPreviewRenderStageDurations::default()
        },
    })
}

fn output_boundary_from_color_context(color_context: &ColorContext) -> RenderOutputColorBoundary {
    match (&color_context.ocio_display, &color_context.ocio_view) {
        (Some(display), Some(view)) => RenderOutputColorBoundary::display_view(
            color_context.output_color_space,
            display.clone(),
            view.clone(),
            color_context.tone_map,
            color_context.engine.clone(),
        ),
        _ => RenderOutputColorBoundary::display(
            color_context.output_color_space,
            color_context.tone_map,
            color_context.engine.clone(),
        ),
    }
}

fn gpu_composite_layers_for_resolved(
    _width: u32,
    _height: u32,
    resolved: &[ResolvedPreviewElement],
    working_color_space: ColorSpace,
) -> Result<Vec<AppUiGpuPreviewCompositeLayer>, GpuCompositingBlockerReason> {
    if resolved.len() > 5 {
        return Err(GpuCompositingBlockerReason::TooManyLayers);
    }
    let mut layers = Vec::with_capacity(resolved.len());
    for element in resolved {
        match element {
            ResolvedPreviewElement::Media {
                frame,
                opacity,
                blend_mode,
                transform,
                effect_graph,
                ..
            } => {
                if !effect_graph.graph.is_identity() {
                    return Err(GpuCompositingBlockerReason::EffectRequiresCpu);
                }
                if *blend_mode != BlendMode::Normal {
                    return Err(GpuCompositingBlockerReason::UnsupportedBlendMode);
                }
                let layer_working_color_space = frame
                    .frame
                    .as_ref()
                    .map(|working| working.descriptor().color_space)
                    .or_else(|| {
                        frame
                            .gpu_source
                            .as_ref()
                            .map(|source| source.input_transform.working_color_space)
                    })
                    .ok_or(GpuCompositingBlockerReason::GpuUnavailable)?;
                if layer_working_color_space != working_color_space {
                    return Err(GpuCompositingBlockerReason::UnsupportedTransform);
                }
                if !is_preview_gpu_media_transform_supported(*transform) {
                    return Err(GpuCompositingBlockerReason::UnsupportedTransform);
                }
                layers.push(AppUiGpuPreviewCompositeLayer::Media {
                    frame: frame.frame.clone(),
                    gpu_source: frame.gpu_source(),
                    opacity: *opacity,
                    transform: *transform,
                });
            }
            ResolvedPreviewElement::SolidColor(layer) => {
                if !layer.effect_graph.graph.is_identity() {
                    return Err(GpuCompositingBlockerReason::EffectRequiresCpu);
                }
                if layer.blend_mode != BlendMode::Normal {
                    return Err(GpuCompositingBlockerReason::UnsupportedBlendMode);
                }
                if !is_preview_identity_transform(layer.transform) {
                    return Err(GpuCompositingBlockerReason::UnsupportedTransform);
                }
                layers.push(AppUiGpuPreviewCompositeLayer::SolidColor { layer: layer.clone() });
            }
            ResolvedPreviewElement::Adjustment(_) => {
                return Err(GpuCompositingBlockerReason::EffectRequiresCpu);
            }
        }
    }
    Ok(layers)
}

fn preview_elements_require_deferred_composite(resolved: &[ResolvedPreviewElement]) -> bool {
    resolved
        .iter()
        .any(|element| matches!(element, ResolvedPreviewElement::Media { .. }))
}

fn is_preview_identity_transform(transform: [f32; 6]) -> bool {
    const EPSILON: f32 = 1.0e-6;
    (transform[0] - 1.0).abs() <= EPSILON
        && transform[1].abs() <= EPSILON
        && transform[2].abs() <= EPSILON
        && transform[3].abs() <= EPSILON
        && (transform[4] - 1.0).abs() <= EPSILON
        && transform[5].abs() <= EPSILON
}

fn is_preview_gpu_media_transform_supported(transform: [f32; 6]) -> bool {
    let det = transform[0] * transform[4] - transform[3] * transform[1];
    det.abs() > 1.0e-8
}

fn composite_resolved_preview(
    service: &AppUiPreviewService,
    width: u32,
    height: u32,
    resolved: &[ResolvedPreviewElement],
    color_context: &ColorContext,
    scratch: &mut TimelineCompositeScratch,
) -> Result<PreviewCompositeOutput, String> {
    let composite =
        composite_resolved_preview_working(width, height, resolved, color_context, scratch)?;
    for diagnostics in composite.input_color_diagnostics {
        service.record_color_transform(diagnostics);
    }
    if composite.input_color_stage_diagnostics != RenderColorStageDiagnostics::default() {
        service.record_color_stage(composite.input_color_stage_diagnostics);
    }
    if composite.composite_diagnostics.legacy_rgba8_composites > 0 {
        use crate::app_ui::preview_gpu_output_blocker::PreviewGpuOutputBlocker;
        service.record_preview_gpu_output_blocker(
            &PreviewGpuOutputBlocker::LegacyRgba8CompositeBoundary {
                legacy_composites: composite.composite_diagnostics.legacy_rgba8_composites,
            },
        );
    }
    let output_boundary_started_at = Instant::now();
    let mut render_stage_durations = composite.render_stage_durations;
    execute_cpu_output_boundary_rgba8(&composite.frame, &composite.boundary)
        .map(|output| {
            render_stage_durations.cpu_output_boundary_us =
                app_duration_us(output_boundary_started_at.elapsed());
            PreviewCompositeOutput {
                rgba: output.rgba,
                composite_diagnostics: composite.composite_diagnostics,
                color_diagnostics: output.color_diagnostics,
                color_stage_diagnostics: output.stage_diagnostics,
                render_stage_durations,
            }
        })
        .map_err(|err| format!("viewer preview final color transform failed: {err}"))
}

fn viewer_raster_frame_key(cache_key: &ViewerPreviewCacheKey) -> String {
    format!(
        "app-ui.viewer.raster:{}:{}x{}:{:016x}",
        cache_key.sequence_id, cache_key.width, cache_key.height, cache_key.plan_signature
    )
}

fn uncached_viewer_raster_frame_key(
    sequence_id: SequenceId,
    frame: i64,
    width: u32,
    height: u32,
) -> String {
    format!(
        "app-ui.viewer.raster-uncached:{sequence_id}:{width}x{height}:f{}",
        frame.max(0)
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PreviewMediaDecodePath {
    path: PathBuf,
    resolution: PreviewMediaDecodePathResolution,
    fingerprint: PreviewFileFingerprint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum PreviewMediaDecodePathResolution {
    Source,
    Proxy,
    ProxyMissing,
    ProxyStale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct PreviewProxyGenerationRequestKey {
    asset_id: AssetId,
    source_fingerprint: PreviewFileFingerprint,
    resolution: PreviewMediaDecodePathResolution,
}

fn should_request_preview_proxy_generation(
    request_missing_proxy_generation: bool,
    project_proxy_enabled: bool,
    asset_proxy_mode: bool,
    resolution: PreviewMediaDecodePathResolution,
) -> bool {
    request_missing_proxy_generation
        && project_proxy_enabled
        && asset_proxy_mode
        && matches!(
            resolution,
            PreviewMediaDecodePathResolution::ProxyMissing
                | PreviewMediaDecodePathResolution::ProxyStale
        )
}

fn resolve_preview_media_decode_path(
    prefer_proxy: bool,
    source_path: &Path,
    proxy_config: &mondrian_media::ProxyConfig,
) -> Option<PreviewMediaDecodePath> {
    let source_metadata = media_path_metadata(source_path)?;
    if !prefer_proxy {
        return Some(PreviewMediaDecodePath {
            path: source_path.to_path_buf(),
            resolution: PreviewMediaDecodePathResolution::Source,
            fingerprint: source_metadata.fingerprint,
        });
    }
    let proxy_generator = mondrian_media::ProxyGenerator::new(proxy_config.clone());
    let proxy_path = proxy_generator.proxy_path(source_path);
    match media_path_metadata(&proxy_path) {
        Some(proxy_metadata) if proxy_metadata.modified >= source_metadata.modified => {
            Some(PreviewMediaDecodePath {
                path: proxy_path,
                resolution: PreviewMediaDecodePathResolution::Proxy,
                fingerprint: proxy_metadata.fingerprint,
            })
        }
        None => Some(PreviewMediaDecodePath {
            path: source_path.to_path_buf(),
            resolution: PreviewMediaDecodePathResolution::ProxyMissing,
            fingerprint: source_metadata.fingerprint,
        }),
        Some(_) => Some(PreviewMediaDecodePath {
            path: source_path.to_path_buf(),
            resolution: PreviewMediaDecodePathResolution::ProxyStale,
            fingerprint: source_metadata.fingerprint,
        }),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MediaPathMetadata {
    fingerprint: PreviewFileFingerprint,
    modified: SystemTime,
}

fn media_path_metadata(path: &Path) -> Option<MediaPathMetadata> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?;
    Some(MediaPathMetadata {
        fingerprint: PreviewFileFingerprint::from_metadata(&metadata),
        modified,
    })
}

const PREVIEW_SCRUB_HOT_REQUEST_WINDOW_US: u64 = 250_000;
const PREVIEW_SCRUB_HOT_SOURCE_WINDOW_US: i64 = 750_000;
const PREVIEW_SCRUB_SLOW_LATENCY_US: u64 = 40_000;
const PREVIEW_SCRUB_RECOVERY_LATENCY_US: u64 = 25_000;
const PREVIEW_SCRUB_SLOW_SCORE_MAX: u8 = 3;

#[derive(Debug, Clone, Copy)]
struct PreviewScrubRequestObservation {
    asset_id: AssetId,
    source_micros: i64,
    observed_at: Instant,
}

#[derive(Debug, Default)]
struct PreviewScrubAdaptationState {
    last_request: Option<PreviewScrubRequestObservation>,
    hot_request_streak: u8,
    slow_latency_score: u8,
}

impl PreviewScrubAdaptationState {
    fn observe_request(&mut self, key: &MediaPreviewKey) -> PreviewDecodeAdaptiveHints {
        let now = Instant::now();
        let is_hot_region = self
            .last_request
            .map(|last| {
                last.asset_id == key.asset_id
                    && app_duration_us(now.saturating_duration_since(last.observed_at))
                        <= PREVIEW_SCRUB_HOT_REQUEST_WINDOW_US
                    && key.source_micros.saturating_sub(last.source_micros).abs()
                        <= PREVIEW_SCRUB_HOT_SOURCE_WINDOW_US
            })
            .unwrap_or(false);
        self.hot_request_streak = if is_hot_region {
            self.hot_request_streak.saturating_add(1)
        } else {
            0
        };
        self.last_request = Some(PreviewScrubRequestObservation {
            asset_id: key.asset_id,
            source_micros: key.source_micros,
            observed_at: now,
        });

        let scrub_class = if self.slow_latency_score >= 2 {
            PreviewScrubAdaptiveClass::SlowLatency
        } else if self.slow_latency_score == 1 {
            PreviewScrubAdaptiveClass::Recovery
        } else if self.hot_request_streak >= 2 {
            PreviewScrubAdaptiveClass::HotRegion
        } else {
            PreviewScrubAdaptiveClass::Normal
        };
        PreviewDecodeAdaptiveHints { scrub_class }
    }

    fn observe_decode(&mut self, diagnostics: PreviewDecodeDiagnostics) {
        if diagnostics.access_mode != PreviewDecodeAccessMode::ScrubCursor {
            return;
        }
        if diagnostics.elapsed_us >= PREVIEW_SCRUB_SLOW_LATENCY_US {
            self.slow_latency_score =
                self.slow_latency_score.saturating_add(1).min(PREVIEW_SCRUB_SLOW_SCORE_MAX);
        } else if diagnostics.elapsed_us <= PREVIEW_SCRUB_RECOVERY_LATENCY_US {
            self.slow_latency_score = self.slow_latency_score.saturating_sub(1);
        }
    }
}

fn source_micros(source_secs: f64) -> i64 {
    (source_secs.max(0.0) * 1_000_000.0).round() as i64
}

fn media_preview_job_deadline_at(
    priority: MediaPreviewRequestPriority,
    access_mode: PreviewDecodeAccessMode,
    enqueued_at: Instant,
    playback_current_deadline_budget_us: Option<u64>,
) -> Option<Instant> {
    if priority == MediaPreviewRequestPriority::Current
        && access_mode == PreviewDecodeAccessMode::PlaybackCursor
    {
        let budget_us = playback_current_deadline_budget_us?;
        return Some(enqueued_at + Duration::from_micros(budget_us));
    }
    None
}

fn media_preview_playback_current_deadline_budget_us(frame_rate: Rational) -> Option<u64> {
    let fps = frame_rate.to_f64();
    if !fps.is_finite() || fps <= 0.0 {
        return None;
    }
    let frame_duration_us = (1_000_000.0 / fps).round();
    if !frame_duration_us.is_finite() || frame_duration_us <= 0.0 {
        return None;
    }
    Some((frame_duration_us as u64).clamp(
        MEDIA_PREVIEW_PLAYBACK_CURRENT_MIN_DEADLINE_US,
        MEDIA_PREVIEW_PLAYBACK_CURRENT_MAX_DEADLINE_US,
    ))
}

fn media_preview_forward_prefetch_window_frames(frame_rate: Rational) -> Option<usize> {
    let fps = frame_rate.to_f64();
    if !fps.is_finite() || fps <= 0.0 {
        return None;
    }
    let frames = ((MEDIA_PREVIEW_FORWARD_PREFETCH_HORIZON_US as f64 / 1_000_000.0) * fps).round();
    if !frames.is_finite() || frames <= 0.0 {
        return None;
    }
    Some((frames as usize).clamp(
        MEDIA_PREVIEW_FORWARD_PREFETCH_MIN_FRAMES,
        MEDIA_PREVIEW_FORWARD_PREFETCH_MAX_FRAMES,
    ))
}

fn media_preview_deadline_expired(deadline_at: Option<Instant>) -> bool {
    deadline_at.is_some_and(|deadline| Instant::now() >= deadline)
}

fn media_preview_worker(
    lane: MediaPreviewWorkerLane,
    jobs: MediaPreviewJobQueueReceiver,
    results: mpsc::Sender<MediaPreviewResult>,
    scheduler: MediaPreviewScheduler,
    worker_activity: Arc<PreviewWorkerActivity>,
    shutdown: Arc<AtomicBool>,
) {
    while let Some(outcome) = jobs.recv_for_worker_outcome(lane) {
        let job = match outcome {
            MediaPreviewJobQueueReceive::Job(job) => job,
            MediaPreviewJobQueueReceive::DroppedExpiredPlaybackCurrent(job) => {
                if shutdown.load(Ordering::Acquire) {
                    break;
                }
                let queue_wait_us = app_duration_us(job.enqueued_at.elapsed());
                if !scheduler.should_decode(&job.key, job.access_mode) {
                    continue;
                }
                let result = media_preview_canceled_result(
                    job,
                    queue_wait_us,
                    MediaPreviewCancelReason::PlaybackDeadline,
                    0,
                    Some(0),
                );
                if results.send(result).is_err() {
                    break;
                }
                continue;
            }
        };
        if shutdown.load(Ordering::Acquire) {
            break;
        }
        let queue_wait_us = app_duration_us(job.enqueued_at.elapsed());
        if !scheduler.should_decode(&job.key, job.access_mode) {
            continue;
        }
        if media_preview_deadline_expired(job.deadline_at) {
            let result = media_preview_canceled_result(
                job,
                queue_wait_us,
                MediaPreviewCancelReason::PlaybackDeadline,
                0,
                Some(0),
            );
            if results.send(result).is_err() {
                break;
            }
            continue;
        }
        let _activity_lease = worker_activity.begin(lane, job.priority, job.access_mode);
        let cancel_key = job.key.clone();
        let cancel_generation = job.generation;
        let cancel_priority = job.priority;
        let cancel_access_mode = job.access_mode;
        let cancel_deadline_at = job.deadline_at;
        let cancel_scheduler = scheduler.clone();
        let decode_started_at = Instant::now();
        let observed_cancel_reason = Cell::new(None);
        let observed_cancel_elapsed_us = Cell::new(None);
        let mut result = decode_media_preview(job, queue_wait_us, || {
            let reason = media_preview_cancel_reason(
                shutdown.load(Ordering::Acquire),
                cancel_scheduler.is_decode_current(
                    &cancel_key,
                    cancel_generation,
                    cancel_access_mode,
                ),
                cancel_scheduler.has_pending_current_request_other_than(&cancel_key),
                cancel_scheduler.has_pending_realtime_current_request_other_than(&cancel_key),
                cancel_priority,
                cancel_access_mode,
                decode_started_at.elapsed(),
                media_preview_deadline_expired(cancel_deadline_at),
            );
            if reason.is_some() {
                if observed_cancel_elapsed_us.get().is_none() {
                    observed_cancel_elapsed_us
                        .set(Some(app_duration_us(decode_started_at.elapsed())));
                }
                observed_cancel_reason.set(reason);
            }
            reason.is_some()
        });
        if result.canceled && result.cancel_reason.is_none() {
            result.cancel_reason =
                Some(observed_cancel_reason.get().unwrap_or(MediaPreviewCancelReason::Unknown));
        }
        if result.canceled && result.cancel_observed_elapsed_us.is_none() {
            result.cancel_observed_elapsed_us = observed_cancel_elapsed_us.get();
        }
        if results.send(result).is_err() {
            break;
        }
    }
    mondrian_media::clear_thread_local_preview_decode_session();
}

fn media_preview_cancel_reason(
    shutdown: bool,
    scheduler_current: bool,
    other_current_pending: bool,
    other_realtime_current_pending: bool,
    priority: MediaPreviewRequestPriority,
    access_mode: PreviewDecodeAccessMode,
    elapsed: Duration,
    playback_deadline_expired: bool,
) -> Option<MediaPreviewCancelReason> {
    if shutdown {
        return Some(MediaPreviewCancelReason::Shutdown);
    }
    if !scheduler_current {
        return Some(MediaPreviewCancelReason::Obsolete);
    }
    if priority == MediaPreviewRequestPriority::Prefetch
        && access_mode == PreviewDecodeAccessMode::PlaybackCursor
        && other_current_pending
    {
        return Some(MediaPreviewCancelReason::PrefetchPreemptedByCurrent);
    }
    if priority == MediaPreviewRequestPriority::Current
        && access_mode == PreviewDecodeAccessMode::RandomAccessStillFrame
        && other_realtime_current_pending
    {
        return Some(MediaPreviewCancelReason::StillPreemptedByRealtimeCurrent);
    }
    if priority == MediaPreviewRequestPriority::Current
        && access_mode == PreviewDecodeAccessMode::PlaybackCursor
        && playback_deadline_expired
    {
        return Some(MediaPreviewCancelReason::PlaybackDeadline);
    }
    if priority == MediaPreviewRequestPriority::Prefetch
        && access_mode == PreviewDecodeAccessMode::PlaybackCursor
        && app_duration_us(elapsed) >= MEDIA_PREVIEW_PREFETCH_DECODE_BUDGET_US
    {
        return Some(MediaPreviewCancelReason::PrefetchDeadline);
    }
    None
}

fn media_preview_canceled_result(
    job: MediaPreviewJob,
    queue_wait_us: u64,
    reason: MediaPreviewCancelReason,
    decode_elapsed_us: u64,
    cancel_observed_elapsed_us: Option<u64>,
) -> MediaPreviewResult {
    MediaPreviewResult {
        key: job.key,
        frame: None,
        error: None,
        failure_reason: None,
        generation: job.generation,
        priority: job.priority,
        access_mode: job.access_mode,
        queue_wait_us,
        decode_elapsed_us,
        cancel_observed_elapsed_us,
        canceled: true,
        cancel_reason: Some(reason),
        decode_diagnostics: None,
        color_diagnostics: None,
        color_stage_diagnostics: None,
    }
}

fn decode_media_preview(
    job: MediaPreviewJob,
    queue_wait_us: u64,
    should_cancel: impl Fn() -> bool,
) -> MediaPreviewResult {
    let decode_started_at = Instant::now();
    let signature = media_preview_frame_signature(&job.key);
    let priority = job.priority;
    let access_mode = job.access_mode;
    let decode_outcome = decode_media_preview_for_access_mode(
        job.key.path.as_path(),
        job.source_secs,
        Some(job.key.target_width.max(1)),
        Some(job.key.target_height.max(1)),
        access_mode,
        job.key.fingerprint,
        job.adaptive_hints,
        should_cancel,
    );
    let decode_elapsed_us = app_duration_us(decode_started_at.elapsed());
    match decode_outcome {
        Ok(PreviewDecodeOutcome::Frame(frame)) => {
            let decode_diagnostics = frame.diagnostics;
            let width = frame.width;
            let height = frame.height;
            let source = CpuEncodedColorFrame::source_rgba8_shared(
                width,
                height,
                job.key.input_color_space,
                frame.into_shared_data(),
            );
            let input_transform = RenderInputTransform::to_working(
                job.key.working_color_space,
                job.key.tone_map,
                job.key.engine.clone(),
            );
            let gpu_source = MediaPreviewGpuSourceFrame::from_decode_diagnostics(
                source,
                input_transform,
                decode_diagnostics,
            );
            MediaPreviewResult {
                key: job.key,
                frame: Some(MediaPreviewFrame {
                    width,
                    height,
                    frame: None,
                    gpu_source: Some(gpu_source),
                    signature,
                }),
                error: None,
                failure_reason: None,
                generation: job.generation,
                priority,
                access_mode,
                queue_wait_us,
                decode_elapsed_us,
                cancel_observed_elapsed_us: None,
                canceled: false,
                cancel_reason: None,
                decode_diagnostics: Some(decode_diagnostics),
                color_diagnostics: None,
                color_stage_diagnostics: None,
            }
        }
        Ok(PreviewDecodeOutcome::Canceled) => MediaPreviewResult {
            key: job.key,
            frame: None,
            error: None,
            failure_reason: None,
            generation: job.generation,
            priority,
            access_mode,
            queue_wait_us,
            decode_elapsed_us,
            cancel_observed_elapsed_us: None,
            canceled: true,
            cancel_reason: None,
            decode_diagnostics: None,
            color_diagnostics: None,
            color_stage_diagnostics: None,
        },
        Err(err) => {
            let failure_reason = media_preview_failure_reason(&err);
            MediaPreviewResult {
                key: job.key,
                frame: None,
                error: Some(err.to_string()),
                failure_reason: Some(failure_reason),
                generation: job.generation,
                priority,
                access_mode,
                queue_wait_us,
                decode_elapsed_us,
                cancel_observed_elapsed_us: None,
                canceled: false,
                cancel_reason: None,
                decode_diagnostics: None,
                color_diagnostics: None,
                color_stage_diagnostics: None,
            }
        }
    }
}

fn media_preview_failure_reason(err: &MondrianError) -> MediaPreviewFailureReason {
    match err {
        MondrianError::DecodeTimeout { .. } => MediaPreviewFailureReason::Timeout,
        MondrianError::DecodeBudgetExhausted { .. } => {
            MediaPreviewFailureReason::ForwardDecodeBudgetExhausted
        }
        _ => MediaPreviewFailureReason::DecodeError,
    }
}

fn preview_hardware_decode_request_for_access_mode(
    access_mode: PreviewDecodeAccessMode,
) -> PreviewHardwareDecodeRequest {
    match access_mode {
        PreviewDecodeAccessMode::PlaybackCursor => PreviewHardwareDecodeRequest::PreferGpuResident,
        PreviewDecodeAccessMode::ScrubCursor | PreviewDecodeAccessMode::RandomAccessStillFrame => {
            PreviewHardwareDecodeRequest::Auto
        }
    }
}

fn decode_media_preview_for_access_mode(
    path: &Path,
    source_secs: f64,
    max_width: Option<u32>,
    max_height: Option<u32>,
    access_mode: PreviewDecodeAccessMode,
    fingerprint: Option<PreviewFileFingerprint>,
    adaptive_hints: PreviewDecodeAdaptiveHints,
    should_cancel: impl Fn() -> bool,
) -> mondrian_core::Result<PreviewDecodeOutcome> {
    let mut request = PreviewDecodeRgbaRequest::new(path, source_secs, access_mode)
        .with_max_size(max_width, max_height)
        .with_adaptive_hints(adaptive_hints)
        .with_hardware_decode_request(preview_hardware_decode_request_for_access_mode(access_mode));
    if let Some(fingerprint) = fingerprint {
        request = request.with_fingerprint(fingerprint);
    }
    decode_preview_rgba_scaled_cancellable(request, should_cancel)
}

#[cfg(test)]
mod tests {
    use super::*;

    use mondrian_assets::AssetLibrary;
    use mondrian_core::types::{AssetId, Rational, TimeCode};
    use mondrian_core::{ensure_mondrian_default_ocio_loaded, Color, ProjectColorManagement};
    use mondrian_effects::{get_or_compile_scheduled_effect_graph, EffectRenderPlan};
    use mondrian_media::info::{PixelFormat, VideoCodec};
    use mondrian_media::{
        DetectedColorInterpretation, MediaInfo, VideoColorDetectionMethod,
        VideoColorInterpretationConfidence, VideoColorSpaceSource, VideoStreamInfo,
    };
    use mondrian_renderer::{ColorFrameDomain, RenderColorStageGpuBlockerBreakdown};
    use mondrian_timeline::clip::Clip;
    use mondrian_timeline::sequence::{MissingColorMetadataPolicy, Sequence};
    use mondrian_timeline::track::Track;

    fn ensure_test_ocio_loaded() {
        ensure_mondrian_default_ocio_loaded().expect("preview tests require Mondrian default OCIO");
    }

    fn state_with_solid_color_clip(color: Color) -> AppState {
        ensure_test_ocio_loaded();
        let mut state = AppState::new();
        let mut sequence = Sequence::new("preview");
        let tb = sequence.time_base();
        sequence.video_tracks[0]
            .add_clip(Clip::new_solid_color(
                AssetId::new(),
                color,
                TimeCode::new(0, tb),
                TimeCode::new(24, tb),
            ))
            .expect("solid clip should be insertable");
        state.sequence = Some(sequence);
        state.seek(4);
        state
    }

    fn unique_preview_test_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "{name}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time")
                .as_nanos()
        ))
    }

    fn rec709_video_media_info(path: PathBuf, file_size: u64) -> MediaInfo {
        MediaInfo {
            path,
            duration: Duration::from_secs(2),
            file_size,
            container: "mp4".to_owned(),
            video_streams: vec![VideoStreamInfo {
                index: 0,
                codec: VideoCodec::H265,
                width: 3840,
                height: 2160,
                frame_rate: Rational::new(25, 1),
                pixel_format: PixelFormat::Yuv420p10le,
                detected_color_space: Some(ColorSpace::Rec709),
                color_interpretation: DetectedColorInterpretation {
                    color_space: Some(ColorSpace::Rec709),
                    confidence: VideoColorInterpretationConfidence::High,
                    source: VideoColorSpaceSource::Metadata,
                    method: VideoColorDetectionMethod::CicpTags,
                    evidence: Vec::new(),
                    warnings: Vec::new(),
                    user_overridable: true,
                },
                color_space_source: VideoColorSpaceSource::Metadata,
                color_detection_method: VideoColorDetectionMethod::CicpTags,
                color_metadata: None,
                color_metadata_hints: Vec::new(),
                hdr_metadata: Vec::new(),
                bit_depth: 10,
                has_alpha: false,
                avg_bitrate: 20_000_000,
                total_frames: Some(50),
            }],
            audio_streams: Vec::new(),
            has_video: true,
            has_audio: false,
        }
    }

    fn state_with_invalid_video_asset() -> (AppState, AssetId, PathBuf) {
        ensure_test_ocio_loaded();
        let root = unique_preview_test_root("mondrian-preview-invalid-video");
        let media_path = root.join("source.mp4");
        std::fs::create_dir_all(&root).expect("test root");
        std::fs::write(&media_path, b"not a real video").expect("invalid media");
        let file_size = std::fs::metadata(&media_path).expect("media metadata").len();
        let library = AssetLibrary::open(root.join("library")).expect("asset library");
        let asset_id = library
            .upsert_media_file_with_info(
                &media_path,
                rec709_video_media_info(media_path.clone(), file_size),
            )
            .expect("insert video asset");

        let mut state = AppState::new();
        state.asset_library = Some(library);
        let mut sequence = Sequence::new("media");
        let tb = sequence.time_base();
        sequence.video_tracks[0]
            .add_clip(Clip::new(
                asset_id,
                TimeCode::new(0, tb),
                TimeCode::new(50, tb),
            ))
            .expect("media clip should be insertable");
        state.sequence = Some(sequence);
        state.seek(0);
        (state, asset_id, root)
    }

    fn state_with_two_invalid_video_assets() -> (AppState, PathBuf) {
        ensure_test_ocio_loaded();
        let root = unique_preview_test_root("mondrian-preview-two-invalid-videos");
        std::fs::create_dir_all(&root).expect("test root");
        let library = AssetLibrary::open(root.join("library")).expect("asset library");
        let mut asset_ids = Vec::new();
        for name in ["bottom.mp4", "top.mp4"] {
            let media_path = root.join(name);
            std::fs::write(&media_path, b"not a real video").expect("invalid media");
            let file_size = std::fs::metadata(&media_path).expect("media metadata").len();
            let asset_id = library
                .upsert_media_file_with_info(
                    &media_path,
                    rec709_video_media_info(media_path.clone(), file_size),
                )
                .expect("insert video asset");
            asset_ids.push(asset_id);
        }

        let mut state = AppState::new();
        state.asset_library = Some(library);
        let mut sequence = Sequence::new("multi-track media");
        let tb = sequence.time_base();
        for (track_index, asset_id) in asset_ids.into_iter().enumerate() {
            sequence.video_tracks[track_index]
                .add_clip(Clip::new(
                    asset_id,
                    TimeCode::new(0, tb),
                    TimeCode::new(50, tb),
                ))
                .expect("media clip should be insertable");
        }
        state.sequence = Some(sequence);
        state.seek(0);
        (state, root)
    }

    fn state_with_icc_display_policy(color: Color) -> AppState {
        let mut state = state_with_solid_color_clip(color);
        let sequence = state.sequence.as_mut().expect("test state has sequence");
        sequence.settings.color_management.inherit = false;
        sequence.settings.color_management.display_management =
            mondrian_core::DisplayManagementPolicy {
                monitor_profile: mondrian_core::MonitorProfileReference::IccProfile {
                    profile_id: "os-default".to_owned(),
                },
                viewer_mode: mondrian_core::ViewerDisplayMode::Sdr,
                tone_map_policy: mondrian_core::DisplayToneMapPolicy::Automatic,
                ..Default::default()
            };
        state
    }

    fn managed_icc_display_snapshot(color_space: ColorSpace) -> DisplayOutputSnapshot {
        let mut snapshot = mondrian_core::display_probe::FakeDisplayProbe::sdr_pass().snapshot;
        snapshot.monitor_profile_status = MonitorProfileStatus::ManagedColorSpace {
            color_space,
            source: mondrian_core::display_contract::MonitorProfileSource::OsIccProfile,
        };
        snapshot.resolved_output_color_space = format!("{color_space:?}");
        snapshot
    }

    fn test_color_context(output_color_space: ColorSpace) -> ColorContext {
        ensure_test_ocio_loaded();
        Sequence::new("color-context")
            .settings
            .root_preview_color_context(&ProjectColorManagement::default(), output_color_space)
    }

    #[test]
    fn preview_display_color_space_resolves_managed_monitor_profile() {
        let mut sequence = Sequence::new("p3-preview");
        sequence.settings.color_management.inherit = false;
        sequence.settings.color_management.display_management =
            mondrian_core::DisplayManagementPolicy {
                monitor_profile: mondrian_core::MonitorProfileReference::ColorSpace(
                    ColorSpace::DciP3,
                ),
                viewer_mode: mondrian_core::ViewerDisplayMode::Sdr,
                tone_map_policy: mondrian_core::DisplayToneMapPolicy::Automatic,
                ..Default::default()
            };

        assert_eq!(
            preview_display_color_space(&sequence, &ProjectColorManagement::default(), None)
                .expect("display color space"),
            ColorSpace::DciP3
        );
    }

    #[test]
    fn preview_display_color_space_resolves_explicit_hdr_viewer_mode() {
        let mut sequence = Sequence::new("hdr-preview");
        sequence.settings.color_management.inherit = false;
        sequence.settings.color_management.display_management =
            mondrian_core::DisplayManagementPolicy {
                monitor_profile: mondrian_core::MonitorProfileReference::ColorSpace(
                    ColorSpace::Rec709,
                ),
                viewer_mode: mondrian_core::ViewerDisplayMode::HdrPq,
                tone_map_policy: mondrian_core::DisplayToneMapPolicy::Automatic,
                ..Default::default()
            };

        assert_eq!(
            preview_display_color_space(&sequence, &ProjectColorManagement::default(), None)
                .expect("display color space"),
            ColorSpace::Rec2100Pq
        );
    }

    #[test]
    fn preview_display_color_space_rejects_icc_before_display_contract_resolution() {
        let mut sequence = Sequence::new("icc-preview");
        sequence.settings.color_management.inherit = false;
        sequence.settings.color_management.display_management =
            mondrian_core::DisplayManagementPolicy {
                monitor_profile: mondrian_core::MonitorProfileReference::IccProfile {
                    profile_id: "display-profile".to_owned(),
                },
                viewer_mode: mondrian_core::ViewerDisplayMode::Sdr,
                tone_map_policy: mondrian_core::DisplayToneMapPolicy::Automatic,
                ..Default::default()
            };

        let err = preview_display_color_space(&sequence, &ProjectColorManagement::default(), None)
            .expect_err("ICC profile requires display contract resolution and must fail closed");

        assert!(matches!(
            err,
            crate::app_ui::preview_gpu_output_blocker::PreviewGpuOutputBlocker::UnsupportedFeature {
                ref feature,
                ..
            } if feature == "icc_preview_color_space_resolution"
        ));
    }

    #[test]
    fn preview_display_color_space_resolves_icc_from_display_contract() {
        let mut sequence = Sequence::new("icc-preview");
        sequence.settings.color_management.inherit = false;
        sequence.settings.color_management.display_management =
            mondrian_core::DisplayManagementPolicy {
                monitor_profile: mondrian_core::MonitorProfileReference::IccProfile {
                    profile_id: "os-default".to_owned(),
                },
                viewer_mode: mondrian_core::ViewerDisplayMode::Sdr,
                tone_map_policy: mondrian_core::DisplayToneMapPolicy::Automatic,
                ..Default::default()
            };
        let snapshot = managed_icc_display_snapshot(ColorSpace::DciP3);

        assert_eq!(
            preview_display_color_space(
                &sequence,
                &ProjectColorManagement::default(),
                Some(&snapshot)
            )
            .expect("ICC display color space should resolve from display contract"),
            ColorSpace::DciP3
        );
    }

    #[test]
    fn gpu_preview_frame_for_icc_policy_uses_display_contract_color_space() {
        let service = AppUiPreviewService::new();
        let state = state_with_icc_display_policy(Color::from_rgba8(24, 80, 160, 255));
        let snapshot = managed_icc_display_snapshot(ColorSpace::DciP3);
        service.set_display_output_snapshot(Some(&snapshot));

        let frame = match service.gpu_preview_frame_for_state(&state) {
            AppUiGpuPreviewFrameState::Ready(frame) => frame,
            AppUiGpuPreviewFrameState::Current => panic!("expected new GPU preview candidate"),
            AppUiGpuPreviewFrameState::Loading => panic!("expected ready GPU preview candidate"),
            AppUiGpuPreviewFrameState::Unavailable => {
                panic!("ICC policy should resolve through display contract")
            }
        };

        assert_eq!(frame.boundary.output_color_space, ColorSpace::DciP3);
        assert_eq!(frame.working_color_space, ColorSpace::Rec709);
    }

    fn ready_frame(state: ViewerPreviewState) -> ViewerFrameImage {
        match state {
            ViewerPreviewState::Ready(ViewerFrameContent::Raster(frame)) => frame,
            ViewerPreviewState::Ready(other) => {
                panic!("expected raster ready frame, got {other:?}")
            }
            other => panic!("expected ready frame, got {other:?}"),
        }
    }

    #[test]
    fn solid_color_sequence_returns_preview_frame_at_preview_scale() {
        let service = AppUiPreviewService::new();
        let state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));

        let frame = service.viewer_preview_for_state(&state);
        let frame = ready_frame(frame);

        assert_eq!(frame.width, 960);
        assert_eq!(frame.height, 540);
        assert_eq!(frame.rgba.len(), 960 * 540 * 4);
        assert!(frame.key.starts_with("app-ui.viewer.raster:"));
        assert!(frame.key.contains(":960x540:"));
    }

    #[test]
    fn gpu_preview_frame_for_state_returns_gpu_composite_candidate() {
        let service = AppUiPreviewService::new();
        let state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));

        let frame = match service.gpu_preview_frame_for_state(&state) {
            AppUiGpuPreviewFrameState::Ready(frame) => frame,
            AppUiGpuPreviewFrameState::Current => panic!("expected new GPU preview candidate"),
            AppUiGpuPreviewFrameState::Loading => panic!("expected ready GPU preview candidate"),
            AppUiGpuPreviewFrameState::Unavailable => {
                panic!("expected available GPU preview candidate")
            }
        };

        assert_eq!(frame.width, 960);
        assert_eq!(frame.height, 540);
        assert_eq!(frame.working_color_space, ColorSpace::Rec709);
        match &frame.working_input {
            AppUiGpuPreviewWorkingInput::GpuComposite { layers } => {
                assert_eq!(layers.len(), 1);
                assert!(matches!(
                    layers[0],
                    AppUiGpuPreviewCompositeLayer::SolidColor { .. }
                ));
            }
        }
        assert!(frame.external_texture_key().starts_with("app-ui.viewer.gpu:"));
        assert_eq!(frame.preview_candidate_id(), 1);

        let diagnostics = service.diagnostics();
        assert_eq!(diagnostics.gpu_preview_candidate_requests, 1);
        assert_eq!(diagnostics.gpu_preview_candidate_ready, 1);
        assert_eq!(diagnostics.gpu_preview_candidate_current, 0);
        assert_eq!(diagnostics.gpu_preview_candidate_loading, 0);
        assert_eq!(diagnostics.gpu_preview_candidate_unavailable, 0);
        assert_eq!(diagnostics.gpu_preview_candidate_pixels, 960_u64 * 540);
    }

    #[test]
    fn gpu_composite_layers_accept_transformed_media_frame() {
        let media = test_media_frame_with_size(180, 320, 180, 42);
        let transform = [3.0, 0.0, 12.0, 0.0, 3.0, 18.0];
        let effect_graph = mondrian_effects::get_or_compile_scheduled_effect_graph(
            &mondrian_effects::EffectRenderPlan::default(),
        )
        .expect("compile identity graph");
        let elements = vec![ResolvedPreviewElement::Media {
            frame: media,
            opacity: 0.85,
            blend_mode: BlendMode::Normal,
            transform,
            effect_graph,
            frame_seed: 7,
        }];

        let layers = gpu_composite_layers_for_resolved(960, 540, &elements, ColorSpace::Rec709)
            .expect("affine transformed media should stay on GPU composite path");

        assert_eq!(layers.len(), 1);
        match &layers[0] {
            AppUiGpuPreviewCompositeLayer::Media {
                opacity, transform: actual_transform, ..
            } => {
                assert_eq!(*opacity, 0.85);
                assert_eq!(*actual_transform, transform);
            }
            AppUiGpuPreviewCompositeLayer::SolidColor { .. } => {
                panic!("expected media layer")
            }
        }
    }

    #[test]
    fn gpu_composite_layers_accept_source_only_media_frame() {
        let source = CpuEncodedColorFrame::source_rgba8(
            320,
            180,
            ColorSpace::Rec709,
            vec![0; 320 * 180 * 4],
        );
        let input_transform =
            RenderInputTransform::to_working(ColorSpace::Rec709, false, ColorEngine::MondrianSmart);
        let media = MediaPreviewFrame {
            width: 320,
            height: 180,
            frame: None,
            gpu_source: Some(MediaPreviewGpuSourceFrame::new(source, input_transform)),
            signature: 44,
        };
        let effect_graph = mondrian_effects::get_or_compile_scheduled_effect_graph(
            &mondrian_effects::EffectRenderPlan::default(),
        )
        .expect("compile identity graph");
        let elements = vec![ResolvedPreviewElement::Media {
            frame: media,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_graph,
            frame_seed: 7,
        }];

        let layers = gpu_composite_layers_for_resolved(960, 540, &elements, ColorSpace::Rec709)
            .expect("source-only media should stay on GPU input/composite path");

        match &layers[0] {
            AppUiGpuPreviewCompositeLayer::Media { frame, gpu_source, .. } => {
                assert!(frame.is_none());
                assert!(gpu_source.is_some());
            }
            AppUiGpuPreviewCompositeLayer::SolidColor { .. } => {
                panic!("expected media layer")
            }
        }
    }

    #[test]
    fn gpu_composite_layers_reject_singular_media_transform() {
        let media = test_media_frame_with_size(180, 320, 180, 43);
        let effect_graph = mondrian_effects::get_or_compile_scheduled_effect_graph(
            &mondrian_effects::EffectRenderPlan::default(),
        )
        .expect("compile identity graph");
        let elements = vec![ResolvedPreviewElement::Media {
            frame: media,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            effect_graph,
            frame_seed: 7,
        }];

        let err = match gpu_composite_layers_for_resolved(960, 540, &elements, ColorSpace::Rec709) {
            Ok(_) => panic!("singular transform cannot stay on GPU composite path"),
            Err(err) => err,
        };

        assert_eq!(err, GpuCompositingBlockerReason::UnsupportedTransform);
    }

    #[test]
    fn external_gpu_preview_frame_overrides_raster_preview_for_same_plan() {
        let service = AppUiPreviewService::new();
        let state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
        let frame = match service.gpu_preview_frame_for_state(&state) {
            AppUiGpuPreviewFrameState::Ready(frame) => frame,
            _ => panic!("expected ready GPU preview candidate"),
        };
        let key = frame.external_texture_key();
        let first_candidate_id = frame.preview_candidate_id();

        assert!(service.set_external_viewer_frame(&frame, key.clone()));
        match service.gpu_preview_frame_for_state(&state) {
            AppUiGpuPreviewFrameState::Current => {}
            _ => panic!("expected current external GPU preview frame"),
        }
        match service.viewer_preview_for_state(&state) {
            ViewerPreviewState::Ready(ViewerFrameContent::ExternalTexture(frame)) => {
                assert_eq!(frame.key, key);
                assert_eq!(frame.width, 960);
                assert_eq!(frame.height, 540);
            }
            other => panic!("expected external GPU preview frame, got {other:?}"),
        }
        let diagnostics = service.diagnostics();
        assert_eq!(frame.preview_candidate_id(), first_candidate_id);
        assert_eq!(diagnostics.gpu_preview_candidate_requests, 2);
        assert_eq!(diagnostics.gpu_preview_candidate_ready, 1);
        assert_eq!(diagnostics.gpu_preview_candidate_current, 1);
        assert_eq!(diagnostics.gpu_preview_external_frames_registered, 1);
        assert_eq!(diagnostics.gpu_preview_external_frames_rejected, 0);
        assert_eq!(diagnostics.gpu_preview_external_frames_cleared, 0);

        service.clear_external_viewer_frame();
        let second_frame = match service.gpu_preview_frame_for_state(&state) {
            AppUiGpuPreviewFrameState::Ready(frame) => frame,
            _ => panic!("expected new ready GPU preview candidate after external frame clear"),
        };
        assert!(second_frame.preview_candidate_id() > first_candidate_id);
    }

    #[test]
    fn preview_diagnostics_count_ready_render_requests() {
        let service = AppUiPreviewService::new();
        let state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));

        let diagnostics = service.diagnostics();
        assert_eq!(diagnostics.render_requests, 0);

        let frame = service.viewer_preview_for_state(&state);
        let _ = ready_frame(frame);

        let diagnostics = service.diagnostics();
        assert_eq!(diagnostics.render_requests, 1);
        assert_eq!(diagnostics.ready_frames, 1);
        assert_eq!(diagnostics.loading_frames, 0);
        assert_eq!(diagnostics.stale_frames, 0);
        assert_eq!(diagnostics.unavailable_frames, 0);
        assert_eq!(diagnostics.gpu_preview_candidate_requests, 0);
        assert_eq!(diagnostics.gpu_preview_candidate_ready, 0);
        assert_eq!(diagnostics.gpu_preview_candidate_current, 0);
        assert_eq!(diagnostics.gpu_preview_candidate_loading, 0);
        assert_eq!(diagnostics.gpu_preview_candidate_unavailable, 0);
        assert_eq!(diagnostics.gpu_preview_candidate_pixels, 0);
        assert_eq!(diagnostics.gpu_preview_external_frames_registered, 0);
        assert_eq!(diagnostics.gpu_preview_external_frames_rejected, 0);
        assert_eq!(diagnostics.gpu_preview_external_frames_cleared, 0);
        assert_eq!(diagnostics.input_color_resolution_override, 0);
        assert_eq!(diagnostics.input_color_resolution_detected_metadata, 0);
        assert_eq!(diagnostics.input_color_resolution_missing_assume_rec709, 0);
        assert_eq!(diagnostics.input_color_resolution_missing_assume_working, 0);
        assert_eq!(diagnostics.input_color_resolution_missing_rejected, 0);
        assert_eq!(diagnostics.input_color_resolution_data_texture, 0);
        assert_eq!(diagnostics.viewer_frame_cache_hits, 0);
        assert_eq!(diagnostics.viewer_frame_cache_misses, 1);
        assert_eq!(diagnostics.viewer_frame_cache_entries, 1);
        assert_eq!(diagnostics.media_cache_entries, 0);
        assert_eq!(diagnostics.media_failure_entries, 0);
        assert_eq!(diagnostics.color_input_transform_calls, 0);
        assert_eq!(diagnostics.color_input_transform_pixels, 0);
        assert_eq!(diagnostics.color_output_transform_calls, 1);
        assert_eq!(diagnostics.color_output_transform_pixels, 960_u64 * 540);
        assert_eq!(diagnostics.color_rgba8_boundary_calls, 1);
        assert_eq!(diagnostics.color_stage_plans, 1);
        assert_eq!(diagnostics.color_stage_total_stages, 1);
        assert_eq!(diagnostics.color_stage_cpu_input_stages, 0);
        assert_eq!(diagnostics.color_stage_cpu_output_stages, 1);
        assert_eq!(diagnostics.color_stage_gpu_color_stages, 0);
        assert_eq!(diagnostics.color_stage_upload_stages, 0);
        assert_eq!(diagnostics.color_stage_readback_stages, 0);
        assert_eq!(diagnostics.color_stage_gpu_blockers, 0);
        assert_eq!(diagnostics.color_stage_gpu_shader_module_blockers, 0);
        assert_eq!(diagnostics.color_stage_gpu_ocio_resource_blockers, 0);
        assert_eq!(diagnostics.color_stage_gpu_wrapper_blockers, 0);
        assert_eq!(diagnostics.color_stage_gpu_render_pipeline_blockers, 0);
        assert_eq!(diagnostics.color_stage_pixels, 960_u64 * 540);
        assert_eq!(diagnostics.color_composite_plans, 1);
        assert_eq!(diagnostics.color_composite_elements, 1);
        assert_eq!(diagnostics.color_composite_float_linear, 1);
        assert_eq!(diagnostics.color_composite_legacy_rgba8, 0);
    }

    #[test]
    fn preview_diagnostics_count_gpu_stage_blocker_breakdown() {
        let service = AppUiPreviewService::new();
        service.record_color_stage(RenderColorStageDiagnostics {
            total_stages: 1,
            gpu_color_stages: 1,
            gpu_blockers: 4,
            gpu_blocker_breakdown: RenderColorStageGpuBlockerBreakdown {
                shader_module_not_prepared: 1,
                ocio_resource_bind_group_not_prepared: 1,
                fullscreen_wrapper_not_prepared: 1,
                render_pipeline_not_prepared: 1,
                ..RenderColorStageGpuBlockerBreakdown::default()
            },
            ..RenderColorStageDiagnostics::default()
        });

        let diagnostics = service.diagnostics();

        assert_eq!(diagnostics.color_stage_gpu_blockers, 4);
        assert_eq!(diagnostics.color_stage_gpu_shader_module_blockers, 1);
        assert_eq!(diagnostics.color_stage_gpu_ocio_resource_blockers, 1);
        assert_eq!(diagnostics.color_stage_gpu_wrapper_blockers, 1);
        assert_eq!(diagnostics.color_stage_gpu_render_pipeline_blockers, 1);
    }

    #[test]
    fn preview_diagnostics_count_decode_paths_and_duration() {
        let service = AppUiPreviewService::new();

        service.record_preview_decode(
            PreviewDecodeDiagnostics {
                path: PreviewDecodePath::InProcessFfmpegCpuRgba,
                elapsed_us: 1_000,
                cache_hit: false,
                access_mode: PreviewDecodeAccessMode::ScrubCursor,
                external_process: false,
                cpu_resident: true,
                seek_performed: true,
                seek_strategy: PreviewDecodeSeekStrategy::BoundedAnyFrame,
                forward_reuse_frame_window: 1,
                forward_decode_budget_frames: 8,
                any_seek_window_ms: 120,
                scrub_adaptive_class: PreviewScrubAdaptiveClass::Normal,
                hardware_decode_request: PreviewHardwareDecodeRequest::Auto,
                hardware_decode_decision: PreviewHardwareDecodeDecision::CpuRgbaNotRequested,
                hardware_decode_candidate_backend: None,
                hardware_decode_candidate_handle_kind: None,
                hardware_decode_adapter_available: false,
                session_reused: false,
                forward_reused: false,
                seek_index_available: true,
                seek_index_keyframes: 3,
                seek_index_observed_packets: 90,
                seek_index_source: PreviewSeekIndexSource::ProbeBacked,
                seek_index_used: true,
                seek_index_anchor_pts: Some(120),
                decoded_frame_count: 48,
                threading_kind: PreviewDecodeThreadingKind::Frame,
                threading_count: 6,
                stage_durations: PreviewDecodeStageDurations {
                    session_open_us: 100,
                    cache_lookup_us: 2,
                    seek_us: 300,
                    packet_decode_us: 500,
                    swscale_us: 70,
                    rgba_copy_us: 30,
                    external_process_us: 0,
                },
                hw_accel_backend: HwAccelBackend::None,
                hardware_decode_active: false,
                zero_copy_active: false,
                decoded_frame_residency: DecodedFrameResidency::CpuRgba,
                gpu_frame_handle_kind: None,
                renderer_import_ready: false,
                hardware_decode_blocker: PreviewHardwareDecodeBlocker::TextureResidencyNotConnected,
                decoded_surface_format: DecodedVideoSurfaceFormat::P010,
            },
            MediaPreviewRequestPriority::Current,
            1_200,
        );
        service.record_preview_decode(
            PreviewDecodeDiagnostics {
                path: PreviewDecodePath::ExternalFfmpegCpuRgba,
                elapsed_us: 2_500,
                cache_hit: false,
                access_mode: PreviewDecodeAccessMode::PlaybackCursor,
                external_process: true,
                cpu_resident: true,
                seek_performed: false,
                seek_strategy: PreviewDecodeSeekStrategy::KeyframeBefore,
                forward_reuse_frame_window: 3,
                forward_decode_budget_frames: 48,
                any_seek_window_ms: 0,
                scrub_adaptive_class: PreviewScrubAdaptiveClass::Normal,
                hardware_decode_request: PreviewHardwareDecodeRequest::PreferGpuResident,
                hardware_decode_decision: PreviewHardwareDecodeDecision::CpuRgbaBackendBoundary,
                hardware_decode_candidate_backend: None,
                hardware_decode_candidate_handle_kind: None,
                hardware_decode_adapter_available: false,
                session_reused: true,
                forward_reused: false,
                seek_index_available: false,
                seek_index_keyframes: 0,
                seek_index_observed_packets: 0,
                seek_index_source: PreviewSeekIndexSource::None,
                seek_index_used: false,
                seek_index_anchor_pts: None,
                decoded_frame_count: 0,
                threading_kind: PreviewDecodeThreadingKind::None,
                threading_count: 0,
                stage_durations: PreviewDecodeStageDurations {
                    session_open_us: 0,
                    cache_lookup_us: 0,
                    seek_us: 0,
                    packet_decode_us: 0,
                    swscale_us: 0,
                    rgba_copy_us: 0,
                    external_process_us: 2_450,
                },
                hw_accel_backend: HwAccelBackend::None,
                hardware_decode_active: false,
                zero_copy_active: false,
                decoded_frame_residency: DecodedFrameResidency::CpuRgba,
                gpu_frame_handle_kind: None,
                renderer_import_ready: false,
                hardware_decode_blocker: PreviewHardwareDecodeBlocker::TextureResidencyNotConnected,
                decoded_surface_format: DecodedVideoSurfaceFormat::Unknown,
            },
            MediaPreviewRequestPriority::Current,
            400,
        );
        service.record_preview_decode(
            PreviewDecodeDiagnostics {
                path: PreviewDecodePath::PreviewCacheHit,
                elapsed_us: 25,
                cache_hit: true,
                access_mode: PreviewDecodeAccessMode::RandomAccessStillFrame,
                external_process: false,
                cpu_resident: true,
                seek_performed: false,
                seek_strategy: PreviewDecodeSeekStrategy::KeyframeBefore,
                forward_reuse_frame_window: 0,
                forward_decode_budget_frames: 48,
                any_seek_window_ms: 0,
                scrub_adaptive_class: PreviewScrubAdaptiveClass::Normal,
                hardware_decode_request: PreviewHardwareDecodeRequest::Auto,
                hardware_decode_decision: PreviewHardwareDecodeDecision::CpuRgbaNotRequested,
                hardware_decode_candidate_backend: None,
                hardware_decode_candidate_handle_kind: None,
                hardware_decode_adapter_available: false,
                session_reused: false,
                forward_reused: false,
                seek_index_available: true,
                seek_index_keyframes: 2,
                seek_index_observed_packets: 40,
                seek_index_source: PreviewSeekIndexSource::SessionObserved,
                seek_index_used: false,
                seek_index_anchor_pts: None,
                decoded_frame_count: 0,
                threading_kind: PreviewDecodeThreadingKind::None,
                threading_count: 0,
                stage_durations: PreviewDecodeStageDurations {
                    session_open_us: 0,
                    cache_lookup_us: 20,
                    seek_us: 0,
                    packet_decode_us: 0,
                    swscale_us: 0,
                    rgba_copy_us: 0,
                    external_process_us: 0,
                },
                hw_accel_backend: HwAccelBackend::None,
                hardware_decode_active: false,
                zero_copy_active: false,
                decoded_frame_residency: DecodedFrameResidency::CpuRgba,
                gpu_frame_handle_kind: None,
                renderer_import_ready: false,
                hardware_decode_blocker: PreviewHardwareDecodeBlocker::TextureResidencyNotConnected,
                decoded_surface_format: DecodedVideoSurfaceFormat::Nv12,
            },
            MediaPreviewRequestPriority::Current,
            20,
        );
        service.record_preview_decode(
            PreviewDecodeDiagnostics {
                path: PreviewDecodePath::PlaybackSessionRingHit,
                elapsed_us: 40,
                cache_hit: true,
                access_mode: PreviewDecodeAccessMode::PlaybackCursor,
                external_process: false,
                cpu_resident: true,
                seek_performed: false,
                seek_strategy: PreviewDecodeSeekStrategy::KeyframeBefore,
                forward_reuse_frame_window: 3,
                forward_decode_budget_frames: 48,
                any_seek_window_ms: 0,
                scrub_adaptive_class: PreviewScrubAdaptiveClass::Normal,
                hardware_decode_request: PreviewHardwareDecodeRequest::PreferGpuResident,
                hardware_decode_decision: PreviewHardwareDecodeDecision::CpuRgbaBackendUnavailable,
                hardware_decode_candidate_backend: Some(HwAccelBackend::D3D11VA),
                hardware_decode_candidate_handle_kind: Some(
                    DecodedGpuFrameHandleKind::D3D11Texture2D,
                ),
                hardware_decode_adapter_available: false,
                session_reused: true,
                forward_reused: false,
                seek_index_available: true,
                seek_index_keyframes: 4,
                seek_index_observed_packets: 128,
                seek_index_source: PreviewSeekIndexSource::ProbeBacked,
                seek_index_used: false,
                seek_index_anchor_pts: None,
                decoded_frame_count: 0,
                threading_kind: PreviewDecodeThreadingKind::None,
                threading_count: 0,
                stage_durations: PreviewDecodeStageDurations {
                    session_open_us: 0,
                    cache_lookup_us: 12,
                    seek_us: 0,
                    packet_decode_us: 0,
                    swscale_us: 0,
                    rgba_copy_us: 0,
                    external_process_us: 0,
                },
                hw_accel_backend: HwAccelBackend::None,
                hardware_decode_active: false,
                zero_copy_active: false,
                decoded_frame_residency: DecodedFrameResidency::CpuRgba,
                gpu_frame_handle_kind: None,
                renderer_import_ready: false,
                hardware_decode_blocker: PreviewHardwareDecodeBlocker::TextureResidencyNotConnected,
                decoded_surface_format: DecodedVideoSurfaceFormat::Nv12,
            },
            MediaPreviewRequestPriority::Current,
            0,
        );
        service.record_preview_decode_queue_wait(
            MediaPreviewRequestPriority::Prefetch,
            PreviewDecodeAccessMode::PlaybackCursor,
            400,
        );
        service.record_preview_decode_queue_wait(
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::ScrubCursor,
            1_200,
        );
        service.record_preview_decode_queue_wait(
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::RandomAccessStillFrame,
            20,
        );
        service.record_preview_decode_cancel(
            PreviewDecodeAccessMode::PlaybackCursor,
            Some(MediaPreviewCancelReason::PrefetchDeadline),
            700,
            Some(600),
        );
        service.record_preview_decode_cancel(
            PreviewDecodeAccessMode::ScrubCursor,
            Some(MediaPreviewCancelReason::Obsolete),
            1_400,
            Some(1_000),
        );
        service.record_preview_decode_cancel(
            PreviewDecodeAccessMode::RandomAccessStillFrame,
            Some(MediaPreviewCancelReason::Shutdown),
            20,
            Some(5),
        );

        let diagnostics = service.diagnostics();

        assert_eq!(diagnostics.decode_canceled_jobs, 3);
        assert_eq!(diagnostics.decode_canceled_shutdown_jobs, 1);
        assert_eq!(diagnostics.decode_canceled_obsolete_jobs, 1);
        assert_eq!(diagnostics.decode_canceled_prefetch_deadline_jobs, 1);
        assert_eq!(diagnostics.decode_canceled_unknown_jobs, 0);
        assert_eq!(diagnostics.decode_canceled_total_duration_us, 2_120);
        assert_eq!(diagnostics.decode_canceled_max_duration_us, 1_400);
        assert_eq!(diagnostics.decode_canceled_last_duration_us, 20);
        assert_eq!(diagnostics.decode_canceled_return_latency_total_us, 515);
        assert_eq!(diagnostics.decode_canceled_return_latency_max_us, 400);
        assert_eq!(diagnostics.decode_canceled_return_latency_last_us, 15);
        assert_eq!(diagnostics.decode_in_process_cpu_rgba_frames, 1);
        assert_eq!(diagnostics.decode_external_ffmpeg_cpu_rgba_frames, 1);
        assert_eq!(diagnostics.decode_playback_session_ring_hit_frames, 1);
        assert_eq!(diagnostics.decode_cache_hit_frames, 1);
        assert_eq!(diagnostics.decode_playback_cursor_frames, 2);
        assert_eq!(diagnostics.decode_scrub_cursor_frames, 1);
        assert_eq!(diagnostics.decode_random_access_still_frames, 1);
        assert_eq!(diagnostics.decode_canceled_playback_cursor_jobs, 1);
        assert_eq!(diagnostics.decode_canceled_scrub_cursor_jobs, 1);
        assert_eq!(diagnostics.decode_canceled_random_access_still_jobs, 1);
        assert_eq!(diagnostics.decode_total_duration_us, 3_565);
        assert_eq!(diagnostics.decode_max_duration_us, 2_500);
        assert_eq!(diagnostics.decode_last_duration_us, 40);
        assert_eq!(diagnostics.decode_queue_wait_total_us, 1_620);
        assert_eq!(diagnostics.decode_queue_wait_max_us, 1_200);
        assert_eq!(diagnostics.decode_queue_wait_last_us, 20);
        assert_eq!(diagnostics.decode_current_queue_wait_max_us, 1_200);
        assert_eq!(diagnostics.decode_prefetch_queue_wait_max_us, 400);
        assert_eq!(diagnostics.decode_seeked_frames, 1);
        assert_eq!(diagnostics.decode_decoded_frame_count, 48);
        assert_eq!(diagnostics.decode_max_decoded_frame_count, 48);
        assert_eq!(diagnostics.decode_threading_none_frames, 3);
        assert_eq!(diagnostics.decode_threading_frame_frames, 1);
        assert_eq!(diagnostics.decode_threading_slice_frames, 0);
        assert_eq!(diagnostics.decode_last_threading_count, 0);
        assert_eq!(diagnostics.decode_max_threading_count, 6);
        assert_eq!(diagnostics.decode_stage_durations.session_open_us, 100);
        assert_eq!(diagnostics.decode_stage_durations.cache_lookup_us, 34);
        assert_eq!(diagnostics.decode_stage_durations.seek_us, 300);
        assert_eq!(diagnostics.decode_stage_durations.packet_decode_us, 500);
        assert_eq!(diagnostics.decode_stage_durations.swscale_us, 70);
        assert_eq!(diagnostics.decode_stage_durations.rgba_copy_us, 30);
        assert_eq!(
            diagnostics.decode_stage_durations.external_process_us,
            2_450
        );
        assert_eq!(
            diagnostics.decode_max_frame_stage_durations.external_process_us,
            2_450
        );
        assert_eq!(diagnostics.decode_max_frame_queue_wait_us, 400);
        assert_eq!(
            diagnostics.decode_max_frame_bottleneck,
            AppUiPreviewDecodeBottleneck::ExternalProcess
        );
        let playback_profile = diagnostics.decode_access_mode_profiles.playback_cursor;
        assert_eq!(playback_profile.frames, 2);
        assert_eq!(playback_profile.external_ffmpeg_cpu_rgba_frames, 1);
        assert_eq!(playback_profile.playback_session_ring_hit_frames, 1);
        assert_eq!(playback_profile.total_duration_us, 2_540);
        assert_eq!(playback_profile.max_duration_us, 2_500);
        assert_eq!(playback_profile.last_duration_us, 40);
        assert_eq!(playback_profile.latency_buckets.le_10ms, 2);
        assert_eq!(playback_profile.latency_buckets.total(), 2);
        assert_eq!(playback_profile.queue_wait_total_us, 400);
        assert_eq!(playback_profile.queue_wait_max_us, 400);
        assert_eq!(playback_profile.queue_wait_last_us, 400);
        assert_eq!(playback_profile.queue_wait_buckets.le_10ms, 1);
        assert_eq!(playback_profile.queue_wait_buckets.total(), 1);
        assert_eq!(playback_profile.max_frame_queue_wait_us, 400);
        assert_eq!(
            playback_profile.max_frame_bottleneck,
            AppUiPreviewDecodeBottleneck::ExternalProcess
        );
        assert_eq!(playback_profile.canceled_jobs, 1);
        assert_eq!(playback_profile.canceled_prefetch_deadline_jobs, 1);
        assert_eq!(playback_profile.canceled_total_duration_us, 700);
        assert_eq!(playback_profile.canceled_max_duration_us, 700);
        assert_eq!(playback_profile.canceled_last_duration_us, 700);
        assert_eq!(playback_profile.canceled_return_latency_total_us, 100);
        assert_eq!(playback_profile.canceled_return_latency_max_us, 100);
        assert_eq!(playback_profile.canceled_return_latency_last_us, 100);
        assert_eq!(playback_profile.canceled_shutdown_jobs, 0);
        assert_eq!(playback_profile.canceled_obsolete_jobs, 0);
        assert_eq!(playback_profile.canceled_unknown_jobs, 0);
        assert_eq!(playback_profile.session_reused_frames, 2);
        assert_eq!(playback_profile.session_opened_frames, 0);
        assert_eq!(playback_profile.forward_reused_frames, 0);
        assert_eq!(playback_profile.seek_index_available_frames, 1);
        assert_eq!(playback_profile.seek_index_probe_backed_frames, 1);
        assert_eq!(playback_profile.seek_index_session_observed_frames, 0);
        assert_eq!(playback_profile.hardware_decode_active_frames, 0);
        assert_eq!(playback_profile.zero_copy_active_frames, 0);
        assert_eq!(playback_profile.gpu_texture_resident_frames, 0);
        assert_eq!(playback_profile.decoded_nv12_surface_frames, 1);
        assert_eq!(playback_profile.decoded_p010_surface_frames, 0);
        assert_eq!(playback_profile.renderer_import_ready_frames, 0);
        assert_eq!(
            playback_profile.hardware_decode_texture_residency_blocker_frames,
            2
        );
        assert_eq!(playback_profile.hardware_decode_auto_requested_frames, 0);
        assert_eq!(
            playback_profile.hardware_decode_prefer_gpu_requested_frames,
            2
        );
        assert_eq!(
            playback_profile.hardware_decode_require_gpu_requested_frames,
            0
        );
        assert_eq!(playback_profile.hardware_decode_cpu_not_requested_frames, 0);
        assert_eq!(playback_profile.hardware_decode_cpu_unavailable_frames, 0);
        assert_eq!(
            playback_profile.hardware_decode_backend_unavailable_frames,
            1
        );
        assert_eq!(playback_profile.hardware_decode_backend_boundary_frames, 1);
        assert_eq!(
            playback_profile.hardware_decode_gpu_resident_native_frames,
            0
        );
        assert_eq!(playback_profile.hardware_decode_candidate_d3d11va_frames, 1);
        assert_eq!(
            playback_profile.hardware_decode_candidate_videotoolbox_frames,
            0
        );
        assert_eq!(playback_profile.hardware_decode_candidate_vaapi_frames, 0);
        assert_eq!(playback_profile.hardware_decode_candidate_cuda_frames, 0);
        assert_eq!(
            playback_profile.hardware_decode_adapter_unavailable_frames,
            1
        );
        assert_eq!(playback_profile.seek_index_used_frames, 0);
        assert_eq!(playback_profile.seek_index_keyframes_max, 4);
        assert_eq!(playback_profile.seek_index_observed_packets_max, 128);
        assert_eq!(playback_profile.stage_durations.cache_lookup_us, 12);
        assert_eq!(
            playback_profile.max_frame_stage_durations.external_process_us,
            2_450
        );
        let scrub_profile = diagnostics.decode_access_mode_profiles.scrub_cursor;
        assert_eq!(scrub_profile.frames, 1);
        assert_eq!(scrub_profile.in_process_cpu_rgba_frames, 1);
        assert_eq!(scrub_profile.latency_buckets.le_10ms, 1);
        assert_eq!(scrub_profile.latency_buckets.total(), 1);
        assert_eq!(scrub_profile.seeked_frames, 1);
        assert_eq!(scrub_profile.bounded_any_seek_strategy_frames, 1);
        assert_eq!(scrub_profile.keyframe_seek_strategy_frames, 0);
        assert_eq!(scrub_profile.forward_reuse_frame_window_max, 1);
        assert_eq!(scrub_profile.forward_decode_budget_frames_max, 8);
        assert_eq!(scrub_profile.any_seek_window_ms_max, 120);
        assert_eq!(scrub_profile.session_reused_frames, 0);
        assert_eq!(scrub_profile.session_opened_frames, 1);
        assert_eq!(scrub_profile.forward_reused_frames, 0);
        assert_eq!(scrub_profile.seek_index_available_frames, 1);
        assert_eq!(scrub_profile.seek_index_probe_backed_frames, 1);
        assert_eq!(scrub_profile.seek_index_session_observed_frames, 0);
        assert_eq!(scrub_profile.decoded_nv12_surface_frames, 0);
        assert_eq!(scrub_profile.decoded_p010_surface_frames, 1);
        assert_eq!(scrub_profile.seek_index_used_frames, 1);
        assert_eq!(scrub_profile.seek_index_keyframes_max, 3);
        assert_eq!(scrub_profile.seek_index_observed_packets_max, 90);
        assert_eq!(scrub_profile.hardware_decode_auto_requested_frames, 1);
        assert_eq!(scrub_profile.hardware_decode_cpu_not_requested_frames, 1);
        assert_eq!(scrub_profile.hardware_decode_prefer_gpu_requested_frames, 0);
        assert_eq!(scrub_profile.queue_wait_total_us, 1_200);
        assert_eq!(scrub_profile.queue_wait_max_us, 1_200);
        assert_eq!(scrub_profile.queue_wait_last_us, 1_200);
        assert_eq!(scrub_profile.queue_wait_buckets.le_10ms, 1);
        assert_eq!(scrub_profile.queue_wait_buckets.total(), 1);
        assert_eq!(scrub_profile.canceled_jobs, 1);
        assert_eq!(scrub_profile.canceled_obsolete_jobs, 1);
        assert_eq!(scrub_profile.canceled_total_duration_us, 1_400);
        assert_eq!(scrub_profile.canceled_max_duration_us, 1_400);
        assert_eq!(scrub_profile.canceled_last_duration_us, 1_400);
        assert_eq!(scrub_profile.canceled_return_latency_total_us, 400);
        assert_eq!(scrub_profile.canceled_return_latency_max_us, 400);
        assert_eq!(scrub_profile.canceled_return_latency_last_us, 400);
        assert_eq!(scrub_profile.canceled_shutdown_jobs, 0);
        assert_eq!(scrub_profile.canceled_prefetch_deadline_jobs, 0);
        assert_eq!(scrub_profile.canceled_unknown_jobs, 0);
        assert_eq!(scrub_profile.decoded_frame_count, 48);
        assert_eq!(scrub_profile.max_decoded_frame_count, 48);
        assert_eq!(scrub_profile.stage_durations.packet_decode_us, 500);
        let still_profile = diagnostics.decode_access_mode_profiles.random_access_still;
        assert_eq!(still_profile.frames, 1);
        assert_eq!(still_profile.cache_hit_frames, 1);
        assert_eq!(still_profile.latency_buckets.le_10ms, 1);
        assert_eq!(still_profile.latency_buckets.total(), 1);
        assert_eq!(still_profile.queue_wait_total_us, 20);
        assert_eq!(still_profile.queue_wait_max_us, 20);
        assert_eq!(still_profile.queue_wait_last_us, 20);
        assert_eq!(still_profile.queue_wait_buckets.le_10ms, 1);
        assert_eq!(still_profile.queue_wait_buckets.total(), 1);
        assert_eq!(still_profile.canceled_jobs, 1);
        assert_eq!(still_profile.canceled_shutdown_jobs, 1);
        assert_eq!(still_profile.canceled_total_duration_us, 20);
        assert_eq!(still_profile.canceled_max_duration_us, 20);
        assert_eq!(still_profile.canceled_last_duration_us, 20);
        assert_eq!(still_profile.canceled_return_latency_total_us, 15);
        assert_eq!(still_profile.canceled_return_latency_max_us, 15);
        assert_eq!(still_profile.canceled_return_latency_last_us, 15);
        assert_eq!(still_profile.canceled_obsolete_jobs, 0);
        assert_eq!(still_profile.canceled_prefetch_deadline_jobs, 0);
        assert_eq!(still_profile.canceled_unknown_jobs, 0);
        assert_eq!(still_profile.session_reused_frames, 0);
        assert_eq!(still_profile.session_opened_frames, 1);
        assert_eq!(still_profile.seek_index_available_frames, 1);
        assert_eq!(still_profile.seek_index_probe_backed_frames, 0);
        assert_eq!(still_profile.seek_index_session_observed_frames, 1);
        assert_eq!(still_profile.decoded_nv12_surface_frames, 1);
        assert_eq!(still_profile.decoded_p010_surface_frames, 0);
        assert_eq!(still_profile.seek_index_used_frames, 0);
        assert_eq!(still_profile.seek_index_keyframes_max, 2);
        assert_eq!(still_profile.seek_index_observed_packets_max, 40);
        assert_eq!(still_profile.hardware_decode_auto_requested_frames, 1);
        assert_eq!(still_profile.hardware_decode_cpu_not_requested_frames, 1);
        assert_eq!(still_profile.stage_durations.cache_lookup_us, 20);
        assert_eq!(
            diagnostics.decode_access_mode_profiles.slowest_access_mode(),
            Some(PreviewDecodeAccessMode::PlaybackCursor)
        );
    }

    #[test]
    fn preview_diagnostics_count_decode_failures_by_access_mode() {
        let service = AppUiPreviewService::new();

        service.record_preview_decode_failure(
            PreviewDecodeAccessMode::ScrubCursor,
            Some(MediaPreviewFailureReason::Timeout),
        );
        service.record_preview_decode_failure(
            PreviewDecodeAccessMode::RandomAccessStillFrame,
            Some(MediaPreviewFailureReason::DecodeError),
        );
        service.record_preview_decode_failure(
            PreviewDecodeAccessMode::ScrubCursor,
            Some(MediaPreviewFailureReason::ForwardDecodeBudgetExhausted),
        );

        let diagnostics = service.diagnostics();

        assert_eq!(diagnostics.decode_failures, 3);
        assert_eq!(diagnostics.decode_timeout_failures, 1);
        assert_eq!(diagnostics.decode_budget_exhausted_failures, 1);
        let scrub_profile = diagnostics.decode_access_mode_profiles.scrub_cursor;
        assert_eq!(scrub_profile.failed_jobs, 2);
        assert_eq!(scrub_profile.timeout_failures, 1);
        assert_eq!(scrub_profile.budget_exhausted_failures, 1);
        let still_profile = diagnostics.decode_access_mode_profiles.random_access_still;
        assert_eq!(still_profile.failed_jobs, 1);
        assert_eq!(still_profile.timeout_failures, 0);
        assert_eq!(
            diagnostics
                .decode_performance_summary(50_000)
                .expect("decode evidence")
                .decode_timeout_failures,
            1
        );
    }

    #[test]
    fn preview_diagnostics_count_prefetch_preemptions_by_access_mode() {
        let service = AppUiPreviewService::new();

        service.record_preview_decode_cancel(
            PreviewDecodeAccessMode::PlaybackCursor,
            Some(MediaPreviewCancelReason::PrefetchPreemptedByCurrent),
            120,
            Some(80),
        );

        let diagnostics = service.diagnostics();
        assert_eq!(diagnostics.decode_canceled_jobs, 1);
        assert_eq!(diagnostics.decode_canceled_prefetch_preempted_jobs, 1);
        assert_eq!(diagnostics.decode_canceled_prefetch_deadline_jobs, 0);
        assert_eq!(diagnostics.decode_canceled_obsolete_jobs, 0);
        assert_eq!(diagnostics.decode_canceled_playback_cursor_jobs, 1);
        let playback_profile = diagnostics.decode_access_mode_profiles.playback_cursor;
        assert_eq!(playback_profile.canceled_jobs, 1);
        assert_eq!(playback_profile.canceled_prefetch_preempted_jobs, 1);
        assert_eq!(playback_profile.canceled_prefetch_deadline_jobs, 0);
        assert_eq!(playback_profile.canceled_return_latency_total_us, 40);
    }

    #[test]
    fn preview_diagnostics_count_playback_deadline_cancellations_by_access_mode() {
        let service = AppUiPreviewService::new();

        service.record_preview_decode_cancel(
            PreviewDecodeAccessMode::PlaybackCursor,
            Some(MediaPreviewCancelReason::PlaybackDeadline),
            0,
            Some(0),
        );

        let diagnostics = service.diagnostics();
        assert_eq!(diagnostics.decode_canceled_jobs, 1);
        assert_eq!(diagnostics.decode_canceled_playback_deadline_jobs, 1);
        assert_eq!(diagnostics.decode_canceled_prefetch_deadline_jobs, 0);
        assert_eq!(diagnostics.decode_canceled_obsolete_jobs, 0);
        assert_eq!(diagnostics.playback_schedule.current_drop_late_decisions, 1);
        assert_eq!(
            diagnostics.playback_schedule.current_proxy_or_hardware_recommended_decisions,
            1
        );
        assert_eq!(diagnostics.decode_canceled_playback_cursor_jobs, 1);
        let playback_profile = diagnostics.decode_access_mode_profiles.playback_cursor;
        assert_eq!(playback_profile.canceled_jobs, 1);
        assert_eq!(playback_profile.canceled_playback_deadline_jobs, 1);
        assert_eq!(playback_profile.canceled_prefetch_deadline_jobs, 0);
    }

    #[test]
    fn preview_diagnostics_count_still_preemptions_by_access_mode() {
        let service = AppUiPreviewService::new();

        service.record_preview_decode_cancel(
            PreviewDecodeAccessMode::RandomAccessStillFrame,
            Some(MediaPreviewCancelReason::StillPreemptedByRealtimeCurrent),
            320,
            Some(200),
        );

        let diagnostics = service.diagnostics();
        assert_eq!(diagnostics.decode_canceled_jobs, 1);
        assert_eq!(diagnostics.decode_canceled_still_preempted_jobs, 1);
        assert_eq!(diagnostics.decode_canceled_obsolete_jobs, 0);
        assert_eq!(diagnostics.decode_canceled_random_access_still_jobs, 1);
        let still_profile = diagnostics.decode_access_mode_profiles.random_access_still;
        assert_eq!(still_profile.canceled_jobs, 1);
        assert_eq!(still_profile.canceled_still_preempted_jobs, 1);
        assert_eq!(still_profile.canceled_obsolete_jobs, 0);
        assert_eq!(still_profile.canceled_return_latency_total_us, 120);
    }

    #[test]
    fn preview_decode_performance_report_fails_scrub_keyframe_seek_strategy() {
        let diagnostics = AppUiPreviewDiagnostics {
            decode_successes: 1,
            decode_in_process_cpu_rgba_frames: 1,
            decode_scrub_cursor_frames: 1,
            decode_access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles {
                scrub_cursor: AppUiPreviewDecodeAccessModeProfile {
                    frames: 1,
                    in_process_cpu_rgba_frames: 1,
                    keyframe_seek_strategy_frames: 1,
                    ..AppUiPreviewDecodeAccessModeProfile::default()
                },
                ..AppUiPreviewDecodeAccessModeProfiles::default()
            },
            ..AppUiPreviewDiagnostics::default()
        };

        let report = build_preview_decode_performance_report(
            diagnostics.decode_performance_summary(50_000),
            "preview-decode-scrub-seek-strategy-test",
            50_000,
        );

        assert_eq!(report.verdict, AppUiPreviewDecodePerformanceVerdict::Fail);
        assert!(report.checks.iter().any(|check| {
            check.code == "preview_decode_scrub_cursor_bounded_any_seek_strategy"
                && check.severity == AppUiPreviewDecodePerformanceSeverity::Fail
                && check.observed == 0
                && check.limit == Some(1)
        }));
        assert!(report.root_causes.iter().any(|root| {
            root.code == "preview_decode_scrub_cursor_not_using_low_latency_seek"
                && root.evidence.contains("scrub_frames=1")
                && root.evidence.contains("keyframe_seek_strategy_frames=1")
        }));
        assert!(report
            .actions
            .iter()
            .any(|action| action.code == "route_scrub_decode_through_bounded_any_seek"));
    }

    #[test]
    fn preview_decode_performance_report_fails_scrub_without_any_seek_window() {
        let diagnostics = AppUiPreviewDiagnostics {
            decode_successes: 1,
            decode_in_process_cpu_rgba_frames: 1,
            decode_scrub_cursor_frames: 1,
            decode_access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles {
                scrub_cursor: AppUiPreviewDecodeAccessModeProfile {
                    frames: 1,
                    in_process_cpu_rgba_frames: 1,
                    bounded_any_seek_strategy_frames: 1,
                    forward_reuse_frame_window_max: 1,
                    forward_decode_budget_frames_max: 8,
                    any_seek_window_ms_max: 0,
                    ..AppUiPreviewDecodeAccessModeProfile::default()
                },
                ..AppUiPreviewDecodeAccessModeProfiles::default()
            },
            ..AppUiPreviewDiagnostics::default()
        };

        let report = build_preview_decode_performance_report(
            diagnostics.decode_performance_summary(50_000),
            "preview-decode-scrub-seek-window-test",
            50_000,
        );

        assert_eq!(report.verdict, AppUiPreviewDecodePerformanceVerdict::Fail);
        assert!(report.checks.iter().any(|check| {
            check.code == "preview_decode_scrub_cursor_any_seek_window_ms"
                && check.severity == AppUiPreviewDecodePerformanceSeverity::Fail
                && check.observed == 0
                && check.limit == Some(1)
        }));
        assert!(report.root_causes.iter().any(|root| {
            root.code == "preview_decode_scrub_cursor_missing_any_seek_window"
                && root.evidence.contains("scrub_frames=1")
                && root.evidence.contains("any_seek_window_ms_max=0")
                && root.evidence.contains("forward_decode_budget_frames_max=8")
        }));
        assert!(report
            .actions
            .iter()
            .any(|action| action.code == "restore_scrub_bounded_any_seek_window"));
    }

    #[test]
    fn preview_decode_performance_report_fails_structured_budget_exhaustion() {
        let diagnostics = AppUiPreviewDiagnostics {
            decode_failures: 1,
            decode_budget_exhausted_failures: 1,
            decode_access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles {
                scrub_cursor: AppUiPreviewDecodeAccessModeProfile {
                    failed_jobs: 1,
                    budget_exhausted_failures: 1,
                    ..AppUiPreviewDecodeAccessModeProfile::default()
                },
                ..AppUiPreviewDecodeAccessModeProfiles::default()
            },
            ..AppUiPreviewDiagnostics::default()
        };

        let report = build_preview_decode_performance_report(
            diagnostics.decode_performance_summary(50_000),
            "test",
            50_000,
        );

        assert_eq!(report.verdict, AppUiPreviewDecodePerformanceVerdict::Fail);
        assert!(report.checks.iter().any(|check| {
            check.code == "preview_decode_forward_budget_exhausted_failures"
                && check.severity == AppUiPreviewDecodePerformanceSeverity::Fail
        }));
        assert!(report.root_causes.iter().any(|root| {
            root.code == "preview_decode_forward_budget_exhausted"
                && root.evidence.contains("scrub_budget_exhausted_failures=1")
        }));
        assert!(report
            .actions
            .iter()
            .any(|action| action.code == "inspect_access_mode_forward_decode_budget"));
    }

    #[test]
    fn preview_playback_schedule_diagnostics_records_clock_contract() {
        let service = AppUiPreviewService::new();

        service.record_playback_current_deadline_budget(Some(33_333));
        service.record_playback_forward_prefetch_window(Some(2));

        let diagnostics = service.diagnostics().playback_schedule;
        assert_eq!(diagnostics.last_current_deadline_budget_us, Some(33_333));
        assert_eq!(diagnostics.current_deadline_assignments, 1);
        assert_eq!(diagnostics.current_deadline_missing_frame_rate, 0);
        assert_eq!(diagnostics.current_decode_decisions, 1);
        assert_eq!(diagnostics.current_drop_late_decisions, 0);
        assert_eq!(
            diagnostics.current_proxy_or_hardware_recommended_decisions,
            0
        );
        assert_eq!(
            diagnostics.forward_prefetch_horizon_us,
            MEDIA_PREVIEW_FORWARD_PREFETCH_HORIZON_US
        );
        assert_eq!(diagnostics.last_forward_prefetch_window_frames, Some(2));
        assert_eq!(
            diagnostics.forward_prefetch_min_frames,
            MEDIA_PREVIEW_FORWARD_PREFETCH_MIN_FRAMES
        );
        assert_eq!(
            diagnostics.forward_prefetch_max_frames,
            MEDIA_PREVIEW_FORWARD_PREFETCH_MAX_FRAMES
        );
        assert_eq!(diagnostics.forward_prefetch_window_evaluations, 1);
        assert_eq!(diagnostics.forward_prefetch_invalid_frame_rate, 0);
        service.shutdown();
    }

    #[test]
    fn preview_decode_performance_report_warns_invalid_playback_clock_contract() {
        let diagnostics = AppUiPreviewDiagnostics {
            decode_canceled_jobs: 1,
            playback_schedule: AppUiPreviewPlaybackScheduleDiagnostics {
                current_deadline_missing_frame_rate: 1,
                forward_prefetch_invalid_frame_rate: 1,
                forward_prefetch_horizon_us: MEDIA_PREVIEW_FORWARD_PREFETCH_HORIZON_US,
                forward_prefetch_min_frames: MEDIA_PREVIEW_FORWARD_PREFETCH_MIN_FRAMES,
                forward_prefetch_max_frames: MEDIA_PREVIEW_FORWARD_PREFETCH_MAX_FRAMES,
                ..AppUiPreviewPlaybackScheduleDiagnostics::default()
            },
            ..AppUiPreviewDiagnostics::default()
        };

        let report = build_preview_decode_performance_report(
            diagnostics.decode_performance_summary(50_000),
            "preview-playback-clock-test",
            50_000,
        );

        assert_eq!(report.verdict, AppUiPreviewDecodePerformanceVerdict::Warn);
        assert!(report.checks.iter().any(|check| {
            check.code == "preview_decode_playback_deadline_invalid_frame_rate"
                && check.severity == AppUiPreviewDecodePerformanceSeverity::Warn
                && check.observed == 1
        }));
        assert!(report.checks.iter().any(|check| {
            check.code == "preview_decode_prefetch_window_invalid_frame_rate"
                && check.severity == AppUiPreviewDecodePerformanceSeverity::Warn
                && check.observed == 1
        }));
        assert!(report.root_causes.iter().any(|root| {
            root.code == "preview_decode_playback_deadline_invalid_frame_rate"
                && root.evidence.contains("current_deadline_missing_frame_rate=1")
        }));
        assert!(report.root_causes.iter().any(|root| {
            root.code == "preview_decode_prefetch_window_invalid_frame_rate"
                && root.evidence.contains("forward_prefetch_invalid_frame_rate=1")
        }));
        assert!(report
            .actions
            .iter()
            .any(|action| action.code == "fix_sequence_playback_frame_rate_contract"));
        assert!(report
            .actions
            .iter()
            .any(|action| action.code == "fix_sequence_prefetch_frame_rate_contract"));
    }

    #[test]
    fn preview_decode_performance_report_fails_structured_timeout_failures() {
        let diagnostics = AppUiPreviewDiagnostics {
            decode_failures: 1,
            decode_timeout_failures: 1,
            decode_access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles {
                scrub_cursor: AppUiPreviewDecodeAccessModeProfile {
                    failed_jobs: 1,
                    timeout_failures: 1,
                    ..AppUiPreviewDecodeAccessModeProfile::default()
                },
                ..AppUiPreviewDecodeAccessModeProfiles::default()
            },
            ..AppUiPreviewDiagnostics::default()
        };

        let report = build_preview_decode_performance_report(
            diagnostics.decode_performance_summary(50_000),
            "test",
            50_000,
        );

        assert_eq!(report.verdict, AppUiPreviewDecodePerformanceVerdict::Fail);
        assert!(
            report.checks.iter().any(|check| check.code == "preview_decode_timeout_failures"
                && check.severity == AppUiPreviewDecodePerformanceSeverity::Fail)
        );
        assert!(report
            .root_causes
            .iter()
            .any(|root| root.code == "preview_decode_timeout_failures"
                && root.evidence.contains("scrub_timeout_failures=1")));
        assert!(report
            .actions
            .iter()
            .any(|action| action.code == "inspect_access_mode_decode_timeout_budget"));
    }

    #[test]
    fn preview_decode_performance_report_defaults_to_no_required_access_modes() {
        let diagnostics = AppUiPreviewDiagnostics {
            decode_successes: 1,
            decode_in_process_cpu_rgba_frames: 1,
            decode_access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles {
                random_access_still: AppUiPreviewDecodeAccessModeProfile {
                    frames: 1,
                    in_process_cpu_rgba_frames: 1,
                    ..AppUiPreviewDecodeAccessModeProfile::default()
                },
                ..AppUiPreviewDecodeAccessModeProfiles::default()
            },
            ..AppUiPreviewDiagnostics::default()
        };

        let report = build_preview_decode_performance_report(
            diagnostics.decode_performance_summary(50_000),
            "preview-decode-default-coverage-test",
            50_000,
        );

        assert_eq!(report.verdict, AppUiPreviewDecodePerformanceVerdict::Warn);
        assert!(report.required_access_modes.is_empty());
        assert!(!report
            .checks
            .iter()
            .any(|check| check.code == "preview_decode_scrub_cursor_sampled"));
    }

    #[test]
    fn preview_decode_performance_report_fails_missing_required_access_modes() {
        let diagnostics = AppUiPreviewDiagnostics {
            decode_successes: 1,
            decode_in_process_cpu_rgba_frames: 1,
            decode_access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles {
                random_access_still: AppUiPreviewDecodeAccessModeProfile {
                    frames: 1,
                    in_process_cpu_rgba_frames: 1,
                    ..AppUiPreviewDecodeAccessModeProfile::default()
                },
                ..AppUiPreviewDecodeAccessModeProfiles::default()
            },
            ..AppUiPreviewDiagnostics::default()
        };

        let report = build_preview_decode_performance_report_with_required_access_modes(
            diagnostics.decode_performance_summary(50_000),
            "preview-decode-required-coverage-test",
            50_000,
            &[
                PreviewDecodeAccessMode::ScrubCursor,
                PreviewDecodeAccessMode::RandomAccessStillFrame,
            ],
        );

        assert_eq!(report.verdict, AppUiPreviewDecodePerformanceVerdict::Fail);
        assert_eq!(
            report.required_access_modes,
            vec![
                PreviewDecodeAccessMode::ScrubCursor,
                PreviewDecodeAccessMode::RandomAccessStillFrame,
            ]
        );
        assert!(report.checks.iter().any(|check| {
            check.code == "preview_decode_scrub_cursor_sampled"
                && check.severity == AppUiPreviewDecodePerformanceSeverity::Fail
                && check.observed == 0
        }));
        assert!(report.checks.iter().any(|check| {
            check.code == "preview_decode_random_access_still_sampled"
                && check.severity == AppUiPreviewDecodePerformanceSeverity::Pass
                && check.observed == 1
        }));
        assert!(report.root_causes.iter().any(|root| {
            root.code == "preview_decode_required_access_mode_missing"
                && root.evidence.contains("access_mode=ScrubCursor")
        }));
        assert!(report
            .actions
            .iter()
            .any(|action| action.code == "exercise_required_preview_access_modes"));
    }

    #[test]
    fn preview_decode_performance_report_rejects_cache_only_required_access_mode() {
        let diagnostics = AppUiPreviewDiagnostics {
            decode_successes: 1,
            decode_cache_hit_frames: 1,
            decode_access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles {
                scrub_cursor: AppUiPreviewDecodeAccessModeProfile {
                    frames: 1,
                    cache_hit_frames: 1,
                    bounded_any_seek_strategy_frames: 1,
                    any_seek_window_ms_max: 1,
                    ..AppUiPreviewDecodeAccessModeProfile::default()
                },
                ..AppUiPreviewDecodeAccessModeProfiles::default()
            },
            ..AppUiPreviewDiagnostics::default()
        };

        let report = build_preview_decode_performance_report_with_required_access_modes(
            diagnostics.decode_performance_summary(50_000),
            "preview-decode-required-cache-only-test",
            50_000,
            &[PreviewDecodeAccessMode::ScrubCursor],
        );

        assert_eq!(report.verdict, AppUiPreviewDecodePerformanceVerdict::Fail);
        assert!(report.checks.iter().any(|check| {
            check.code == "preview_decode_scrub_cursor_sampled"
                && check.severity == AppUiPreviewDecodePerformanceSeverity::Pass
        }));
        assert!(report.checks.iter().any(|check| {
            check.code == "preview_decode_scrub_cursor_mode_local_sampled"
                && check.severity == AppUiPreviewDecodePerformanceSeverity::Fail
                && check.observed == 0
        }));
        assert!(report.root_causes.iter().any(|root| {
            root.code == "preview_decode_required_access_mode_cache_only"
                && root.evidence.contains("access_mode=ScrubCursor")
                && root.evidence.contains("cache_hit_frames=1")
        }));
        assert!(report.actions.iter().any(|action| {
            action.code == "exercise_required_preview_access_modes_without_global_cache"
        }));
    }

    #[test]
    fn preview_decode_performance_report_accepts_playback_ring_as_mode_local_evidence() {
        let diagnostics = AppUiPreviewDiagnostics {
            decode_successes: 1,
            decode_playback_session_ring_hit_frames: 1,
            decode_access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles {
                playback_cursor: AppUiPreviewDecodeAccessModeProfile {
                    frames: 1,
                    playback_session_ring_hit_frames: 1,
                    ..AppUiPreviewDecodeAccessModeProfile::default()
                },
                ..AppUiPreviewDecodeAccessModeProfiles::default()
            },
            ..AppUiPreviewDiagnostics::default()
        };

        let report = build_preview_decode_performance_report_with_required_access_modes(
            diagnostics.decode_performance_summary(50_000),
            "preview-decode-required-playback-ring-test",
            50_000,
            &[PreviewDecodeAccessMode::PlaybackCursor],
        );

        assert_ne!(report.verdict, AppUiPreviewDecodePerformanceVerdict::Fail);
        assert!(report.checks.iter().any(|check| {
            check.code == "preview_decode_playback_cursor_mode_local_sampled"
                && check.severity == AppUiPreviewDecodePerformanceSeverity::Pass
                && check.observed == 1
        }));
        assert!(!report
            .root_causes
            .iter()
            .any(|root| root.code == "preview_decode_required_access_mode_cache_only"));
    }

    #[test]
    fn preview_decode_performance_report_classifies_codec_bound_slow_frame() {
        let diagnostics = AppUiPreviewDiagnostics {
            decode_successes: 1,
            decode_in_process_cpu_rgba_frames: 1,
            decode_total_duration_us: 120_000,
            decode_max_duration_us: 120_000,
            decode_last_duration_us: 120_000,
            decode_seeked_frames: 1,
            decode_decoded_frame_count: 36,
            decode_max_decoded_frame_count: 36,
            decode_stage_durations: PreviewDecodeStageDurations {
                packet_decode_us: 95_000,
                seek_us: 10_000,
                swscale_us: 8_000,
                rgba_copy_us: 2_000,
                ..PreviewDecodeStageDurations::default()
            },
            decode_max_frame_stage_durations: PreviewDecodeStageDurations {
                packet_decode_us: 95_000,
                seek_us: 10_000,
                swscale_us: 8_000,
                rgba_copy_us: 2_000,
                ..PreviewDecodeStageDurations::default()
            },
            decode_access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles {
                scrub_cursor: AppUiPreviewDecodeAccessModeProfile {
                    frames: 1,
                    in_process_cpu_rgba_frames: 1,
                    total_duration_us: 120_000,
                    max_duration_us: 120_000,
                    last_duration_us: 120_000,
                    seeked_frames: 1,
                    bounded_any_seek_strategy_frames: 1,
                    any_seek_window_ms_max: 1,
                    decoded_frame_count: 36,
                    max_decoded_frame_count: 36,
                    stage_durations: PreviewDecodeStageDurations {
                        packet_decode_us: 95_000,
                        seek_us: 10_000,
                        swscale_us: 8_000,
                        rgba_copy_us: 2_000,
                        ..PreviewDecodeStageDurations::default()
                    },
                    max_frame_stage_durations: PreviewDecodeStageDurations {
                        packet_decode_us: 95_000,
                        seek_us: 10_000,
                        swscale_us: 8_000,
                        rgba_copy_us: 2_000,
                        ..PreviewDecodeStageDurations::default()
                    },
                    ..AppUiPreviewDecodeAccessModeProfile::default()
                },
                ..AppUiPreviewDecodeAccessModeProfiles::default()
            },
            ..AppUiPreviewDiagnostics::default()
        };

        let report = build_preview_decode_performance_report(
            diagnostics.decode_performance_summary(50_000),
            "preview-decode-test",
            50_000,
        );

        assert_eq!(report.verdict, AppUiPreviewDecodePerformanceVerdict::Fail);
        let summary = report.summary.expect("decode summary");
        assert_eq!(
            summary.primary_bottleneck,
            AppUiPreviewDecodeBottleneck::PacketDecode
        );
        assert_eq!(
            summary.slowest_access_mode,
            Some(PreviewDecodeAccessMode::ScrubCursor)
        );
        assert!(report
            .root_causes
            .iter()
            .any(|root| root.code == "preview_decode_frame_over_budget"));
        assert!(report.root_causes.iter().any(|root| {
            root.code == "preview_decode_frame_over_budget"
                && root.evidence.contains("slowest_access_mode=ScrubCursor")
        }));
        assert!(report.checks.iter().any(|check| {
            check.code == "preview_decode_scrub_cursor_max_frame_us"
                && check.severity == AppUiPreviewDecodePerformanceSeverity::Fail
                && check.observed == 120_000
                && check.limit == Some(50_000)
        }));
        assert!(report.root_causes.iter().any(|root| {
            root.code == "preview_decode_access_mode_over_budget"
                && root.area == AppUiPreviewDecodePerformanceArea::AccessMode
                && root.evidence.contains("access_mode=ScrubCursor")
                && root.evidence.contains("packet_decode_us=95000")
                && root.evidence.contains("seek_index_available_frames=0")
        }));
        assert!(report.root_causes.iter().any(|root| {
            root.code == "preview_decode_scrub_cursor_without_seek_index_evidence"
                && root.area == AppUiPreviewDecodePerformanceArea::AccessMode
                && root.evidence.contains("seek_index_available_frames=0")
        }));
        assert!(report
            .root_causes
            .iter()
            .any(|root| root.code == "preview_decode_codec_or_gop_bound"));
        assert!(report
            .actions
            .iter()
            .any(|action| action.code == "enable_proxy_or_hardware_decode"));
        assert!(report
            .actions
            .iter()
            .any(|action| action.code == "build_preview_seek_index_evidence"));
        assert!(report
            .actions
            .iter()
            .any(|action| action.code == "inspect_preview_decode_access_mode_profile"));
    }

    #[test]
    fn preview_decode_performance_report_classifies_queue_wait_bound_frame() {
        let diagnostics = AppUiPreviewDiagnostics {
            decode_successes: 1,
            decode_in_process_cpu_rgba_frames: 1,
            decode_total_duration_us: 12_000,
            decode_max_duration_us: 12_000,
            decode_last_duration_us: 12_000,
            decode_queue_wait_total_us: 95_000,
            decode_queue_wait_max_us: 95_000,
            decode_queue_wait_last_us: 95_000,
            decode_current_queue_wait_max_us: 95_000,
            decode_prefetch_queue_wait_max_us: 15_000,
            enqueued_jobs: 4,
            queue_evicted_prefetch_jobs: 1,
            interactive_cancel_requests: 1,
            interactive_cancel_scheduler_requests: 2,
            interactive_cancel_queued_jobs: 3,
            queue_canceled_jobs: 3,
            queue_pruned_obsolete_jobs: 2,
            queue_promoted_current_jobs: 1,
            worker_queue: MediaPreviewJobQueueDiagnostics {
                queued_jobs: 3,
                queued_current_jobs: 2,
                queued_prefetch_jobs: 1,
                queued_playback_cursor_jobs: 1,
                queued_expired_playback_current_jobs: 1,
                dropped_expired_playback_current_jobs: 0,
                queued_scrub_cursor_jobs: 1,
                queued_random_access_still_jobs: 1,
                queued_any_lane_eligible_jobs: 3,
                queued_playback_lane_eligible_jobs: 1,
                queued_scrub_lane_eligible_jobs: 1,
                queued_still_lane_eligible_jobs: 1,
                queued_interactive_lane_eligible_jobs: 2,
                closed: false,
            },
            worker_activity: PreviewWorkerActivityDiagnostics {
                in_flight_jobs: 2,
                in_flight_current_jobs: 1,
                in_flight_prefetch_jobs: 1,
                in_flight_playback_cursor_jobs: 1,
                in_flight_scrub_cursor_jobs: 1,
                ..PreviewWorkerActivityDiagnostics::default()
            },
            decode_access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles {
                scrub_cursor: AppUiPreviewDecodeAccessModeProfile {
                    frames: 1,
                    in_process_cpu_rgba_frames: 1,
                    total_duration_us: 12_000,
                    max_duration_us: 12_000,
                    last_duration_us: 12_000,
                    queue_wait_total_us: 95_000,
                    queue_wait_max_us: 95_000,
                    queue_wait_last_us: 95_000,
                    bounded_any_seek_strategy_frames: 1,
                    any_seek_window_ms_max: 1,
                    stage_durations: PreviewDecodeStageDurations {
                        packet_decode_us: 10_000,
                        swscale_us: 1_000,
                        rgba_copy_us: 500,
                        ..PreviewDecodeStageDurations::default()
                    },
                    max_frame_stage_durations: PreviewDecodeStageDurations {
                        packet_decode_us: 10_000,
                        swscale_us: 1_000,
                        rgba_copy_us: 500,
                        ..PreviewDecodeStageDurations::default()
                    },
                    max_frame_queue_wait_us: 95_000,
                    max_frame_bottleneck: AppUiPreviewDecodeBottleneck::QueueWait,
                    ..AppUiPreviewDecodeAccessModeProfile::default()
                },
                ..AppUiPreviewDecodeAccessModeProfiles::default()
            },
            decode_stage_durations: PreviewDecodeStageDurations {
                packet_decode_us: 10_000,
                swscale_us: 1_000,
                rgba_copy_us: 500,
                ..PreviewDecodeStageDurations::default()
            },
            decode_max_frame_stage_durations: PreviewDecodeStageDurations {
                packet_decode_us: 10_000,
                swscale_us: 1_000,
                rgba_copy_us: 500,
                ..PreviewDecodeStageDurations::default()
            },
            decode_max_frame_queue_wait_us: 95_000,
            decode_max_frame_bottleneck: AppUiPreviewDecodeBottleneck::QueueWait,
            scheduler: MediaPreviewSchedulerDiagnostics {
                skipped_decode_access_mode_mismatch: 1,
                completed_stale_access_mode_mismatch: 1,
                dropped_pending_window_requests: 2,
                ..MediaPreviewSchedulerDiagnostics::default()
            },
            ..AppUiPreviewDiagnostics::default()
        };

        let report = build_preview_decode_performance_report(
            diagnostics.decode_performance_summary(50_000),
            "preview-decode-queue-wait-test",
            50_000,
        );

        assert_eq!(report.verdict, AppUiPreviewDecodePerformanceVerdict::Warn);
        let summary = report.summary.expect("decode summary");
        assert_eq!(
            summary.primary_bottleneck,
            AppUiPreviewDecodeBottleneck::QueueWait
        );
        assert_eq!(summary.enqueued_jobs, 4);
        assert_eq!(summary.queue_evicted_prefetch_jobs, 1);
        assert_eq!(summary.interactive_cancel_requests, 1);
        assert_eq!(summary.interactive_cancel_scheduler_requests, 2);
        assert_eq!(summary.interactive_cancel_queued_jobs, 3);
        assert_eq!(summary.queue_canceled_jobs, 3);
        assert_eq!(summary.queue_pruned_obsolete_jobs, 2);
        assert_eq!(summary.queue_promoted_current_jobs, 1);
        assert_eq!(summary.worker_queue.queued_jobs, 3);
        assert_eq!(summary.worker_queue.queued_current_jobs, 2);
        assert_eq!(summary.worker_queue.queued_prefetch_jobs, 1);
        assert_eq!(summary.worker_queue.queued_playback_cursor_jobs, 1);
        assert_eq!(summary.worker_queue.queued_expired_playback_current_jobs, 1);
        assert_eq!(summary.worker_queue.queued_scrub_cursor_jobs, 1);
        assert_eq!(summary.worker_queue.queued_random_access_still_jobs, 1);
        assert_eq!(summary.worker_queue.queued_any_lane_eligible_jobs, 3);
        assert_eq!(summary.worker_queue.queued_playback_lane_eligible_jobs, 1);
        assert_eq!(summary.worker_queue.queued_scrub_lane_eligible_jobs, 1);
        assert_eq!(summary.worker_queue.queued_still_lane_eligible_jobs, 1);
        assert_eq!(
            summary.worker_queue.queued_interactive_lane_eligible_jobs,
            2
        );
        assert_eq!(summary.worker_activity.in_flight_jobs, 2);
        assert_eq!(summary.worker_activity.in_flight_current_jobs, 1);
        assert_eq!(summary.worker_activity.in_flight_prefetch_jobs, 1);
        assert_eq!(summary.worker_activity.in_flight_playback_cursor_jobs, 1);
        assert_eq!(summary.worker_activity.in_flight_scrub_cursor_jobs, 1);
        assert_eq!(summary.scheduler.dropped_pending_window_requests, 2);
        assert!(report.root_causes.iter().any(|root| root.code
            == "preview_decode_queue_wait_bound"
            && root.evidence.contains("queued_jobs=3")
            && root.evidence.contains("queued_current_jobs=2")
            && root.evidence.contains("queued_prefetch_jobs=1")
            && root.evidence.contains("queued_playback_cursor_jobs=1")
            && root.evidence.contains("queued_expired_playback_current_jobs=1")
            && root.evidence.contains("queued_scrub_cursor_jobs=1")
            && root.evidence.contains("queued_random_access_still_jobs=1")
            && root.evidence.contains("queued_any_lane_eligible_jobs=3")
            && root.evidence.contains("queued_playback_lane_eligible_jobs=1")
            && root.evidence.contains("queued_scrub_lane_eligible_jobs=1")
            && root.evidence.contains("queued_still_lane_eligible_jobs=1")
            && root.evidence.contains("queued_interactive_lane_eligible_jobs=2")
            && root.evidence.contains("in_flight_jobs=2")
            && root.evidence.contains("in_flight_current_jobs=1")
            && root.evidence.contains("in_flight_prefetch_jobs=1")
            && root.evidence.contains("in_flight_playback_cursor_jobs=1")
            && root.evidence.contains("in_flight_scrub_cursor_jobs=1")
            && root.evidence.contains("queue_evicted_prefetch_jobs=1")
            && root.evidence.contains("interactive_cancel_requests=1")
            && root.evidence.contains("interactive_cancel_scheduler_requests=2")
            && root.evidence.contains("interactive_cancel_queued_jobs=3")
            && root.evidence.contains("queue_canceled_jobs=3")
            && root.evidence.contains("queue_pruned_obsolete_jobs=2")
            && root.evidence.contains("queue_promoted_current_jobs=1")));
        assert!(report.checks.iter().any(|check| {
            check.code == "preview_decode_scrub_cursor_queue_wait_max_us"
                && check.severity == AppUiPreviewDecodePerformanceSeverity::Warn
                && check.observed == 95_000
                && check.limit == Some(50_000)
        }));
        assert!(report.root_causes.iter().any(|root| {
            root.code == "preview_decode_access_mode_queue_wait_bound"
                && root.area == AppUiPreviewDecodePerformanceArea::AccessMode
                && root.evidence.contains("access_mode=ScrubCursor")
                && root.evidence.contains("queue_wait_max_us=95000")
        }));
        assert!(report
            .root_causes
            .iter()
            .any(|root| root.code == "preview_decode_access_mode_mismatch"));
        assert!(report
            .root_causes
            .iter()
            .any(|root| root.code == "preview_decode_pending_window_backpressure"));
        assert!(report
            .actions
            .iter()
            .any(|action| action.code == "prioritize_current_preview_decode"));
        assert!(report
            .actions
            .iter()
            .any(|action| action.code == "inspect_preview_access_mode_transitions"));
    }

    #[test]
    fn preview_decode_performance_report_flags_expired_playback_current_queue() {
        let diagnostics = AppUiPreviewDiagnostics {
            decode_successes: 1,
            decode_in_process_cpu_rgba_frames: 1,
            decode_total_duration_us: 12_000,
            decode_max_duration_us: 12_000,
            decode_last_duration_us: 12_000,
            decode_queue_wait_total_us: 10_000,
            decode_queue_wait_max_us: 10_000,
            decode_queue_wait_last_us: 10_000,
            worker_queue: MediaPreviewJobQueueDiagnostics {
                queued_jobs: 3,
                queued_current_jobs: 2,
                queued_prefetch_jobs: 1,
                queued_playback_cursor_jobs: 2,
                queued_expired_playback_current_jobs: 2,
                dropped_expired_playback_current_jobs: 1,
                queued_any_lane_eligible_jobs: 3,
                queued_playback_lane_eligible_jobs: 2,
                queued_interactive_lane_eligible_jobs: 2,
                closed: false,
                ..MediaPreviewJobQueueDiagnostics::default()
            },
            worker_activity: PreviewWorkerActivityDiagnostics {
                in_flight_jobs: 1,
                in_flight_playback_cursor_jobs: 1,
                ..PreviewWorkerActivityDiagnostics::default()
            },
            playback_schedule: AppUiPreviewPlaybackScheduleDiagnostics {
                current_deadline_assignments: 4,
                current_decode_decisions: 3,
                current_drop_late_decisions: 1,
                current_proxy_or_hardware_recommended_decisions: 1,
                ..AppUiPreviewPlaybackScheduleDiagnostics::default()
            },
            ..AppUiPreviewDiagnostics::default()
        };

        let report = build_preview_decode_performance_report(
            diagnostics.decode_performance_summary(50_000),
            "preview-decode-expired-playback-queue-test",
            50_000,
        );

        assert_eq!(report.verdict, AppUiPreviewDecodePerformanceVerdict::Warn);
        let summary = report.summary.expect("decode summary");
        assert_eq!(summary.worker_queue.queued_expired_playback_current_jobs, 2);
        assert_eq!(
            summary.worker_queue.dropped_expired_playback_current_jobs,
            1
        );
        assert!(report.checks.iter().any(|check| {
            check.code == "preview_decode_expired_playback_current_queue"
                && check.severity == AppUiPreviewDecodePerformanceSeverity::Warn
                && check.observed == 3
                && check.limit == Some(0)
        }));
        assert!(report.root_causes.iter().any(|root| {
            root.code == "preview_decode_expired_playback_current_queue"
                && root.severity == AppUiPreviewDecodePerformanceSeverity::Warn
                && root.evidence.contains("queued_expired_playback_current_jobs=2")
                && root.evidence.contains("dropped_expired_playback_current_jobs=1")
                && root.evidence.contains("queued_playback_cursor_jobs=2")
                && root.evidence.contains("current_deadline_assignments=4")
                && root.evidence.contains("current_decode_decisions=3")
                && root.evidence.contains("current_drop_late_decisions=1")
                && root.evidence.contains("current_proxy_or_hardware_recommended_decisions=1")
        }));
        assert!(report
            .actions
            .iter()
            .any(|action| action.code == "drop_expired_playback_queue_work"));
    }

    #[test]
    fn preview_decode_performance_report_flags_playback_buffering_stall_expiration() {
        let diagnostics = AppUiPreviewDiagnostics {
            playback_current_stalled_expirations: 1,
            queue_canceled_jobs: 1,
            scheduler: MediaPreviewSchedulerDiagnostics {
                canceled_requests: 1,
                ..MediaPreviewSchedulerDiagnostics::default()
            },
            worker_activity: PreviewWorkerActivityDiagnostics {
                in_flight_jobs: 1,
                in_flight_current_jobs: 1,
                ..PreviewWorkerActivityDiagnostics::default()
            },
            ..AppUiPreviewDiagnostics::default()
        };

        let report = build_preview_decode_performance_report(
            diagnostics.decode_performance_summary(50_000),
            "preview-decode-playback-buffering-stall-test",
            50_000,
        );

        assert_eq!(report.verdict, AppUiPreviewDecodePerformanceVerdict::Warn);
        let summary = report.summary.expect("decode summary");
        assert_eq!(summary.playback_current_stalled_expirations, 1);
        assert!(report.checks.iter().any(|check| {
            check.code == "preview_decode_evidence_present"
                && check.severity == AppUiPreviewDecodePerformanceSeverity::Pass
        }));
        assert!(report.checks.iter().any(|check| {
            check.code == "preview_decode_playback_buffering_stall_expirations"
                && check.severity == AppUiPreviewDecodePerformanceSeverity::Warn
                && check.observed == 1
                && check.limit == Some(0)
        }));
        assert!(report.root_causes.iter().any(|root| {
            root.code == "preview_decode_playback_buffering_stall_expirations"
                && root.severity == AppUiPreviewDecodePerformanceSeverity::Warn
                && root.evidence.contains("playback_current_stalled_expirations=1")
                && root.evidence.contains("queue_canceled_jobs=1")
                && root.evidence.contains("scheduler_canceled_requests=1")
                && root.evidence.contains("in_flight_jobs=1")
                && root.evidence.contains("in_flight_current_jobs=1")
        }));
        assert!(report
            .actions
            .iter()
            .any(|action| action.code == "diagnose_preview_current_frame_stalls"));
    }

    #[test]
    fn preview_decode_performance_report_flags_playback_sustained_pressure() {
        let diagnostics = AppUiPreviewDiagnostics {
            decode_canceled_jobs: 2,
            playback_schedule: AppUiPreviewPlaybackScheduleDiagnostics {
                current_late_streak: 2,
                sustained_pressure_active: true,
                sustained_pressure_events: 1,
                sustained_pressure_recoveries: 0,
                current_drop_late_decisions: 2,
                current_proxy_or_hardware_recommended_decisions: 2,
                prefetch_skipped_sustained_pressure: 1,
                ..AppUiPreviewPlaybackScheduleDiagnostics::default()
            },
            worker_queue: MediaPreviewJobQueueDiagnostics {
                queued_prefetch_jobs: 1,
                ..MediaPreviewJobQueueDiagnostics::default()
            },
            worker_activity: PreviewWorkerActivityDiagnostics {
                in_flight_prefetch_jobs: 1,
                ..PreviewWorkerActivityDiagnostics::default()
            },
            ..AppUiPreviewDiagnostics::default()
        };

        let report = build_preview_decode_performance_report(
            diagnostics.decode_performance_summary(50_000),
            "preview-decode-playback-pressure-test",
            50_000,
        );

        assert_eq!(report.verdict, AppUiPreviewDecodePerformanceVerdict::Warn);
        assert!(report.checks.iter().any(|check| {
            check.code == "preview_decode_playback_sustained_pressure_events"
                && check.severity == AppUiPreviewDecodePerformanceSeverity::Warn
                && check.observed == 1
                && check.limit == Some(0)
        }));
        assert!(report.root_causes.iter().any(|root| {
            root.code == "preview_decode_playback_sustained_pressure"
                && root.severity == AppUiPreviewDecodePerformanceSeverity::Warn
                && root.evidence.contains("sustained_pressure_active=true")
                && root.evidence.contains("current_late_streak=2")
                && root.evidence.contains("prefetch_skipped_sustained_pressure=1")
        }));
        assert!(report
            .actions
            .iter()
            .any(|action| action.code == "recover_playback_scheduler_pressure"));
    }

    #[test]
    fn preview_decode_performance_report_checks_access_mode_p95_upper_bounds() {
        let slow_buckets = AppUiPreviewDecodeLatencyBuckets {
            le_50ms: 1,
            le_80ms: 19,
            ..AppUiPreviewDecodeLatencyBuckets::default()
        };
        let diagnostics = AppUiPreviewDiagnostics {
            decode_successes: 20,
            decode_in_process_cpu_rgba_frames: 20,
            decode_total_duration_us: 1_250_000,
            decode_max_duration_us: 70_000,
            decode_last_duration_us: 60_000,
            decode_queue_wait_total_us: 1_200_000,
            decode_queue_wait_max_us: 70_000,
            decode_queue_wait_last_us: 60_000,
            decode_current_queue_wait_max_us: 70_000,
            decode_access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles {
                scrub_cursor: AppUiPreviewDecodeAccessModeProfile {
                    frames: 20,
                    in_process_cpu_rgba_frames: 20,
                    total_duration_us: 1_250_000,
                    max_duration_us: 70_000,
                    last_duration_us: 60_000,
                    latency_buckets: slow_buckets,
                    queue_wait_total_us: 1_200_000,
                    queue_wait_max_us: 70_000,
                    queue_wait_last_us: 60_000,
                    queue_wait_buckets: slow_buckets,
                    bounded_any_seek_strategy_frames: 20,
                    any_seek_window_ms_max: 1,
                    ..AppUiPreviewDecodeAccessModeProfile::default()
                },
                ..AppUiPreviewDecodeAccessModeProfiles::default()
            },
            ..AppUiPreviewDiagnostics::default()
        };

        let report = build_preview_decode_performance_report(
            diagnostics.decode_performance_summary(50_000),
            "preview-decode-p95-test",
            50_000,
        );

        assert_eq!(report.verdict, AppUiPreviewDecodePerformanceVerdict::Fail);
        assert!(report.checks.iter().any(|check| {
            check.code == "preview_decode_scrub_cursor_p95_frame_us"
                && check.severity == AppUiPreviewDecodePerformanceSeverity::Fail
                && check.observed == 80_000
        }));
        assert!(report.checks.iter().any(|check| {
            check.code == "preview_decode_scrub_cursor_queue_wait_p95_us"
                && check.severity == AppUiPreviewDecodePerformanceSeverity::Warn
                && check.observed == 80_000
        }));
        assert!(report.root_causes.iter().any(|root| {
            root.code == "preview_decode_access_mode_over_budget"
                && root.evidence.contains("p95_upper_bound_us=80000")
                && root.evidence.contains("latency_buckets=")
        }));
        assert!(report.root_causes.iter().any(|root| {
            root.code == "preview_decode_access_mode_queue_wait_bound"
                && root.evidence.contains("queue_wait_p95_upper_bound_us=80000")
                && root.evidence.contains("queue_wait_buckets=")
        }));
    }

    #[test]
    fn preview_decode_performance_report_fails_invalid_access_mode_admission() {
        let diagnostics = AppUiPreviewDiagnostics {
            decode_successes: 1,
            decode_in_process_cpu_rgba_frames: 1,
            decode_total_duration_us: 12_000,
            decode_max_duration_us: 12_000,
            decode_last_duration_us: 12_000,
            queue_invalid_access_mode_drops: 1,
            scheduler: MediaPreviewSchedulerDiagnostics {
                dropped_invalid_access_mode_requests: 2,
                ..MediaPreviewSchedulerDiagnostics::default()
            },
            ..AppUiPreviewDiagnostics::default()
        };

        let report = build_preview_decode_performance_report(
            diagnostics.decode_performance_summary(50_000),
            "preview-decode-invalid-access-mode-test",
            50_000,
        );

        assert_eq!(report.verdict, AppUiPreviewDecodePerformanceVerdict::Fail);
        assert!(report.checks.iter().any(|check| {
            check.code == "preview_decode_invalid_access_mode_requests"
                && check.severity == AppUiPreviewDecodePerformanceSeverity::Fail
                && check.observed == 2
                && check.limit == Some(0)
        }));
        assert!(report.checks.iter().any(|check| {
            check.code == "preview_decode_queue_invalid_access_mode_drops"
                && check.severity == AppUiPreviewDecodePerformanceSeverity::Fail
                && check.observed == 1
                && check.limit == Some(0)
        }));
        assert!(report.root_causes.iter().any(|root| {
            root.code == "preview_decode_invalid_access_mode_request"
                && root.area == AppUiPreviewDecodePerformanceArea::Scheduling
                && root.evidence.contains("dropped_invalid_access_mode_requests=2")
        }));
        assert!(report.root_causes.iter().any(|root| {
            root.code == "preview_decode_queue_invalid_access_mode_drop"
                && root.area == AppUiPreviewDecodePerformanceArea::Scheduling
                && root.evidence.contains("queue_invalid_access_mode_drops=1")
        }));
        assert!(report
            .actions
            .iter()
            .any(|action| action.code == "fix_preview_access_mode_admission"));
    }

    #[test]
    fn preview_decode_performance_report_fails_worker_transport_drops() {
        let diagnostics = AppUiPreviewDiagnostics {
            decode_successes: 1,
            decode_in_process_cpu_rgba_frames: 1,
            decode_total_duration_us: 12_000,
            decode_max_duration_us: 12_000,
            decode_last_duration_us: 12_000,
            enqueued_jobs: 3,
            queue_full_drops: 1,
            queue_evicted_prefetch_jobs: 1,
            interactive_cancel_requests: 1,
            interactive_cancel_scheduler_requests: 2,
            interactive_cancel_queued_jobs: 2,
            queue_canceled_jobs: 2,
            queue_pruned_obsolete_jobs: 2,
            queue_promoted_current_jobs: 1,
            worker_disconnected_drops: 1,
            decode_stage_durations: PreviewDecodeStageDurations {
                packet_decode_us: 10_000,
                swscale_us: 1_000,
                rgba_copy_us: 500,
                ..PreviewDecodeStageDurations::default()
            },
            decode_max_frame_stage_durations: PreviewDecodeStageDurations {
                packet_decode_us: 10_000,
                swscale_us: 1_000,
                rgba_copy_us: 500,
                ..PreviewDecodeStageDurations::default()
            },
            scheduler: MediaPreviewSchedulerDiagnostics {
                dropped_pending_window_requests: 2,
                ..MediaPreviewSchedulerDiagnostics::default()
            },
            ..AppUiPreviewDiagnostics::default()
        };

        let report = build_preview_decode_performance_report(
            diagnostics.decode_performance_summary(50_000),
            "preview-decode-worker-queue-test",
            50_000,
        );

        assert_eq!(report.verdict, AppUiPreviewDecodePerformanceVerdict::Fail);
        let summary = report.summary.expect("decode summary");
        assert_eq!(summary.enqueued_jobs, 3);
        assert_eq!(summary.queue_full_drops, 1);
        assert_eq!(summary.interactive_cancel_requests, 1);
        assert_eq!(summary.interactive_cancel_scheduler_requests, 2);
        assert_eq!(summary.interactive_cancel_queued_jobs, 2);
        assert_eq!(summary.queue_canceled_jobs, 2);
        assert_eq!(summary.worker_disconnected_drops, 1);
        assert!(report.checks.iter().any(|check| {
            check.code == "preview_decode_worker_queue_full_drops"
                && check.severity == AppUiPreviewDecodePerformanceSeverity::Fail
                && check.observed == 1
                && check.limit == Some(0)
        }));
        assert!(report.checks.iter().any(|check| {
            check.code == "preview_decode_worker_disconnected_drops"
                && check.severity == AppUiPreviewDecodePerformanceSeverity::Fail
                && check.observed == 1
                && check.limit == Some(0)
        }));
        assert!(report.root_causes.iter().any(|root| {
            root.code == "preview_decode_worker_queue_full_drops"
                && root.severity == AppUiPreviewDecodePerformanceSeverity::Fail
                && root.evidence.contains("queue_full_drops=1")
                && root.evidence.contains("interactive_cancel_requests=1")
                && root.evidence.contains("interactive_cancel_scheduler_requests=2")
                && root.evidence.contains("interactive_cancel_queued_jobs=2")
                && root.evidence.contains("queue_canceled_jobs=2")
                && root.evidence.contains("scheduler_dropped_pending_window_requests=2")
        }));
        assert!(report.root_causes.iter().any(|root| {
            root.code == "preview_decode_worker_disconnected_drops"
                && root.severity == AppUiPreviewDecodePerformanceSeverity::Fail
                && root.evidence.contains("worker_disconnected_drops=1")
        }));
        assert!(report
            .actions
            .iter()
            .any(|action| action.code == "reduce_preview_worker_transport_backpressure"));
        assert!(report
            .actions
            .iter()
            .any(|action| action.code == "restore_preview_worker_lifecycle"));
    }

    #[test]
    fn preview_decode_performance_report_keeps_queue_wait_evidence_without_successful_frame() {
        let diagnostics = AppUiPreviewDiagnostics {
            decode_canceled_jobs: 1,
            decode_canceled_obsolete_jobs: 1,
            decode_canceled_scrub_cursor_jobs: 1,
            decode_queue_wait_total_us: 75_000,
            decode_queue_wait_max_us: 75_000,
            decode_queue_wait_last_us: 75_000,
            decode_current_queue_wait_max_us: 75_000,
            decode_access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles {
                scrub_cursor: AppUiPreviewDecodeAccessModeProfile {
                    queue_wait_total_us: 75_000,
                    queue_wait_max_us: 75_000,
                    queue_wait_last_us: 75_000,
                    canceled_jobs: 1,
                    canceled_obsolete_jobs: 1,
                    ..AppUiPreviewDecodeAccessModeProfile::default()
                },
                ..AppUiPreviewDecodeAccessModeProfiles::default()
            },
            ..AppUiPreviewDiagnostics::default()
        };

        let report = build_preview_decode_performance_report(
            diagnostics.decode_performance_summary(50_000),
            "preview-decode-canceled-queue-wait-test",
            50_000,
        );

        assert_eq!(report.verdict, AppUiPreviewDecodePerformanceVerdict::Warn);
        assert!(report.checks.iter().any(|check| {
            check.code == "preview_decode_scrub_cursor_queue_wait_max_us"
                && check.severity == AppUiPreviewDecodePerformanceSeverity::Warn
                && check.observed == 75_000
                && check.limit == Some(50_000)
        }));
        assert!(!report
            .checks
            .iter()
            .any(|check| check.code == "preview_decode_scrub_cursor_max_frame_us"));
        assert!(report.root_causes.iter().any(|root| {
            root.code == "preview_decode_access_mode_queue_wait_bound"
                && root.evidence.contains("access_mode=ScrubCursor")
                && root.evidence.contains("frames=0")
        }));
        assert!(report.root_causes.iter().any(|root| {
            root.code == "preview_decode_obsolete_cancellations"
                && root.evidence.contains("scrub_obsolete_jobs=1")
                && root.evidence.contains("playback_obsolete_jobs=0")
        }));
    }

    #[test]
    fn preview_decode_performance_report_flags_slow_cancellation() {
        let diagnostics = AppUiPreviewDiagnostics {
            decode_canceled_jobs: 1,
            decode_canceled_obsolete_jobs: 1,
            decode_canceled_scrub_cursor_jobs: 1,
            decode_canceled_total_duration_us: 85_000,
            decode_canceled_max_duration_us: 85_000,
            decode_canceled_last_duration_us: 85_000,
            decode_access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles {
                scrub_cursor: AppUiPreviewDecodeAccessModeProfile {
                    canceled_jobs: 1,
                    canceled_obsolete_jobs: 1,
                    canceled_total_duration_us: 85_000,
                    canceled_max_duration_us: 85_000,
                    canceled_last_duration_us: 85_000,
                    ..AppUiPreviewDecodeAccessModeProfile::default()
                },
                ..AppUiPreviewDecodeAccessModeProfiles::default()
            },
            ..AppUiPreviewDiagnostics::default()
        };

        let report = build_preview_decode_performance_report(
            diagnostics.decode_performance_summary(50_000),
            "preview-decode-slow-cancel-test",
            50_000,
        );

        assert_eq!(report.verdict, AppUiPreviewDecodePerformanceVerdict::Warn);
        assert!(report.checks.iter().any(|check| {
            check.code == "preview_decode_canceled_max_frame_us"
                && check.severity == AppUiPreviewDecodePerformanceSeverity::Warn
                && check.observed == 85_000
                && check.limit == Some(50_000)
        }));
        assert!(report.root_causes.iter().any(|root| {
            root.code == "preview_decode_slow_cancellation"
                && root.evidence.contains("scrub_canceled_max_duration_us=85000")
        }));
        assert!(report
            .actions
            .iter()
            .any(|action| action.code == "inspect_preview_decode_cancellation_points"));
    }

    #[test]
    fn preview_decode_performance_report_flags_slow_cancel_return_latency() {
        let diagnostics = AppUiPreviewDiagnostics {
            decode_canceled_jobs: 1,
            decode_canceled_obsolete_jobs: 1,
            decode_canceled_random_access_still_jobs: 1,
            decode_canceled_total_duration_us: 90_000,
            decode_canceled_max_duration_us: 90_000,
            decode_canceled_last_duration_us: 90_000,
            decode_canceled_return_latency_total_us: 70_000,
            decode_canceled_return_latency_max_us: 70_000,
            decode_canceled_return_latency_last_us: 70_000,
            decode_access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles {
                random_access_still: AppUiPreviewDecodeAccessModeProfile {
                    canceled_jobs: 1,
                    canceled_obsolete_jobs: 1,
                    canceled_total_duration_us: 90_000,
                    canceled_max_duration_us: 90_000,
                    canceled_last_duration_us: 90_000,
                    canceled_return_latency_total_us: 70_000,
                    canceled_return_latency_max_us: 70_000,
                    canceled_return_latency_last_us: 70_000,
                    ..AppUiPreviewDecodeAccessModeProfile::default()
                },
                ..AppUiPreviewDecodeAccessModeProfiles::default()
            },
            ..AppUiPreviewDiagnostics::default()
        };

        let report = build_preview_decode_performance_report(
            diagnostics.decode_performance_summary(50_000),
            "preview-decode-cancel-return-test",
            50_000,
        );

        assert_eq!(report.verdict, AppUiPreviewDecodePerformanceVerdict::Warn);
        assert!(report.checks.iter().any(|check| {
            check.code == "preview_decode_cancel_return_latency_max_us"
                && check.severity == AppUiPreviewDecodePerformanceSeverity::Warn
                && check.observed == 70_000
                && check.limit == Some(50_000)
        }));
        assert!(report.root_causes.iter().any(|root| {
            root.code == "preview_decode_slow_cancel_return"
                && root.evidence.contains("random_access_still_cancel_return_latency_max_us=70000")
        }));
        assert!(report
            .actions
            .iter()
            .any(|action| action.code == "shorten_preview_decode_cancel_cleanup"));
    }

    #[test]
    fn preview_decode_performance_report_classifies_prefetch_deadline_cancellations() {
        let diagnostics = AppUiPreviewDiagnostics {
            decode_successes: 1,
            decode_canceled_jobs: 3,
            decode_canceled_prefetch_deadline_jobs: 2,
            decode_canceled_prefetch_preempted_jobs: 1,
            decode_canceled_playback_cursor_jobs: 3,
            decode_in_process_cpu_rgba_frames: 1,
            decode_total_duration_us: 12_000,
            decode_max_duration_us: 12_000,
            decode_last_duration_us: 12_000,
            worker_queue: MediaPreviewJobQueueDiagnostics {
                queued_current_jobs: 1,
                queued_prefetch_jobs: 1,
                ..MediaPreviewJobQueueDiagnostics::default()
            },
            decode_stage_durations: PreviewDecodeStageDurations {
                packet_decode_us: 10_000,
                swscale_us: 1_000,
                rgba_copy_us: 500,
                ..PreviewDecodeStageDurations::default()
            },
            decode_max_frame_stage_durations: PreviewDecodeStageDurations {
                packet_decode_us: 10_000,
                swscale_us: 1_000,
                rgba_copy_us: 500,
                ..PreviewDecodeStageDurations::default()
            },
            decode_access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles {
                playback_cursor: AppUiPreviewDecodeAccessModeProfile {
                    frames: 1,
                    in_process_cpu_rgba_frames: 1,
                    canceled_jobs: 3,
                    canceled_prefetch_deadline_jobs: 2,
                    canceled_prefetch_preempted_jobs: 1,
                    ..AppUiPreviewDecodeAccessModeProfile::default()
                },
                ..AppUiPreviewDecodeAccessModeProfiles::default()
            },
            ..AppUiPreviewDiagnostics::default()
        };

        let report = build_preview_decode_performance_report(
            diagnostics.decode_performance_summary(50_000),
            "preview-decode-cancel-test",
            50_000,
        );

        assert_eq!(report.verdict, AppUiPreviewDecodePerformanceVerdict::Warn);
        let summary = report.summary.expect("decode summary");
        assert_eq!(summary.canceled_jobs, 3);
        assert_eq!(summary.canceled_prefetch_deadline_jobs, 2);
        assert_eq!(summary.canceled_prefetch_preempted_jobs, 1);
        assert_eq!(summary.canceled_playback_cursor_jobs, 3);
        assert!(report.root_causes.iter().any(|root| {
            root.code == "preview_decode_prefetch_deadline_cancellations"
                && root.evidence.contains("playback_prefetch_deadline_jobs=2")
                && root.evidence.contains("scrub_prefetch_deadline_jobs=0")
        }));
        assert!(report.root_causes.iter().any(|root| {
            root.code == "preview_decode_prefetch_preempted_by_current"
                && root.evidence.contains("playback_prefetch_preempted_jobs=1")
                && root.evidence.contains("queued_current_jobs=1")
                && root.evidence.contains("queued_prefetch_jobs=1")
        }));
        assert!(report
            .actions
            .iter()
            .any(|action| action.code == "tune_preview_prefetch_deadline_or_proxy"));
        assert!(report
            .actions
            .iter()
            .any(|action| action.code == "reduce_speculative_prefetch_pressure"));
    }

    #[test]
    fn preview_decode_performance_report_classifies_playback_deadline_cancellations() {
        let diagnostics = AppUiPreviewDiagnostics {
            decode_canceled_jobs: 1,
            decode_canceled_playback_deadline_jobs: 1,
            decode_canceled_playback_cursor_jobs: 1,
            decode_access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles {
                playback_cursor: AppUiPreviewDecodeAccessModeProfile {
                    canceled_jobs: 1,
                    canceled_playback_deadline_jobs: 1,
                    queue_wait_max_us: 55_000,
                    max_duration_us: 0,
                    ..AppUiPreviewDecodeAccessModeProfile::default()
                },
                ..AppUiPreviewDecodeAccessModeProfiles::default()
            },
            playback_schedule: AppUiPreviewPlaybackScheduleDiagnostics {
                current_decode_decisions: 1,
                current_drop_late_decisions: 1,
                current_proxy_or_hardware_recommended_decisions: 1,
                ..AppUiPreviewPlaybackScheduleDiagnostics::default()
            },
            ..AppUiPreviewDiagnostics::default()
        };

        let report = build_preview_decode_performance_report(
            diagnostics.decode_performance_summary(50_000),
            "preview-decode-playback-deadline-test",
            50_000,
        );

        assert_eq!(report.verdict, AppUiPreviewDecodePerformanceVerdict::Warn);
        let summary = report.summary.expect("decode summary");
        assert_eq!(summary.canceled_playback_deadline_jobs, 1);
        assert!(report.root_causes.iter().any(|root| {
            root.code == "preview_decode_playback_deadline_cancellations"
                && root.evidence.contains("canceled_playback_deadline_jobs=1")
                && root.evidence.contains("playback_cursor_deadline_jobs=1")
                && root.evidence.contains("current_decode_decisions=1")
                && root.evidence.contains("current_drop_late_decisions=1")
                && root.evidence.contains("current_proxy_or_hardware_recommended_decisions=1")
        }));
        assert!(report.actions.iter().any(|action| {
            action.code == "drop_late_playback_frames_or_use_proxy_hardware_decode"
        }));
    }

    #[test]
    fn preview_decode_performance_report_classifies_still_preemptions() {
        let diagnostics = AppUiPreviewDiagnostics {
            decode_successes: 1,
            decode_canceled_jobs: 1,
            decode_canceled_still_preempted_jobs: 1,
            decode_canceled_random_access_still_jobs: 1,
            decode_in_process_cpu_rgba_frames: 1,
            decode_total_duration_us: 18_000,
            decode_max_duration_us: 18_000,
            decode_last_duration_us: 18_000,
            worker_queue: MediaPreviewJobQueueDiagnostics {
                queued_current_jobs: 2,
                queued_scrub_cursor_jobs: 1,
                queued_playback_cursor_jobs: 1,
                queued_random_access_still_jobs: 1,
                ..MediaPreviewJobQueueDiagnostics::default()
            },
            decode_access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles {
                random_access_still: AppUiPreviewDecodeAccessModeProfile {
                    frames: 1,
                    in_process_cpu_rgba_frames: 1,
                    canceled_jobs: 1,
                    canceled_still_preempted_jobs: 1,
                    ..AppUiPreviewDecodeAccessModeProfile::default()
                },
                ..AppUiPreviewDecodeAccessModeProfiles::default()
            },
            ..AppUiPreviewDiagnostics::default()
        };

        let report = build_preview_decode_performance_report(
            diagnostics.decode_performance_summary(50_000),
            "preview-decode-still-preempt-test",
            50_000,
        );

        assert_eq!(report.verdict, AppUiPreviewDecodePerformanceVerdict::Warn);
        let summary = report.summary.expect("decode summary");
        assert_eq!(summary.canceled_still_preempted_jobs, 1);
        assert_eq!(summary.canceled_random_access_still_jobs, 1);
        assert!(report.checks.iter().any(|check| {
            check.code == "preview_decode_still_preempted_by_realtime_cancellations"
                && check.severity == AppUiPreviewDecodePerformanceSeverity::Warn
                && check.observed == 1
                && check.limit == Some(0)
        }));
        assert!(report.root_causes.iter().any(|root| {
            root.code == "preview_decode_still_preempted_by_realtime_current"
                && root.evidence.contains("random_access_still_preempted_jobs=1")
                && root.evidence.contains("queued_scrub_cursor_jobs=1")
                && root.evidence.contains("queued_playback_cursor_jobs=1")
        }));
        assert!(report
            .actions
            .iter()
            .any(|action| action.code == "preempt_still_decode_for_realtime_preview"));
    }

    #[test]
    fn preview_decode_performance_report_breaks_down_unknown_cancellations_by_access_mode() {
        let diagnostics = AppUiPreviewDiagnostics {
            decode_canceled_jobs: 1,
            decode_canceled_unknown_jobs: 1,
            decode_canceled_random_access_still_jobs: 1,
            decode_access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles {
                random_access_still: AppUiPreviewDecodeAccessModeProfile {
                    canceled_jobs: 1,
                    canceled_unknown_jobs: 1,
                    ..AppUiPreviewDecodeAccessModeProfile::default()
                },
                ..AppUiPreviewDecodeAccessModeProfiles::default()
            },
            ..AppUiPreviewDiagnostics::default()
        };

        let report = build_preview_decode_performance_report(
            diagnostics.decode_performance_summary(50_000),
            "preview-decode-unknown-cancel-test",
            50_000,
        );

        assert_eq!(report.verdict, AppUiPreviewDecodePerformanceVerdict::Warn);
        assert!(report.root_causes.iter().any(|root| {
            root.code == "preview_decode_unknown_cancellations"
                && root.evidence.contains("random_access_still_unknown_jobs=1")
                && root.evidence.contains("playback_unknown_jobs=0")
                && root.evidence.contains("scrub_unknown_jobs=0")
        }));
    }

    #[test]
    fn preview_decode_performance_report_flags_playback_without_locality() {
        let diagnostics = AppUiPreviewDiagnostics {
            decode_successes: 2,
            decode_in_process_cpu_rgba_frames: 2,
            decode_total_duration_us: 80_000,
            decode_max_duration_us: 45_000,
            decode_last_duration_us: 35_000,
            decode_seeked_frames: 2,
            decode_decoded_frame_count: 96,
            decode_max_decoded_frame_count: 48,
            decode_stage_durations: PreviewDecodeStageDurations {
                seek_us: 20_000,
                packet_decode_us: 55_000,
                ..PreviewDecodeStageDurations::default()
            },
            decode_max_frame_stage_durations: PreviewDecodeStageDurations {
                seek_us: 10_000,
                packet_decode_us: 30_000,
                ..PreviewDecodeStageDurations::default()
            },
            decode_access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles {
                playback_cursor: AppUiPreviewDecodeAccessModeProfile {
                    frames: 2,
                    in_process_cpu_rgba_frames: 2,
                    total_duration_us: 80_000,
                    max_duration_us: 45_000,
                    last_duration_us: 35_000,
                    seeked_frames: 2,
                    session_opened_frames: 2,
                    session_reused_frames: 0,
                    forward_reused_frames: 0,
                    decoded_frame_count: 96,
                    max_decoded_frame_count: 48,
                    stage_durations: PreviewDecodeStageDurations {
                        seek_us: 20_000,
                        packet_decode_us: 55_000,
                        ..PreviewDecodeStageDurations::default()
                    },
                    max_frame_stage_durations: PreviewDecodeStageDurations {
                        seek_us: 10_000,
                        packet_decode_us: 30_000,
                        ..PreviewDecodeStageDurations::default()
                    },
                    ..AppUiPreviewDecodeAccessModeProfile::default()
                },
                ..AppUiPreviewDecodeAccessModeProfiles::default()
            },
            ..AppUiPreviewDiagnostics::default()
        };

        let report = build_preview_decode_performance_report(
            diagnostics.decode_performance_summary(50_000),
            "preview-playback-locality-test",
            50_000,
        );

        assert!(report.root_causes.iter().any(|root| {
            root.code == "preview_decode_playback_session_not_reused"
                && root.evidence.contains("session_opened_frames=2")
        }));
        assert!(report.root_causes.iter().any(|root| {
            root.code == "preview_decode_playback_without_locality"
                && root.evidence.contains("source_decode_frames=2")
                && root.evidence.contains("forward_reused_frames=0")
        }));
        assert!(report
            .actions
            .iter()
            .any(|action| action.code == "improve_playback_decoder_residency"));
    }

    #[test]
    fn preview_render_performance_report_classifies_output_boundary_bound_slow_frame() {
        let diagnostics = AppUiPreviewDiagnostics {
            render_timed_frames: 1,
            render_total_duration_us: 120_000,
            render_max_duration_us: 120_000,
            render_last_duration_us: 120_000,
            render_stage_durations: AppUiPreviewRenderStageDurations {
                resolve_us: 2_000,
                final_cache_lookup_us: 100,
                working_prepare_us: 7_000,
                cpu_composite_us: 20_000,
                cpu_output_boundary_us: 90_000,
                frame_packaging_us: 900,
            },
            render_max_frame_stage_durations: AppUiPreviewRenderStageDurations {
                resolve_us: 2_000,
                final_cache_lookup_us: 100,
                working_prepare_us: 7_000,
                cpu_composite_us: 20_000,
                cpu_output_boundary_us: 90_000,
                frame_packaging_us: 900,
            },
            ..AppUiPreviewDiagnostics::default()
        };

        let report = build_preview_render_performance_report(
            diagnostics.render_performance_summary(50_000),
            "preview-render-test",
            50_000,
        );

        assert_eq!(report.verdict, AppUiPreviewRenderPerformanceVerdict::Fail);
        let summary = report.summary.expect("render summary");
        assert_eq!(
            summary.primary_bottleneck,
            AppUiPreviewRenderBottleneck::CpuOutputBoundary
        );
        assert!(report
            .root_causes
            .iter()
            .any(|root| root.code == "preview_render_frame_over_budget"));
        assert!(report
            .root_causes
            .iter()
            .any(|root| root.code == "preview_render_cpu_output_boundary_bound"));
        assert!(report
            .actions
            .iter()
            .any(|action| action.code == "move_preview_output_boundary_to_gpu"));
    }

    #[test]
    fn preview_performance_reports_classify_slowest_frame_not_aggregate_total() {
        let decode_diagnostics = AppUiPreviewDiagnostics {
            decode_successes: 2,
            decode_in_process_cpu_rgba_frames: 2,
            decode_total_duration_us: 160_000,
            decode_max_duration_us: 120_000,
            decode_last_duration_us: 40_000,
            decode_decoded_frame_count: 40,
            decode_max_decoded_frame_count: 36,
            decode_stage_durations: PreviewDecodeStageDurations {
                packet_decode_us: 30_000,
                swscale_us: 200_000,
                ..PreviewDecodeStageDurations::default()
            },
            decode_max_frame_stage_durations: PreviewDecodeStageDurations {
                packet_decode_us: 95_000,
                swscale_us: 10_000,
                rgba_copy_us: 2_000,
                ..PreviewDecodeStageDurations::default()
            },
            ..AppUiPreviewDiagnostics::default()
        };
        let decode_report = build_preview_decode_performance_report(
            decode_diagnostics.decode_performance_summary(50_000),
            "preview-decode-max-frame-test",
            50_000,
        );

        assert_eq!(
            decode_report.summary.expect("decode summary").primary_bottleneck,
            AppUiPreviewDecodeBottleneck::PacketDecode
        );
        assert!(decode_report
            .root_causes
            .iter()
            .any(|root| root.evidence.contains("packet_decode_us=95000")));

        let render_diagnostics = AppUiPreviewDiagnostics {
            render_timed_frames: 2,
            render_total_duration_us: 160_000,
            render_max_duration_us: 120_000,
            render_last_duration_us: 40_000,
            render_stage_durations: AppUiPreviewRenderStageDurations {
                cpu_composite_us: 200_000,
                cpu_output_boundary_us: 30_000,
                ..AppUiPreviewRenderStageDurations::default()
            },
            render_max_frame_stage_durations: AppUiPreviewRenderStageDurations {
                cpu_composite_us: 20_000,
                cpu_output_boundary_us: 90_000,
                ..AppUiPreviewRenderStageDurations::default()
            },
            ..AppUiPreviewDiagnostics::default()
        };
        let render_report = build_preview_render_performance_report(
            render_diagnostics.render_performance_summary(50_000),
            "preview-render-max-frame-test",
            50_000,
        );

        assert_eq!(
            render_report.summary.expect("render summary").primary_bottleneck,
            AppUiPreviewRenderBottleneck::CpuOutputBoundary
        );
        assert!(render_report
            .root_causes
            .iter()
            .any(|root| root.evidence.contains("cpu_output_boundary_us=90000")));
    }

    #[test]
    fn preview_decode_bottleneck_uses_queue_wait_from_same_slowest_frame() {
        let diagnostics = AppUiPreviewDiagnostics {
            decode_successes: 1,
            decode_in_process_cpu_rgba_frames: 1,
            decode_total_duration_us: 120_000,
            decode_max_duration_us: 120_000,
            decode_last_duration_us: 120_000,
            decode_queue_wait_total_us: 201_000,
            decode_queue_wait_max_us: 200_000,
            decode_queue_wait_last_us: 200_000,
            decode_current_queue_wait_max_us: 200_000,
            decode_stage_durations: PreviewDecodeStageDurations {
                packet_decode_us: 95_000,
                ..PreviewDecodeStageDurations::default()
            },
            decode_max_frame_stage_durations: PreviewDecodeStageDurations {
                packet_decode_us: 95_000,
                ..PreviewDecodeStageDurations::default()
            },
            decode_max_frame_queue_wait_us: 1_000,
            decode_access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles {
                scrub_cursor: AppUiPreviewDecodeAccessModeProfile {
                    frames: 1,
                    in_process_cpu_rgba_frames: 1,
                    total_duration_us: 120_000,
                    max_duration_us: 120_000,
                    last_duration_us: 120_000,
                    queue_wait_total_us: 201_000,
                    queue_wait_max_us: 200_000,
                    bounded_any_seek_strategy_frames: 1,
                    any_seek_window_ms_max: 1,
                    stage_durations: PreviewDecodeStageDurations {
                        packet_decode_us: 95_000,
                        ..PreviewDecodeStageDurations::default()
                    },
                    max_frame_stage_durations: PreviewDecodeStageDurations {
                        packet_decode_us: 95_000,
                        ..PreviewDecodeStageDurations::default()
                    },
                    max_frame_queue_wait_us: 1_000,
                    max_frame_bottleneck: AppUiPreviewDecodeBottleneck::PacketDecode,
                    ..AppUiPreviewDecodeAccessModeProfile::default()
                },
                ..AppUiPreviewDecodeAccessModeProfiles::default()
            },
            ..AppUiPreviewDiagnostics::default()
        };

        let report = build_preview_decode_performance_report(
            diagnostics.decode_performance_summary(50_000),
            "preview-decode-same-frame-bottleneck-test",
            50_000,
        );

        let summary = report.summary.expect("decode summary");
        assert_eq!(
            summary.primary_bottleneck,
            AppUiPreviewDecodeBottleneck::PacketDecode
        );
        assert_eq!(summary.queue_wait_max_us, 200_000);
        assert_eq!(summary.max_frame_queue_wait_us, 1_000);
        assert!(report.root_causes.iter().any(|root| {
            root.code == "preview_decode_frame_over_budget"
                && root.evidence.contains("max_frame_queue_wait_us=1000")
                && root.evidence.contains("primary_bottleneck=PacketDecode")
        }));
        assert!(report
            .root_causes
            .iter()
            .any(|root| root.code == "preview_decode_access_mode_queue_wait_bound"));
    }

    #[test]
    fn preview_diagnostics_count_input_color_resolution_sources() {
        let service = AppUiPreviewService::new();

        service.record_input_color_resolution(InputColorResolutionSource::Override);
        service.record_input_color_resolution(InputColorResolutionSource::DataTexture);
        service.record_input_color_resolution(InputColorResolutionSource::DetectedMetadata);
        service
            .record_input_color_resolution(InputColorResolutionSource::MissingPolicyAssumeRec709);
        service.record_input_color_resolution(
            InputColorResolutionSource::MissingPolicyAssumeSequenceWorkingSpace,
        );
        service.record_input_color_resolution(InputColorResolutionSource::MissingPolicyRejectMedia);
        service.record_input_color_resolution(InputColorResolutionSource::DetectedMetadata);

        let diagnostics = service.diagnostics();
        assert_eq!(diagnostics.input_color_resolution_override, 1);
        assert_eq!(diagnostics.input_color_resolution_data_texture, 1);
        assert_eq!(diagnostics.input_color_resolution_detected_metadata, 2);
        assert_eq!(diagnostics.input_color_resolution_missing_assume_rec709, 1);
        assert_eq!(diagnostics.input_color_resolution_missing_assume_working, 1);
        assert_eq!(diagnostics.input_color_resolution_missing_rejected, 1);
    }

    #[test]
    fn preview_diagnostics_derives_composite_color_path_summary() {
        let diagnostics = AppUiPreviewDiagnostics {
            color_composite_elements: 5,
            color_composite_float_linear: 2,
            color_composite_legacy_rgba8: 1,
            color_composite_legacy_media_transform: 1,
            color_composite_legacy_adjustment_effect: 2,
            ..AppUiPreviewDiagnostics::default()
        };

        let summary = diagnostics.composite_color_path_summary();

        assert_eq!(summary.path, TimelineCompositeColorPath::LegacyRgba8);
        assert_eq!(summary.elements, 5);
        assert_eq!(summary.composite_plans(), 3);
        assert_eq!(summary.legacy_breakdown.media_transform, 1);
        assert_eq!(summary.legacy_breakdown.adjustment_effect, 2);
        assert_eq!(summary.legacy_breakdown.total(), 3);
    }

    #[test]
    fn preview_diagnostics_derives_color_health_summary() {
        assert_eq!(
            AppUiPreviewDiagnostics::default().color_health_summary(),
            None
        );

        let diagnostics = AppUiPreviewDiagnostics {
            input_color_resolution_override: 2,
            input_color_resolution_data_texture: 3,
            input_color_resolution_detected_metadata: 5,
            input_color_resolution_missing_assume_rec709: 7,
            input_color_resolution_missing_rejected: 11,
            color_stage_total_stages: 4,
            color_stage_cpu_input_stages: 1,
            color_stage_cpu_output_stages: 1,
            color_stage_gpu_color_stages: 2,
            color_stage_upload_stages: 1,
            color_stage_gpu_blockers: 2,
            color_stage_gpu_shader_module_blockers: 1,
            color_stage_gpu_render_pipeline_blockers: 1,
            color_stage_pixels: 128,
            color_rgba8_boundary_calls: 1,
            color_composite_plans: 3,
            color_composite_elements: 9,
            color_composite_float_linear: 2,
            color_composite_legacy_rgba8: 1,
            color_composite_legacy_media_transform: 1,
            ..AppUiPreviewDiagnostics::default()
        };

        let summary = diagnostics.color_health_summary().expect("preview color health");

        assert_eq!(summary.composite_plans, 3);
        assert_eq!(summary.detected_metadata, 5);
        assert_eq!(summary.override_count, 2);
        assert_eq!(summary.policy_assumptions, 7);
        assert_eq!(summary.data_textures, 3);
        assert_eq!(summary.policy_rejections, 11);
        assert_eq!(summary.explicit_metadata_or_override, 7);
        assert_eq!(summary.cpu_input_stages, 1);
        assert_eq!(summary.cpu_output_stages, 1);
        assert_eq!(summary.gpu_color_stages, 2);
        assert_eq!(summary.gpu_blockers, 2);
        assert_eq!(summary.gpu_blocker_breakdown.shader_module_not_prepared, 1);
        assert_eq!(
            summary.gpu_blocker_breakdown.render_pipeline_not_prepared,
            1
        );
        assert_eq!(summary.transfer_stages, 1);
        assert_eq!(summary.rgba8_boundary_calls, 1);
        assert_eq!(summary.float_linear_composites, 2);
        assert_eq!(summary.legacy_rgba8_composites, 1);
        assert_eq!(summary.legacy_reason_total, 1);
        assert_eq!(summary.legacy_breakdown.media_transform, 1);
        assert!(!summary.fully_float_linear);
        assert!(!summary.gpu_path_ready);
        assert_eq!(summary.cpu_output_fallback_frames, 0);
        assert_eq!(summary.cpu_output_fallback_pixels, 0);
        assert!(summary.preview_gpu_output_blocker_breakdown.is_empty());
    }

    fn assert_preview_export_color_health_match(
        preview: AppUiPreviewColorHealthSummary,
        export: mondrian_export::queue::ExportJobColorDiagnosticsSummary,
    ) {
        assert_eq!(export.diagnosed_frames, 1);
        assert_eq!(preview.detected_metadata, export.detected_metadata);
        assert_eq!(preview.override_count, export.override_count);
        assert_eq!(preview.policy_assumptions, export.policy_assumptions);
        assert_eq!(preview.data_textures, export.data_textures);
        assert_eq!(preview.policy_rejections, export.policy_rejections);
        assert_eq!(
            preview.explicit_metadata_or_override,
            export.explicit_metadata_or_override
        );
        assert_eq!(preview.cpu_input_stages, export.cpu_input_stages);
        assert_eq!(preview.cpu_output_stages, export.cpu_output_stages);
        assert_eq!(preview.gpu_color_stages, export.gpu_color_stages);
        assert_eq!(preview.gpu_blockers, export.gpu_blockers);
        assert_eq!(preview.gpu_blocker_breakdown, export.gpu_blocker_breakdown);
        assert_eq!(preview.transfer_stages, export.transfer_stages);
        assert_eq!(
            preview.float_linear_composites,
            export.float_linear_composites
        );
        assert_eq!(
            preview.legacy_rgba8_composites,
            export.legacy_rgba8_composites
        );
        assert_eq!(preview.legacy_reason_total, export.legacy_reason_total);
        assert_eq!(preview.legacy_breakdown, export.legacy_breakdown);
        assert_eq!(preview.fully_float_linear, export.fully_float_linear);
        assert_eq!(preview.gpu_path_ready, export.gpu_path_ready);
    }

    fn assert_preview_export_color_reports_match(
        preview: &AppUiPreviewColorHealthReport,
        export: &mondrian_export::queue::ExportColorHealthReport,
    ) {
        assert_eq!(
            preview_color_report_verdict(preview.verdict),
            export_color_report_verdict(export.verdict)
        );
        assert_eq!(
            preview_shared_check_signature(preview),
            export_shared_check_signature(export)
        );
        assert_eq!(
            preview_root_cause_signature(preview),
            export_root_cause_signature(export)
        );
        assert_eq!(
            preview_action_signature(preview),
            export_action_signature(export)
        );
    }

    fn preview_color_report_verdict(verdict: AppUiPreviewColorHealthVerdict) -> &'static str {
        match verdict {
            AppUiPreviewColorHealthVerdict::Pass => "pass",
            AppUiPreviewColorHealthVerdict::Warn => "warn",
            AppUiPreviewColorHealthVerdict::Fail => "fail",
        }
    }

    fn export_color_report_verdict(
        verdict: mondrian_export::queue::ExportColorHealthVerdict,
    ) -> &'static str {
        match verdict {
            mondrian_export::queue::ExportColorHealthVerdict::Pass => "pass",
            mondrian_export::queue::ExportColorHealthVerdict::Warn => "warn",
            mondrian_export::queue::ExportColorHealthVerdict::Fail => "fail",
        }
    }

    fn preview_shared_check_signature(
        report: &AppUiPreviewColorHealthReport,
    ) -> Vec<(String, String, String, u64, Option<u64>)> {
        let mut signature = report
            .checks
            .iter()
            .filter(|check| is_shared_color_report_check(check.code))
            .map(|check| {
                (
                    format!("{:?}", check.area),
                    check.code.to_owned(),
                    preview_color_report_severity(check.severity).to_owned(),
                    check.observed,
                    check.limit,
                )
            })
            .collect::<Vec<_>>();
        signature.sort();
        signature
    }

    fn export_shared_check_signature(
        report: &mondrian_export::queue::ExportColorHealthReport,
    ) -> Vec<(String, String, String, u64, Option<u64>)> {
        let mut signature = report
            .checks
            .iter()
            .filter(|check| is_shared_color_report_check(check.code))
            .map(|check| {
                (
                    format!("{:?}", check.area),
                    check.code.to_owned(),
                    export_color_report_severity(check.severity).to_owned(),
                    check.observed,
                    check.limit,
                )
            })
            .collect::<Vec<_>>();
        signature.sort();
        signature
    }

    fn is_shared_color_report_check(code: &str) -> bool {
        matches!(
            code,
            "fully_float_linear"
                | "gpu_path_ready"
                | "gpu_blockers"
                | "transfer_stages"
                | "legacy_reason_total"
                | "policy_rejections"
        )
    }

    fn preview_root_cause_signature(
        report: &AppUiPreviewColorHealthReport,
    ) -> Vec<(String, String, String, String)> {
        let mut signature = report
            .root_causes
            .iter()
            .map(|root| {
                (
                    format!("{:?}", root.area),
                    normalized_color_root_cause_code(root.code).to_owned(),
                    preview_color_report_severity(root.severity).to_owned(),
                    root.evidence.clone(),
                )
            })
            .collect::<Vec<_>>();
        signature.sort();
        signature
    }

    fn export_root_cause_signature(
        report: &mondrian_export::queue::ExportColorHealthReport,
    ) -> Vec<(String, String, String, String)> {
        let mut signature = report
            .root_causes
            .iter()
            .filter(|root| root.code != "asset_color_diagnostics_warning")
            .map(|root| {
                (
                    format!("{:?}", root.area),
                    normalized_color_root_cause_code(root.code).to_owned(),
                    export_color_report_severity(root.severity).to_owned(),
                    root.evidence.clone(),
                )
            })
            .collect::<Vec<_>>();
        signature.sort();
        signature
    }

    fn preview_action_signature(report: &AppUiPreviewColorHealthReport) -> Vec<(String, String)> {
        let mut signature = report
            .actions
            .iter()
            .map(|action| {
                (
                    format!("{:?}", action.area),
                    normalized_color_action_code(action.code).to_owned(),
                )
            })
            .collect::<Vec<_>>();
        signature.sort();
        signature
    }

    fn export_action_signature(
        report: &mondrian_export::queue::ExportColorHealthReport,
    ) -> Vec<(String, String)> {
        let mut signature = report
            .actions
            .iter()
            .filter(|action| action.code != "inspect_asset_color_warning_evidence")
            .map(|action| {
                (
                    format!("{:?}", action.area),
                    normalized_color_action_code(action.code).to_owned(),
                )
            })
            .collect::<Vec<_>>();
        signature.sort();
        signature
    }

    fn normalized_color_root_cause_code(code: &str) -> &str {
        match code {
            "missing_preview_color_evidence" | "missing_export_color_evidence" => {
                "missing_color_evidence"
            }
            "preview_gpu_color_stage_blocked" | "export_gpu_color_stage_blocked" => {
                "gpu_color_stage_blocked"
            }
            "preview_transfer_stage_present" | "export_transfer_stage_present" => {
                "transfer_stage_present"
            }
            "legacy_rgba8_composite_path" => "legacy_rgba8_composite_path",
            "input_color_policy_rejected_source" => "input_color_policy_rejected_source",
            other => other,
        }
    }

    fn normalized_color_action_code(code: &str) -> &str {
        match code {
            "inspect_preview_diagnostics" | "inspect_export_render_path" => {
                "inspect_color_evidence"
            }
            "inspect_preview_asset_color_diagnostics" | "inspect_asset_color_diagnostics" => {
                "inspect_asset_color_diagnostics"
            }
            "inspect_preview_gpu_blockers" | "inspect_export_gpu_blockers" => {
                "inspect_gpu_blockers"
            }
            "remove_preview_transfer_stage" | "remove_export_transfer_stage" => {
                "remove_transfer_stage"
            }
            "migrate_preview_legacy_composite_reason" | "migrate_legacy_composite_reason" => {
                "migrate_legacy_composite_reason"
            }
            other => other,
        }
    }

    fn preview_color_report_severity(severity: AppUiPreviewColorHealthSeverity) -> &'static str {
        match severity {
            AppUiPreviewColorHealthSeverity::Pass => "pass",
            AppUiPreviewColorHealthSeverity::Warn => "warn",
            AppUiPreviewColorHealthSeverity::Fail => "fail",
        }
    }

    fn export_color_report_severity(
        severity: mondrian_export::queue::ExportColorHealthSeverity,
    ) -> &'static str {
        match severity {
            mondrian_export::queue::ExportColorHealthSeverity::Pass => "pass",
            mondrian_export::queue::ExportColorHealthSeverity::Warn => "warn",
            mondrian_export::queue::ExportColorHealthSeverity::Fail => "fail",
        }
    }

    fn preview_asset_issue_summary_for_sequence(
        sequence: &Sequence,
        nested_sequences: &[Sequence],
        asset_color_diagnostics: &HashMap<AssetId, VideoColorDiagnostic>,
        depth: usize,
        asset_ids: &mut std::collections::HashSet<AssetId>,
    ) {
        if depth > mondrian_timeline::sequence::MAX_NESTED_SEQUENCE_RENDER_DEPTH {
            return;
        }

        for track in &sequence.video_tracks {
            for clip in &track.clips {
                if clip.is_disabled {
                    continue;
                }
                if clip.is_nested_sequence() {
                    let Some(nested_sequence_id) = clip.nested_sequence_id else {
                        continue;
                    };
                    let Some(nested) =
                        nested_sequences.iter().find(|sequence| sequence.id == nested_sequence_id)
                    else {
                        continue;
                    };
                    preview_asset_issue_summary_for_sequence(
                        nested,
                        nested_sequences,
                        asset_color_diagnostics,
                        depth + 1,
                        asset_ids,
                    );
                    continue;
                }
                if asset_color_diagnostics.contains_key(&clip.asset_id) {
                    asset_ids.insert(clip.asset_id);
                }
            }
        }
    }

    #[test]
    fn preview_dimensions_clamp_invalid_resolution_scale() {
        let mut below_min = Sequence::new("below");
        below_min.settings.preview.resolution_scale = 0.0;
        assert_eq!(preview_dimensions_for_sequence(&below_min), (240, 135));

        let mut above_max = Sequence::new("above");
        above_max.settings.preview.resolution_scale = 2.0;
        assert_eq!(preview_dimensions_for_sequence(&above_max), (1920, 1080));

        let mut invalid = Sequence::new("invalid");
        invalid.settings.preview.resolution_scale = f32::NAN;
        assert_eq!(preview_dimensions_for_sequence(&invalid), (960, 540));
    }

    #[test]
    fn nested_solid_color_sequence_returns_preview_frame() {
        let mut state = AppState::new();
        let mut child = Sequence::new("child");
        let child_id = child.id;
        let child_tb = child.time_base();
        child.video_tracks[0]
            .add_clip(Clip::new_solid_color(
                AssetId::new(),
                Color::from_rgba8(48, 120, 220, 255),
                TimeCode::new(0, child_tb),
                TimeCode::new(24, child_tb),
            ))
            .expect("child solid clip");

        let mut parent = Sequence::new("parent");
        let parent_tb = parent.time_base();
        parent.video_tracks[0]
            .add_clip(Clip::new_nested_sequence(
                child_id,
                TimeCode::new(0, parent_tb),
                TimeCode::new(24, parent_tb),
                Some("child".to_owned()),
            ))
            .expect("parent nested clip");

        state.sequences.push(child);
        state.sequence = Some(parent);
        state.seek(3);

        let service = AppUiPreviewService::new();
        let frame = service.viewer_preview_for_state(&state);
        let frame = ready_frame(frame);

        assert_eq!(frame.width, 960);
        assert_eq!(frame.height, 540);
        assert_eq!(frame.rgba.len(), 960 * 540 * 4);
    }

    #[test]
    fn unsupported_media_plan_returns_no_partial_preview() {
        let mut state = AppState::new();
        let mut sequence = Sequence::new("media");
        let tb = sequence.time_base();
        sequence.video_tracks[0]
            .add_clip(Clip::new(
                AssetId::new(),
                TimeCode::new(0, tb),
                TimeCode::new(24, tb),
            ))
            .expect("media clip should be insertable");
        state.sequence = Some(sequence);

        let service = AppUiPreviewService::new();

        assert!(matches!(
            service.viewer_preview_for_state(&state),
            ViewerPreviewState::Unavailable
        ));
    }

    #[test]
    fn repeated_same_viewer_request_does_not_obsolete_in_flight_decode() {
        let mut state = AppState::new();
        let mut sequence = Sequence::new("media");
        let tb = sequence.time_base();
        sequence.video_tracks[0]
            .add_clip(Clip::new(
                AssetId::new(),
                TimeCode::new(0, tb),
                TimeCode::new(24, tb),
            ))
            .expect("media clip should be insertable");
        state.sequence = Some(sequence);
        state.seek(3);

        let service = AppUiPreviewService::new();
        let _ = service.viewer_preview_for_state(&state);
        let first_generation = service.diagnostics().scheduler.latest_generation;
        let _ = service.viewer_preview_for_state(&state);
        let second_generation = service.diagnostics().scheduler.latest_generation;

        assert_eq!(first_generation, second_generation);

        state.seek(4);
        let _ = service.viewer_preview_for_state(&state);
        let third_generation = service.diagnostics().scheduler.latest_generation;

        assert!(third_generation > second_generation);
    }

    #[test]
    fn preview_raster_key_changes_when_render_plan_changes() {
        let service = AppUiPreviewService::new();
        let first = service.viewer_preview_for_state(&state_with_solid_color_clip(
            Color::from_rgba8(255, 0, 0, 255),
        ));
        let first = ready_frame(first);
        let second = service.viewer_preview_for_state(&state_with_solid_color_clip(
            Color::from_rgba8(0, 0, 255, 255),
        ));
        let second = ready_frame(second);

        assert_ne!(first.key, second.key);
    }

    #[test]
    fn deterministic_solid_preview_reuses_raster_key_across_frames() {
        let service = AppUiPreviewService::new();
        let mut state = state_with_solid_color_clip(Color::from_rgba8(255, 128, 0, 255));

        let first = ready_frame(service.viewer_preview_for_state(&state));
        state.seek(5);
        let second = ready_frame(service.viewer_preview_for_state(&state));

        assert_eq!(first.rgba, second.rgba);
        assert_eq!(first.key, second.key);
        let diagnostics = service.diagnostics();
        assert_eq!(diagnostics.render_requests, 2);
        assert_eq!(diagnostics.ready_frames, 2);
        assert!(diagnostics.viewer_frame_cache_entries >= 1);
        assert!(diagnostics.color_output_transform_calls >= 1);
        assert_eq!(
            diagnostics.color_output_transform_pixels,
            diagnostics.color_output_transform_calls * 960_u64 * 540
        );
        assert_eq!(
            diagnostics.color_stage_plans,
            diagnostics.color_output_transform_calls
        );
        assert_eq!(
            diagnostics.color_stage_total_stages,
            diagnostics.color_output_transform_calls
        );
        assert_eq!(
            diagnostics.color_stage_cpu_output_stages,
            diagnostics.color_output_transform_calls
        );
        assert_eq!(
            diagnostics.color_stage_pixels,
            diagnostics.color_output_transform_calls * 960_u64 * 540
        );
        assert_eq!(
            diagnostics.color_composite_plans,
            diagnostics.color_output_transform_calls
        );
        assert_eq!(
            diagnostics.color_composite_float_linear,
            diagnostics.color_composite_plans
        );
        assert_eq!(diagnostics.color_composite_legacy_rgba8, 0);
    }

    #[test]
    fn resolved_media_preview_cache_key_includes_media_frame_signature() {
        let effect_graph = get_or_compile_scheduled_effect_graph(&EffectRenderPlan::default())
            .expect("default effect graph");
        let make_plan = |signature| {
            vec![ResolvedPreviewElement::Media {
                frame: test_media_frame_with_size(0, 2, 2, signature),
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
                effect_graph: Arc::clone(&effect_graph),
                frame_seed: 12,
            }]
        };
        let sequence_id = SequenceId::new();

        let color_context = test_color_context(ColorSpace::Rec709);
        let first = viewer_preview_cache_key_for_resolved_plan(
            sequence_id,
            320,
            180,
            &make_plan(100),
            &color_context,
        );
        let second = viewer_preview_cache_key_for_resolved_plan(
            sequence_id,
            320,
            180,
            &make_plan(200),
            &color_context,
        );

        assert_ne!(first, second);
    }

    #[test]
    fn resolved_media_preview_cache_key_includes_color_context() {
        let effect_graph = get_or_compile_scheduled_effect_graph(&EffectRenderPlan::default())
            .expect("default effect graph");
        let resolved = vec![ResolvedPreviewElement::Media {
            frame: test_media_frame_with_size(0, 2, 2, 100),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            effect_graph,
            frame_seed: 12,
        }];
        let sequence_id = SequenceId::new();

        let rec709 = test_color_context(ColorSpace::Rec709);
        let srgb = test_color_context(ColorSpace::Srgb);
        let first =
            viewer_preview_cache_key_for_resolved_plan(sequence_id, 320, 180, &resolved, &rec709);
        let second =
            viewer_preview_cache_key_for_resolved_plan(sequence_id, 320, 180, &resolved, &srgb);

        assert_ne!(first, second);
    }

    #[test]
    fn resolved_media_preview_cache_key_includes_display_management_policy() {
        let effect_graph = get_or_compile_scheduled_effect_graph(&EffectRenderPlan::default())
            .expect("default effect graph");
        let resolved = vec![ResolvedPreviewElement::Media {
            frame: test_media_frame_with_size(0, 2, 2, 100),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            effect_graph,
            frame_seed: 12,
        }];
        let sequence_id = SequenceId::new();

        let mut sdr = test_color_context(ColorSpace::Rec709);
        sdr.display_management = mondrian_core::DisplayManagementPolicy {
            monitor_profile: mondrian_core::MonitorProfileReference::ColorSpace(ColorSpace::Rec709),
            viewer_mode: mondrian_core::ViewerDisplayMode::Sdr,
            tone_map_policy: mondrian_core::DisplayToneMapPolicy::Automatic,
            ..Default::default()
        };
        let mut p3 = sdr.clone();
        p3.display_management = mondrian_core::DisplayManagementPolicy {
            monitor_profile: mondrian_core::MonitorProfileReference::ColorSpace(ColorSpace::DciP3),
            viewer_mode: mondrian_core::ViewerDisplayMode::Sdr,
            tone_map_policy: mondrian_core::DisplayToneMapPolicy::Automatic,
            ..Default::default()
        };
        let first =
            viewer_preview_cache_key_for_resolved_plan(sequence_id, 320, 180, &resolved, &sdr);
        let second =
            viewer_preview_cache_key_for_resolved_plan(sequence_id, 320, 180, &resolved, &p3);

        assert_ne!(first, second);
    }

    #[test]
    fn resolved_media_preview_cache_key_includes_display_view_context() {
        let effect_graph = get_or_compile_scheduled_effect_graph(&EffectRenderPlan::default())
            .expect("default effect graph");
        let resolved = vec![ResolvedPreviewElement::Media {
            frame: test_media_frame_with_size(0, 2, 2, 100),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            effect_graph,
            frame_seed: 12,
        }];
        let sequence_id = SequenceId::new();

        let mut rec709_view = test_color_context(ColorSpace::Rec709);
        rec709_view.ocio_display = Some("sRGB - Display".to_owned());
        rec709_view.ocio_view = Some("ACES 2.0 - SDR 100 nits (Rec.709)".to_owned());
        let mut colorimetric_view = rec709_view.clone();
        colorimetric_view.ocio_view = Some("Video (colorimetric)".to_owned());
        let first = viewer_preview_cache_key_for_resolved_plan(
            sequence_id,
            320,
            180,
            &resolved,
            &rec709_view,
        );
        let second = viewer_preview_cache_key_for_resolved_plan(
            sequence_id,
            320,
            180,
            &resolved,
            &colorimetric_view,
        );

        assert_ne!(first, second);
    }

    #[test]
    fn preview_working_composite_boundary_uses_resolved_display_view() {
        let mut color_context = test_color_context(ColorSpace::Rec709);
        color_context.ocio_display = Some("sRGB - Display".to_owned());
        color_context.ocio_view = Some("ACES 2.0 - SDR 100 nits (Rec.709)".to_owned());
        let mut scratch = TimelineCompositeScratch::default();

        let output = composite_resolved_preview_working(2, 2, &[], &color_context, &mut scratch)
            .expect("empty preview composite");

        let display_view = output.boundary.display_view.expect("resolved display/view");
        assert_eq!(display_view.display, "sRGB - Display");
        assert_eq!(display_view.view, "ACES 2.0 - SDR 100 nits (Rec.709)");
    }

    #[test]
    fn preview_input_color_resolution_honors_override_metadata_and_missing_policy() {
        let mut color_context = test_color_context(ColorSpace::Rec709);
        color_context.working_color_space = ColorSpace::Rec2020;
        color_context.missing_metadata_policy =
            MissingColorMetadataPolicy::AssumeSequenceWorkingSpace;

        assert_eq!(
            resolve_preview_input_color_space(
                Some(ColorSpace::SLog3),
                AssetMediaInterpretation::default(),
                Some(ColorSpace::Srgb),
                &color_context,
            )
            .color_space,
            Some(ColorSpace::SLog3)
        );
        assert_eq!(
            resolve_preview_input_color_space(
                Some(ColorSpace::SLog3),
                AssetMediaInterpretation::default(),
                Some(ColorSpace::Srgb),
                &color_context,
            )
            .source,
            mondrian_timeline::sequence::InputColorResolutionSource::Override
        );
        assert_eq!(
            resolve_preview_input_color_space(
                None,
                AssetMediaInterpretation::default(),
                Some(ColorSpace::Srgb),
                &color_context,
            ),
            mondrian_timeline::sequence::InputColorResolution {
                color_space: Some(ColorSpace::Srgb),
                source: mondrian_timeline::sequence::InputColorResolutionSource::DetectedMetadata,
                override_color_space: None,
                detected_color_space: Some(ColorSpace::Srgb),
                missing_metadata_policy: color_context.missing_metadata_policy,
                working_color_space: color_context.working_color_space,
            }
        );
        assert_eq!(
            resolve_preview_input_color_space(
                None,
                AssetMediaInterpretation::default(),
                None,
                &color_context,
            )
            .color_space,
            Some(ColorSpace::Rec2020)
        );
        assert_eq!(
            resolve_preview_input_color_space(
                None,
                AssetMediaInterpretation::default(),
                None,
                &color_context,
            )
            .source,
            mondrian_timeline::sequence::InputColorResolutionSource::MissingPolicyAssumeSequenceWorkingSpace
        );

        let asset_override = resolve_preview_input_color_space(
            None,
            AssetMediaInterpretation {
                color: mondrian_core::timeline_data::MediaColorInterpretation::Override {
                    color_space: ColorSpace::AppleLog,
                },
                ..AssetMediaInterpretation::default()
            },
            Some(ColorSpace::Srgb),
            &color_context,
        );
        assert_eq!(asset_override.color_space, Some(ColorSpace::AppleLog));
        assert_eq!(
            asset_override.source,
            mondrian_timeline::sequence::InputColorResolutionSource::Override
        );

        let data = resolve_preview_input_color_space(
            None,
            AssetMediaInterpretation {
                payload: mondrian_core::timeline_data::AssetColorPayload::NonColorData,
                ..AssetMediaInterpretation::default()
            },
            Some(ColorSpace::Srgb),
            &color_context,
        );
        assert_eq!(data.color_space, Some(ColorSpace::Rec2020));
        assert_eq!(
            data.source,
            mondrian_timeline::sequence::InputColorResolutionSource::DataTexture
        );

        color_context.missing_metadata_policy = MissingColorMetadataPolicy::RejectMedia;
        assert_eq!(
            resolve_preview_input_color_space(
                None,
                AssetMediaInterpretation::default(),
                None,
                &color_context,
            )
            .color_space,
            None
        );
        assert_eq!(
            resolve_preview_input_color_space(
                None,
                AssetMediaInterpretation::default(),
                None,
                &color_context,
            )
            .source,
            mondrian_timeline::sequence::InputColorResolutionSource::MissingPolicyRejectMedia
        );
    }

    #[test]
    fn preview_color_rejection_preserves_resolution_and_media_diagnostic() {
        let service = AppUiPreviewService::new();
        let mut color_context = test_color_context(ColorSpace::Rec709);
        color_context.working_color_space = ColorSpace::Rec2020;
        color_context.missing_metadata_policy = MissingColorMetadataPolicy::RejectMedia;
        let resolution = resolve_preview_input_color_space(
            None,
            AssetMediaInterpretation::default(),
            None,
            &color_context,
        );
        let asset_id = AssetId::new();
        let path = PathBuf::from("E:/media/missing-color-tags.mov");
        let diagnostic =
            "source=MissingMetadata,method=MissingMetadata,warnings=missing_or_unsupported_cicp"
                .to_string();
        let issue_summary = VideoColorDiagnosticIssueSummary {
            source: mondrian_media::VideoColorSpaceSource::MissingMetadata,
            method: mondrian_media::VideoColorDetectionMethod::MissingMetadata,
            confidence: mondrian_media::VideoColorInterpretationConfidence::None,
            missing_or_unsupported_cicp_tags: 1,
            has_user_visible_warnings: true,
            ..VideoColorDiagnosticIssueSummary {
                detected_color_space: None,
                source: mondrian_media::VideoColorSpaceSource::MissingMetadata,
                method: mondrian_media::VideoColorDetectionMethod::MissingMetadata,
                confidence: mondrian_media::VideoColorInterpretationConfidence::None,
                has_raw_cicp_metadata: false,
                metadata_hint_count: 0,
                evidence_count: 0,
                warning_count: 0,
                multiple_metadata_hints: 0,
                ignored_metadata_hints: 0,
                metadata_hint_overrides_cicp_tags: 0,
                partial_cicp_tags: 0,
                missing_or_unsupported_cicp_tags: 0,
                decoder_unavailable: 0,
                hdr_side_data_count: 0,
                has_mastering_display_metadata: false,
                has_content_light_metadata: false,
                has_dynamic_hdr10_plus: false,
                has_dolby_vision_config: false,
                has_icc_profile: false,
                icc_cicp_mismatch: 0,
                has_user_visible_warnings: false,
            }
        };

        service.record_color_rejection(AppUiPreviewColorRejection::new(
            asset_id,
            path.clone(),
            resolution,
            diagnostic.clone(),
            issue_summary,
        ));

        let rejection = service.last_color_rejection().expect("preview color rejection");
        assert_eq!(rejection.asset_id, asset_id);
        assert_eq!(rejection.path, path);
        assert_eq!(
            rejection.missing_metadata_policy,
            MissingColorMetadataPolicy::RejectMedia
        );
        assert_eq!(
            rejection.source,
            InputColorResolutionSource::MissingPolicyRejectMedia
        );
        assert_eq!(rejection.override_color_space, None);
        assert_eq!(rejection.detected_color_space, None);
        assert_eq!(rejection.working_color_space, ColorSpace::Rec2020);
        assert_eq!(rejection.diagnostic_summary, diagnostic);
        assert_eq!(rejection.diagnostic_issue_summary, issue_summary);
    }

    #[test]
    fn preview_render_request_clears_stale_color_rejection() {
        let service = AppUiPreviewService::new();
        let color_context = test_color_context(ColorSpace::Rec709);
        service.record_color_rejection(AppUiPreviewColorRejection::new(
            AssetId::new(),
            PathBuf::from("E:/media/old.mov"),
            resolve_preview_input_color_space(
                None,
                AssetMediaInterpretation::default(),
                None,
                &color_context,
            ),
            "old".to_string(),
            VideoColorDiagnosticIssueSummary {
                detected_color_space: None,
                source: mondrian_media::VideoColorSpaceSource::MissingMetadata,
                method: mondrian_media::VideoColorDetectionMethod::MissingMetadata,
                confidence: mondrian_media::VideoColorInterpretationConfidence::None,
                has_raw_cicp_metadata: false,
                metadata_hint_count: 0,
                evidence_count: 0,
                warning_count: 0,
                multiple_metadata_hints: 0,
                ignored_metadata_hints: 0,
                metadata_hint_overrides_cicp_tags: 0,
                partial_cicp_tags: 0,
                missing_or_unsupported_cicp_tags: 0,
                decoder_unavailable: 0,
                hdr_side_data_count: 0,
                has_mastering_display_metadata: false,
                has_content_light_metadata: false,
                has_dynamic_hdr10_plus: false,
                has_dolby_vision_config: false,
                has_icc_profile: false,
                icc_cicp_mismatch: 0,
                has_user_visible_warnings: false,
            },
        ));
        assert!(service.last_color_rejection().is_some());

        let state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
        let _ = ready_frame(service.viewer_preview_for_state(&state));

        assert_eq!(service.last_color_rejection(), None);
    }

    #[test]
    fn preview_and_export_input_color_resolution_counts_match_for_frame() {
        let mut sequence = Sequence::new("preview-export-color-resolution-parity");
        sequence.settings.color_space = ColorSpace::Rec2020;
        sequence.settings.color_management.missing_metadata_policy =
            MissingColorMetadataPolicy::AssumeSequenceWorkingSpace;
        let tb = sequence.time_base();
        let detected_id = AssetId::new();
        let override_id = AssetId::new();
        let missing_id = AssetId::new();
        let data_id = AssetId::new();

        sequence.video_tracks[0]
            .add_clip(Clip::new(
                detected_id,
                TimeCode::new(0, tb),
                TimeCode::new(10, tb),
            ))
            .expect("add detected clip");
        for (name, asset_id) in [
            ("override", override_id),
            ("missing", missing_id),
            ("data", data_id),
        ] {
            let mut track = Track::new_video(name);
            track
                .add_clip(Clip::new(
                    asset_id,
                    TimeCode::new(0, tb),
                    TimeCode::new(10, tb),
                ))
                .expect("add clip");
            sequence.video_tracks.push(track);
        }

        let mut asset_color_spaces = HashMap::new();
        asset_color_spaces.insert(detected_id, ColorSpace::Srgb);
        let mut asset_interpretations = HashMap::new();
        asset_interpretations.insert(
            override_id,
            AssetMediaInterpretation {
                color: mondrian_core::timeline_data::MediaColorInterpretation::Override {
                    color_space: ColorSpace::SLog3,
                },
                ..AssetMediaInterpretation::default()
            },
        );
        asset_interpretations.insert(
            data_id,
            AssetMediaInterpretation {
                payload: mondrian_core::timeline_data::AssetColorPayload::NonColorData,
                ..AssetMediaInterpretation::default()
            },
        );
        let project_color_management = ProjectColorManagement::default();

        let preview_counts = preview_input_color_resolution_counts_for_frame(
            &sequence,
            &[],
            &asset_color_spaces,
            &asset_interpretations,
            &project_color_management,
            ColorSpace::Rec709,
            0,
        )
        .expect("preview counts");
        let export_counts = mondrian_export::queue::export_input_color_resolution_counts_for_frame(
            &mondrian_export::preset::TimelineExportInput {
                sequence,
                sequences: Vec::new(),
                asset_paths: HashMap::new(),
                asset_color_spaces,
                asset_interpretations,
                asset_color_diagnostics: HashMap::new(),
                range: mondrian_export::preset::TimelineExportRange::SequenceInOut,
                project_color_management,
            },
            0,
        )
        .expect("export counts");

        assert_eq!(preview_counts, export_counts);
        assert_eq!(preview_counts.total(), 4);
        assert_eq!(
            preview_counts
                .count(mondrian_timeline::sequence::InputColorResolutionSource::DetectedMetadata),
            1
        );
        assert_eq!(
            preview_counts.count(mondrian_timeline::sequence::InputColorResolutionSource::Override),
            1
        );
        assert_eq!(
            preview_counts.count(
                mondrian_timeline::sequence::InputColorResolutionSource::MissingPolicyAssumeSequenceWorkingSpace
            ),
            1
        );
        assert_eq!(
            preview_counts
                .count(mondrian_timeline::sequence::InputColorResolutionSource::DataTexture),
            1
        );
    }

    #[test]
    fn preview_and_export_nested_input_color_resolution_counts_match_for_frame() {
        let mut parent = Sequence::new("parent-color-resolution-parity");
        parent.settings.color_space = ColorSpace::Rec2020;
        parent.settings.color_management.missing_metadata_policy =
            MissingColorMetadataPolicy::AssumeSequenceWorkingSpace;
        let mut nested = Sequence::new("nested-color-resolution-parity");
        nested.settings.color_space = ColorSpace::Rec2020;
        nested.settings.color_management.missing_metadata_policy =
            MissingColorMetadataPolicy::AssumeSequenceWorkingSpace;

        let parent_tb = parent.time_base();
        let nested_tb = nested.time_base();
        let parent_override_id = AssetId::new();
        let nested_detected_id = AssetId::new();
        let nested_data_id = AssetId::new();
        let nested_missing_id = AssetId::new();

        parent.video_tracks[0]
            .add_clip(Clip::new(
                parent_override_id,
                TimeCode::new(0, parent_tb),
                TimeCode::new(10, parent_tb),
            ))
            .expect("add parent media clip");
        let mut nested_track = Track::new_video("nested");
        nested_track
            .add_clip(Clip::new_nested_sequence(
                nested.id,
                TimeCode::new(0, parent_tb),
                TimeCode::new(10, parent_tb),
                Some("Nested".to_owned()),
            ))
            .expect("add nested sequence clip");
        parent.video_tracks.push(nested_track);

        nested.video_tracks[0]
            .add_clip(Clip::new(
                nested_detected_id,
                TimeCode::new(0, nested_tb),
                TimeCode::new(10, nested_tb),
            ))
            .expect("add nested detected clip");
        for (name, asset_id) in [("data", nested_data_id), ("missing", nested_missing_id)] {
            let mut track = Track::new_video(name);
            track
                .add_clip(Clip::new(
                    asset_id,
                    TimeCode::new(0, nested_tb),
                    TimeCode::new(10, nested_tb),
                ))
                .expect("add nested media clip");
            nested.video_tracks.push(track);
        }

        let mut asset_color_spaces = HashMap::new();
        asset_color_spaces.insert(nested_detected_id, ColorSpace::Srgb);
        let mut asset_interpretations = HashMap::new();
        asset_interpretations.insert(
            parent_override_id,
            AssetMediaInterpretation {
                color: mondrian_core::timeline_data::MediaColorInterpretation::Override {
                    color_space: ColorSpace::SLog3,
                },
                ..AssetMediaInterpretation::default()
            },
        );
        asset_interpretations.insert(
            nested_data_id,
            AssetMediaInterpretation {
                payload: mondrian_core::timeline_data::AssetColorPayload::NonColorData,
                ..AssetMediaInterpretation::default()
            },
        );
        let project_color_management = ProjectColorManagement::default();
        let nested_sequences = vec![nested.clone()];

        let preview_counts = preview_input_color_resolution_counts_for_frame(
            &parent,
            &nested_sequences,
            &asset_color_spaces,
            &asset_interpretations,
            &project_color_management,
            ColorSpace::Rec709,
            0,
        )
        .expect("preview nested counts");
        let export_counts = mondrian_export::queue::export_input_color_resolution_counts_for_frame(
            &mondrian_export::preset::TimelineExportInput {
                sequence: parent,
                sequences: nested_sequences,
                asset_paths: HashMap::new(),
                asset_color_spaces,
                asset_interpretations,
                asset_color_diagnostics: HashMap::new(),
                range: mondrian_export::preset::TimelineExportRange::SequenceInOut,
                project_color_management,
            },
            0,
        )
        .expect("export nested counts");

        assert_eq!(preview_counts, export_counts);
        assert_eq!(preview_counts.total(), 4);
        assert_eq!(
            preview_counts.count(mondrian_timeline::sequence::InputColorResolutionSource::Override),
            1
        );
        assert_eq!(
            preview_counts
                .count(mondrian_timeline::sequence::InputColorResolutionSource::DetectedMetadata),
            1
        );
        assert_eq!(
            preview_counts
                .count(mondrian_timeline::sequence::InputColorResolutionSource::DataTexture),
            1
        );
        assert_eq!(
            preview_counts.count(
                mondrian_timeline::sequence::InputColorResolutionSource::MissingPolicyAssumeSequenceWorkingSpace
            ),
            1
        );
    }

    #[test]
    fn preview_and_export_asset_issue_summaries_match_for_referenced_assets() {
        let mut parent = Sequence::new("parent-asset-issue-parity");
        let mut nested = Sequence::new("nested-asset-issue-parity");
        let nested_id = nested.id;
        let parent_tb = parent.time_base();
        let nested_tb = nested.time_base();
        let direct_id = AssetId::new();
        let nested_asset_id = AssetId::new();
        let unused_id = AssetId::new();

        parent.video_tracks[0]
            .add_clip(Clip::new(
                direct_id,
                TimeCode::new(0, parent_tb),
                TimeCode::new(10, parent_tb),
            ))
            .expect("add direct media clip");
        let mut nested_track = Track::new_video("nested");
        nested_track
            .add_clip(Clip::new_nested_sequence(
                nested_id,
                TimeCode::new(0, parent_tb),
                TimeCode::new(10, parent_tb),
                Some("Nested".to_owned()),
            ))
            .expect("add nested sequence clip");
        parent.video_tracks.push(nested_track);

        nested.video_tracks[0]
            .add_clip(Clip::new(
                nested_asset_id,
                TimeCode::new(0, nested_tb),
                TimeCode::new(10, nested_tb),
            ))
            .expect("add nested media clip");

        let mut asset_color_diagnostics = HashMap::new();
        asset_color_diagnostics.insert(
            direct_id,
            mondrian_media::VideoColorDiagnostic {
                detected_color_space: None,
                interpretation: mondrian_media::DetectedColorInterpretation {
                    color_space: None,
                    confidence: mondrian_media::VideoColorInterpretationConfidence::None,
                    source: mondrian_media::VideoColorSpaceSource::MissingMetadata,
                    method: mondrian_media::VideoColorDetectionMethod::MissingMetadata,
                    evidence: Vec::new(),
                    warnings: vec![
                        mondrian_media::VideoColorInterpretationWarning::MissingOrUnsupportedCicpTags,
                    ],
                    user_overridable: true,
                },
                source: mondrian_media::VideoColorSpaceSource::MissingMetadata,
                method: mondrian_media::VideoColorDetectionMethod::MissingMetadata,
                metadata: None,
                metadata_hints: Vec::new(),
                hdr_metadata: Vec::new(),
            },
        );
        asset_color_diagnostics.insert(
            nested_asset_id,
            mondrian_media::VideoColorDiagnostic {
                detected_color_space: None,
                interpretation: mondrian_media::DetectedColorInterpretation {
                    color_space: None,
                    confidence: mondrian_media::VideoColorInterpretationConfidence::None,
                    source: mondrian_media::VideoColorSpaceSource::DecoderUnavailable,
                    method: mondrian_media::VideoColorDetectionMethod::DecoderUnavailable,
                    evidence: vec![
                        mondrian_media::VideoColorInterpretationEvidence::DecoderUnavailable,
                    ],
                    warnings: vec![
                        mondrian_media::VideoColorInterpretationWarning::DecoderUnavailable,
                    ],
                    user_overridable: true,
                },
                source: mondrian_media::VideoColorSpaceSource::DecoderUnavailable,
                method: mondrian_media::VideoColorDetectionMethod::DecoderUnavailable,
                metadata: None,
                metadata_hints: Vec::new(),
                hdr_metadata: Vec::new(),
            },
        );
        asset_color_diagnostics.insert(
            unused_id,
            mondrian_media::VideoColorDiagnostic {
                detected_color_space: Some(ColorSpace::Rec709),
                interpretation: mondrian_media::DetectedColorInterpretation {
                    color_space: Some(ColorSpace::Rec709),
                    confidence: mondrian_media::VideoColorInterpretationConfidence::Low,
                    source: mondrian_media::VideoColorSpaceSource::Metadata,
                    method: mondrian_media::VideoColorDetectionMethod::MetadataHint,
                    evidence: Vec::new(),
                    warnings: vec![
                        mondrian_media::VideoColorInterpretationWarning::PartialCicpTags {
                            detected_color_space: ColorSpace::Rec709,
                        },
                    ],
                    user_overridable: true,
                },
                source: mondrian_media::VideoColorSpaceSource::Metadata,
                method: mondrian_media::VideoColorDetectionMethod::MetadataHint,
                metadata: None,
                metadata_hints: Vec::new(),
                hdr_metadata: Vec::new(),
            },
        );

        let nested_sequences = vec![nested.clone()];
        let mut preview_asset_ids = std::collections::HashSet::new();
        preview_asset_issue_summary_for_sequence(
            &parent,
            &nested_sequences,
            &asset_color_diagnostics,
            0,
            &mut preview_asset_ids,
        );
        let mut preview_summary = mondrian_media::VideoColorDiagnosticIssueAggregate::default();
        for asset_id in preview_asset_ids {
            preview_summary.observe(
                asset_color_diagnostics.get(&asset_id).expect("preview referenced diagnostic"),
            );
        }

        let export_summary = mondrian_export::queue::export_asset_issue_summary(
            &mondrian_export::preset::TimelineExportInput {
                sequence: parent,
                sequences: nested_sequences,
                asset_paths: HashMap::new(),
                asset_color_spaces: HashMap::new(),
                asset_interpretations: HashMap::new(),
                asset_color_diagnostics,
                range: mondrian_export::preset::TimelineExportRange::SequenceInOut,
                project_color_management: ProjectColorManagement::default(),
            },
        );

        assert_eq!(preview_summary, export_summary);
        assert_eq!(preview_summary.diagnostics, 2);
        assert_eq!(preview_summary.method_missing_metadata, 1);
        assert_eq!(preview_summary.method_decoder_unavailable, 1);
        assert_eq!(preview_summary.method_metadata_hint, 0);
        assert_eq!(preview_summary.warning_count, 2);
    }

    #[test]
    fn preview_and_export_composite_color_path_summaries_match_for_frame() {
        let mut sequence = Sequence::new("preview-export-composite-diagnostics-parity");
        let tb = sequence.time_base();
        let mut solid = Clip::new_solid_color(
            AssetId::new(),
            Color::from_rgba8(64, 96, 220, 255),
            TimeCode::new(0, tb),
            TimeCode::new(10, tb),
        );
        solid.transform.set_scale(glam::Vec2::new(0.75, 0.75));
        sequence.video_tracks[0]
            .add_clip(solid)
            .expect("add transformed solid color clip");

        let mut state = AppState::new();
        state.sequence = Some(sequence.clone());
        state.seek(0);
        let preview_service = AppUiPreviewService::new();
        let preview_frame = preview_service.viewer_preview_for_state(&state);
        let preview_frame = ready_frame(preview_frame);
        let preview_summary = preview_service.diagnostics().composite_color_path_summary();

        let export_diagnostics = mondrian_export::queue::export_composite_diagnostics_for_frame(
            &mondrian_export::preset::TimelineExportInput {
                sequence,
                sequences: Vec::new(),
                asset_paths: HashMap::new(),
                asset_color_spaces: HashMap::new(),
                asset_interpretations: HashMap::new(),
                asset_color_diagnostics: HashMap::new(),
                range: mondrian_export::preset::TimelineExportRange::SequenceInOut,
                project_color_management: ProjectColorManagement::default(),
            },
            0,
            preview_frame.width,
            preview_frame.height,
        )
        .expect("export composite diagnostics");
        let export_summary = export_diagnostics.color_path_summary();

        assert_eq!(
            preview_summary.path,
            TimelineCompositeColorPath::FloatLinear
        );
        assert_eq!(preview_summary.path, export_summary.path);
        assert_eq!(preview_summary.elements, export_summary.elements);
        assert_eq!(
            preview_summary.float_linear_composites,
            export_summary.float_linear_composites
        );
        assert_eq!(
            preview_summary.legacy_rgba8_composites,
            export_summary.legacy_rgba8_composites
        );
        assert_eq!(
            preview_summary.legacy_breakdown.solid_transform,
            export_summary.legacy_breakdown.solid_transform
        );
        assert_eq!(
            preview_summary.legacy_breakdown.total(),
            export_summary.legacy_breakdown.total()
        );
    }

    #[test]
    fn preview_single_media_color_output_matches_export_composite_contract() {
        let effect_graph = get_or_compile_scheduled_effect_graph(&EffectRenderPlan::default())
            .expect("default effect graph");
        let frame = test_media_frame_rgba(vec![200, 100, 40, 255], 1, 1, 77);
        let color_context = test_color_context(ColorSpace::Srgb);
        let resolved = vec![ResolvedPreviewElement::Media {
            frame: frame.clone(),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            effect_graph: Arc::clone(&effect_graph),
            frame_seed: 0,
        }];
        let mut preview_scratch = TimelineCompositeScratch::default();
        let preview_service = AppUiPreviewService::new();
        let preview = composite_resolved_preview(
            &preview_service,
            1,
            1,
            &resolved,
            &color_context,
            &mut preview_scratch,
        )
        .expect("preview color composite");
        assert_eq!(
            preview.color_diagnostics.output.domain,
            ColorFrameDomain::Display
        );
        assert_eq!(preview.composite_diagnostics.float_linear_composites, 1);
        assert_eq!(preview.composite_diagnostics.legacy_rgba8_composites, 0);
        assert_eq!(preview.composite_diagnostics.legacy_media_transform, 0);

        let export_working_frame = frame.working_frame().expect("export working frame");
        let export_elements = vec![TimelineCompositeElement::Media(TimelineMediaLayer {
            frame: &export_working_frame.frame,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            effect_graph,
            frame_seed: 0,
        })];
        let mut export_scratch = TimelineCompositeScratch::default();
        let expected_frame = mondrian_renderer::composite_timeline_elements_color_frame(
            1,
            1,
            &export_elements,
            TimelineCompositeOptions::default(),
            color_context.working_color_space,
            &mut export_scratch,
        );
        assert_eq!(
            expected_frame.descriptor().color_space,
            color_context.working_color_space
        );
        let export = mondrian_renderer::execute_cpu_output_boundary(
            &expected_frame,
            &RenderOutputColorBoundary::export(
                color_context.output_color_space,
                color_context.tone_map,
                color_context.engine.clone(),
            ),
        )
        .expect("export color transform");
        assert_eq!(
            export.result.diagnostics.output.domain,
            ColorFrameDomain::Export
        );
        assert_eq!(
            preview.color_stage_diagnostics.cpu_output_stages,
            export.stage_diagnostics.cpu_output_stages
        );
        let expected = export.result.frame.into_rgba();

        assert_eq!(preview.rgba, expected);
    }

    #[test]
    fn preview_multilayer_color_output_matches_export_frame_hash() {
        const REC2020_TO_SRGB_DISPLAY_VIEW_MULTILAYER_GOLDEN_HASH: u64 = 6_377_061_385_888_487_029;

        let effect_graph = get_or_compile_scheduled_effect_graph(&EffectRenderPlan::default())
            .expect("default effect graph");
        let source = CpuEncodedColorFrame::source_rgba8(
            2,
            2,
            ColorSpace::Srgb,
            vec![
                200, 24, 16, 255, 40, 220, 96, 255, 12, 64, 240, 255, 240, 220, 40, 255,
            ],
        );
        let frame = execute_cpu_input_stage(
            &source,
            &RenderInputTransform::to_working(
                ColorSpace::Rec2020,
                false,
                ColorEngine::MondrianSmart,
            ),
        )
        .expect("media input transform")
        .result
        .frame;
        let media = MediaPreviewFrame {
            width: frame.descriptor().width,
            height: frame.descriptor().height,
            frame: Some(frame),
            gpu_source: None,
            signature: 2_020,
        };
        let solid = TimelineSolidColorLayer {
            color: Color::from_rgba8(32, 180, 220, 255),
            opacity: 0.35,
            blend_mode: BlendMode::Screen,
            transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            effect_graph: Arc::clone(&effect_graph),
            frame_seed: 14,
        };
        let mut color_context = test_color_context(ColorSpace::Srgb);
        color_context.working_color_space = ColorSpace::Rec2020;
        assert!(color_context.ocio_display.is_some());
        assert!(color_context.ocio_view.is_some());

        let resolved = vec![
            ResolvedPreviewElement::Media {
                frame: media.clone(),
                opacity: 0.85,
                blend_mode: BlendMode::Multiply,
                transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
                effect_graph: Arc::clone(&effect_graph),
                frame_seed: 7,
            },
            ResolvedPreviewElement::SolidColor(solid.clone()),
        ];
        let mut preview_scratch = TimelineCompositeScratch::default();
        let preview_service = AppUiPreviewService::new();
        let preview = composite_resolved_preview(
            &preview_service,
            2,
            2,
            &resolved,
            &color_context,
            &mut preview_scratch,
        )
        .expect("preview multilayer composite");

        let export_media_working = media.working_frame().expect("export media working frame");
        let export_elements = vec![
            TimelineCompositeElement::Media(TimelineMediaLayer {
                frame: &export_media_working.frame,
                opacity: 0.85,
                blend_mode: BlendMode::Multiply,
                transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
                effect_graph,
                frame_seed: 7,
            }),
            TimelineCompositeElement::SolidColor(solid),
        ];
        let mut export_scratch = TimelineCompositeScratch::default();
        let export_working =
            mondrian_renderer::composite_timeline_elements_color_frame_with_diagnostics(
                2,
                2,
                &export_elements,
                TimelineCompositeOptions::default(),
                color_context.working_color_space,
                &mut export_scratch,
            );
        let export_boundary = match (&color_context.ocio_display, &color_context.ocio_view) {
            (Some(display), Some(view)) => RenderOutputColorBoundary::export_view(
                color_context.output_color_space,
                display.clone(),
                view.clone(),
                color_context.tone_map,
                color_context.engine.clone(),
            ),
            _ => RenderOutputColorBoundary::export(
                color_context.output_color_space,
                color_context.tone_map,
                color_context.engine.clone(),
            ),
        };
        let export_output =
            mondrian_renderer::execute_cpu_output_boundary(&export_working.frame, &export_boundary)
                .expect("export multilayer color transform");
        let export = export_output.result.frame.clone().into_rgba();

        assert_eq!(preview.rgba, export);
        let preview_export_hash = stable_rgba_hash(&preview.rgba);
        assert_eq!(preview_export_hash, stable_rgba_hash(&export));
        assert_eq!(
            preview_export_hash,
            REC2020_TO_SRGB_DISPLAY_VIEW_MULTILAYER_GOLDEN_HASH
        );
        assert_eq!(preview.composite_diagnostics.legacy_rgba8_composites, 0);
        assert_eq!(preview.composite_diagnostics.legacy_media_blend_mode, 0);
        assert_eq!(preview.composite_diagnostics.legacy_media_transform, 0);
        assert_eq!(preview.composite_diagnostics.legacy_solid_blend_mode, 0);
        assert_eq!(preview.composite_diagnostics.legacy_solid_transform, 0);

        let preview_composite_summary = preview.composite_diagnostics.color_path_summary();
        let preview_diagnostics = AppUiPreviewDiagnostics {
            color_stage_total_stages: preview.color_stage_diagnostics.total_stages,
            color_stage_cpu_input_stages: preview.color_stage_diagnostics.cpu_input_stages,
            color_stage_cpu_output_stages: preview.color_stage_diagnostics.cpu_output_stages,
            color_stage_gpu_color_stages: preview.color_stage_diagnostics.gpu_color_stages,
            color_stage_upload_stages: preview.color_stage_diagnostics.upload_stages,
            color_stage_readback_stages: preview.color_stage_diagnostics.readback_stages,
            color_stage_gpu_blockers: preview.color_stage_diagnostics.gpu_blockers,
            color_stage_gpu_shader_module_blockers: preview
                .color_stage_diagnostics
                .gpu_blocker_breakdown
                .shader_module_not_prepared,
            color_stage_gpu_ocio_resource_blockers: preview
                .color_stage_diagnostics
                .gpu_blocker_breakdown
                .ocio_resource_bind_group_not_prepared,
            color_stage_gpu_wrapper_blockers: preview
                .color_stage_diagnostics
                .gpu_blocker_breakdown
                .fullscreen_wrapper_not_prepared,
            color_stage_gpu_render_pipeline_blockers: preview
                .color_stage_diagnostics
                .gpu_blocker_breakdown
                .render_pipeline_not_prepared,
            color_stage_pixels: preview.color_stage_diagnostics.stage_pixels,
            color_rgba8_boundary_calls: u64::from(preview.color_diagnostics.used_rgba8_boundary),
            color_composite_plans: preview_composite_summary.composite_plans(),
            color_composite_elements: preview.composite_diagnostics.elements,
            color_composite_float_linear: preview.composite_diagnostics.float_linear_composites,
            color_composite_legacy_rgba8: preview.composite_diagnostics.legacy_rgba8_composites,
            color_composite_legacy_media_blend_mode: preview
                .composite_diagnostics
                .legacy_media_blend_mode,
            color_composite_legacy_media_transform: preview
                .composite_diagnostics
                .legacy_media_transform,
            color_composite_legacy_media_effect: preview.composite_diagnostics.legacy_media_effect,
            color_composite_legacy_solid_blend_mode: preview
                .composite_diagnostics
                .legacy_solid_blend_mode,
            color_composite_legacy_solid_transform: preview
                .composite_diagnostics
                .legacy_solid_transform,
            color_composite_legacy_solid_effect: preview.composite_diagnostics.legacy_solid_effect,
            color_composite_legacy_adjustment_blend_mode: preview
                .composite_diagnostics
                .legacy_adjustment_blend_mode,
            color_composite_legacy_adjustment_effect: preview
                .composite_diagnostics
                .legacy_adjustment_effect,
            ..AppUiPreviewDiagnostics::default()
        };
        let preview_health =
            preview_diagnostics.color_health_summary().expect("preview color health");
        assert_eq!(preview_health.rgba8_boundary_calls, 1);

        let mut export_diagnostics = mondrian_export::queue::ExportJobColorDiagnostics::default();
        export_diagnostics.record_frame_diagnostics(
            mondrian_timeline::sequence::InputColorResolutionSourceCounts::default(),
            export_output.stage_diagnostics,
            export_working.diagnostics,
        );
        let export_health = export_diagnostics.summary().expect("export color health");
        assert_preview_export_color_health_match(preview_health, export_health);
        let preview_report =
            build_preview_color_health_report(Some(preview_health), "preview-export-golden");
        let export_report = export_diagnostics
            .health_report("preview-export-golden")
            .expect("export color report");
        assert_preview_export_color_reports_match(&preview_report, &export_report);
    }

    #[test]
    fn stale_viewer_frame_is_scoped_to_sequence_and_dimensions() {
        let service = AppUiPreviewService::new();
        let state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
        let sequence = state.sequence.as_ref().expect("sequence");
        let (width, height) = preview_dimensions_for_sequence(sequence);

        let ready = ready_frame(service.viewer_preview_for_state(&state));
        let same_scope = service
            .stale_frame_for_sequence(sequence, width, height)
            .expect("same sequence can reuse stale frame");
        assert_eq!(same_scope.key, ready.key);

        let different_sequence = Sequence::new("other");
        assert!(service.stale_frame_for_sequence(&different_sequence, width, height).is_none());
        assert!(service
            .stale_frame_for_sequence(sequence, width.saturating_add(1), height)
            .is_none());
    }

    #[test]
    fn playback_prefetch_yields_while_current_frame_is_pending() {
        let service = AppUiPreviewService::new_without_workers_for_test();
        let mut state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
        state.play();
        let sequence = state.sequence.as_ref().expect("sequence");
        let (width, height) = preview_dimensions_for_sequence(sequence);

        service.current_frame_pending.set(true);
        service.schedule_media_prefetches(&state, sequence, state.current_frame(), width, height);

        let diagnostics = service.diagnostics();
        assert_eq!(diagnostics.prefetch_skipped_current_pending, 1);
        assert_eq!(diagnostics.enqueued_jobs, 0);
        assert_eq!(diagnostics.worker_queue.queued_prefetch_jobs, 0);
    }

    #[test]
    fn stalled_playback_current_expiration_releases_pending_and_queued_work() {
        let service = AppUiPreviewService::new_without_workers_for_test();
        service.seed_pending_playback_current_preview_work_for_test();

        let before = service.diagnostics();
        assert_eq!(before.scheduler.pending_requests, 1);
        assert_eq!(before.worker_queue.queued_jobs, 1);
        assert!(service.current_frame_pending.get());

        assert!(service.expire_stalled_playback_current_with_timeout(Duration::ZERO));

        let diagnostics = service.diagnostics();
        assert_eq!(diagnostics.playback_current_stalled_expirations, 1);
        assert_eq!(diagnostics.playback_schedule.current_drop_late_decisions, 1);
        assert_eq!(diagnostics.playback_schedule.current_late_streak, 1);
        assert!(!diagnostics.playback_schedule.sustained_pressure_active);
        assert_eq!(diagnostics.playback_schedule.sustained_pressure_events, 0);
        assert_eq!(
            diagnostics.playback_schedule.current_proxy_or_hardware_recommended_decisions,
            1
        );
        assert_eq!(diagnostics.queue_canceled_jobs, 1);
        assert_eq!(diagnostics.scheduler.pending_requests, 0);
        assert_eq!(diagnostics.scheduler.canceled_requests, 1);
        assert_eq!(diagnostics.worker_queue.queued_jobs, 0);
        assert!(!service.current_frame_pending.get());
    }

    #[test]
    fn repeated_late_playback_current_frames_enter_pressure_recovery() {
        let service = AppUiPreviewService::new_without_workers_for_test();

        service.seed_pending_playback_current_preview_work_for_test();
        assert!(service.expire_stalled_playback_current_with_timeout(Duration::ZERO));
        service.seed_pending_playback_current_preview_work_for_test();
        assert!(service.expire_stalled_playback_current_with_timeout(Duration::ZERO));

        let diagnostics = service.diagnostics();
        assert_eq!(diagnostics.playback_current_stalled_expirations, 2);
        assert_eq!(diagnostics.playback_schedule.current_drop_late_decisions, 2);
        assert_eq!(diagnostics.playback_schedule.current_late_streak, 2);
        assert!(diagnostics.playback_schedule.sustained_pressure_active);
        assert_eq!(diagnostics.playback_schedule.sustained_pressure_events, 1);
        assert_eq!(
            diagnostics.playback_schedule.sustained_pressure_recoveries,
            0
        );
    }

    #[test]
    fn playback_pressure_recovery_suppresses_forward_prefetch_until_current_success() {
        let service = AppUiPreviewService::new_without_workers_for_test();
        let mut state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
        state.play();
        let sequence = state.sequence.as_ref().expect("sequence");
        let (width, height) = preview_dimensions_for_sequence(sequence);

        service.record_playback_current_late_drop(
            MEDIA_PREVIEW_PLAYBACK_PRESSURE_LATE_STREAK_THRESHOLD,
        );
        service.schedule_media_prefetches(&state, sequence, state.current_frame(), width, height);

        let diagnostics = service.diagnostics();
        assert!(diagnostics.playback_schedule.sustained_pressure_active);
        assert_eq!(
            diagnostics.playback_schedule.prefetch_skipped_sustained_pressure,
            1
        );
        assert_eq!(diagnostics.prefetch_skipped_current_pending, 0);
        assert_eq!(diagnostics.prefetch_skipped_worker_busy, 0);
        assert_eq!(diagnostics.worker_queue.queued_prefetch_jobs, 0);

        service.record_playback_current_success(
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::PlaybackCursor,
        );
        let diagnostics = service.diagnostics();
        assert_eq!(diagnostics.playback_schedule.current_late_streak, 0);
        assert!(!diagnostics.playback_schedule.sustained_pressure_active);
        assert_eq!(
            diagnostics.playback_schedule.sustained_pressure_recoveries,
            1
        );
    }

    #[test]
    fn playback_prefetch_yields_while_current_work_is_queued() {
        let service = AppUiPreviewService::new_without_workers_for_test();
        let mut state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
        state.play();
        let sequence = state.sequence.as_ref().expect("sequence");
        let (width, height) = preview_dimensions_for_sequence(sequence);
        let current_key = test_media_key(100);

        assert_eq!(
            service.jobs.enqueue(MediaPreviewJob {
                key: current_key,
                source_secs: 1.0,
                generation: 1,
                priority: MediaPreviewRequestPriority::Current,
                access_mode: PreviewDecodeAccessMode::ScrubCursor,
                adaptive_hints: PreviewDecodeAdaptiveHints::default(),
                enqueued_at: Instant::now(),
                deadline_at: None,
            }),
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        );

        service.schedule_media_prefetches(&state, sequence, state.current_frame(), width, height);

        let diagnostics = service.diagnostics();
        assert_eq!(diagnostics.prefetch_skipped_current_pending, 0);
        assert_eq!(diagnostics.prefetch_skipped_worker_busy, 1);
        assert_eq!(diagnostics.prefetch_skipped_prefetch_backlog, 0);
        assert_eq!(diagnostics.worker_queue.queued_current_jobs, 1);
        assert_eq!(diagnostics.worker_queue.queued_prefetch_jobs, 0);
    }

    #[test]
    fn playback_prefetch_yields_while_current_work_is_in_flight() {
        let service = AppUiPreviewService::new_without_workers_for_test();
        let mut state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
        state.play();
        let sequence = state.sequence.as_ref().expect("sequence");
        let (width, height) = preview_dimensions_for_sequence(sequence);
        let _current = service.worker_activity.begin(
            MediaPreviewWorkerLane::Scrub,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::ScrubCursor,
        );

        service.schedule_media_prefetches(&state, sequence, state.current_frame(), width, height);

        let diagnostics = service.diagnostics();
        assert_eq!(diagnostics.prefetch_skipped_current_pending, 0);
        assert_eq!(diagnostics.prefetch_skipped_worker_busy, 1);
        assert_eq!(diagnostics.prefetch_skipped_prefetch_backlog, 0);
        assert_eq!(diagnostics.enqueued_jobs, 0);
        assert_eq!(diagnostics.worker_queue.queued_prefetch_jobs, 0);
        assert_eq!(diagnostics.worker_activity.in_flight_current_jobs, 1);
    }

    #[test]
    fn playback_prefetch_yields_when_prefetch_backlog_already_covers_window() {
        let service = AppUiPreviewService::new_without_workers_for_test();
        let mut state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
        state.play();
        let sequence = state.sequence.as_ref().expect("sequence");
        let (width, height) = preview_dimensions_for_sequence(sequence);
        let prefetch_window =
            media_preview_forward_prefetch_window_frames(sequence.settings.frame_rate)
                .expect("valid sequence frame rate");

        for offset in 0..prefetch_window as i64 {
            assert_eq!(
                service.jobs.enqueue(MediaPreviewJob {
                    key: test_media_key(200 + offset),
                    source_secs: offset as f64,
                    generation: 1,
                    priority: MediaPreviewRequestPriority::Prefetch,
                    access_mode: PreviewDecodeAccessMode::PlaybackCursor,
                    adaptive_hints: PreviewDecodeAdaptiveHints::default(),
                    enqueued_at: Instant::now(),
                    deadline_at: None,
                }),
                MediaPreviewJobEnqueueStatus::Enqueued {
                    evicted_prefetch: None,
                    evicted_still: None
                }
            );
        }

        service.schedule_media_prefetches(&state, sequence, state.current_frame(), width, height);

        let diagnostics = service.diagnostics();
        assert_eq!(diagnostics.prefetch_skipped_current_pending, 0);
        assert_eq!(diagnostics.prefetch_skipped_worker_busy, 0);
        assert_eq!(diagnostics.prefetch_skipped_prefetch_backlog, 1);
        assert_eq!(
            diagnostics.worker_queue.queued_prefetch_jobs,
            prefetch_window
        );
    }

    #[test]
    fn playback_prefetch_yields_when_in_flight_prefetch_covers_window() {
        let service = AppUiPreviewService::new_without_workers_for_test();
        let mut state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
        state.play();
        let sequence = state.sequence.as_ref().expect("sequence");
        let (width, height) = preview_dimensions_for_sequence(sequence);
        let prefetch_window =
            media_preview_forward_prefetch_window_frames(sequence.settings.frame_rate)
                .expect("valid sequence frame rate");
        let _first = service.worker_activity.begin(
            MediaPreviewWorkerLane::Playback,
            MediaPreviewRequestPriority::Prefetch,
            PreviewDecodeAccessMode::PlaybackCursor,
        );
        let _second = service.worker_activity.begin(
            MediaPreviewWorkerLane::Playback,
            MediaPreviewRequestPriority::Prefetch,
            PreviewDecodeAccessMode::PlaybackCursor,
        );

        service.schedule_media_prefetches(&state, sequence, state.current_frame(), width, height);

        let diagnostics = service.diagnostics();
        assert_eq!(diagnostics.prefetch_skipped_current_pending, 0);
        assert_eq!(diagnostics.prefetch_skipped_worker_busy, 0);
        assert_eq!(diagnostics.prefetch_skipped_prefetch_backlog, 1);
        assert_eq!(diagnostics.enqueued_jobs, 0);
        assert_eq!(diagnostics.worker_queue.queued_prefetch_jobs, 0);
        assert_eq!(
            diagnostics.worker_activity.in_flight_prefetch_jobs,
            prefetch_window
        );
    }

    #[test]
    fn playback_prefetch_tops_up_only_remaining_window_slots() {
        let service = AppUiPreviewService::new_without_workers_for_test();
        let (mut state, _, root) = state_with_invalid_video_asset();
        state.play();
        let sequence = state.sequence.as_ref().expect("sequence");
        let (width, height) = preview_dimensions_for_sequence(sequence);
        let prefetch_window =
            media_preview_forward_prefetch_window_frames(sequence.settings.frame_rate)
                .expect("valid sequence frame rate");

        assert_eq!(
            service.jobs.enqueue(MediaPreviewJob {
                key: test_media_key(300),
                source_secs: 0.0,
                generation: 1,
                priority: MediaPreviewRequestPriority::Prefetch,
                access_mode: PreviewDecodeAccessMode::PlaybackCursor,
                adaptive_hints: PreviewDecodeAdaptiveHints::default(),
                enqueued_at: Instant::now(),
                deadline_at: None,
            }),
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        );

        service.schedule_media_prefetches(&state, sequence, state.current_frame(), width, height);

        let diagnostics = service.diagnostics();
        assert_eq!(diagnostics.prefetch_skipped_prefetch_backlog, 0);
        assert_eq!(
            diagnostics.worker_queue.queued_prefetch_jobs,
            prefetch_window
        );
        assert_eq!(diagnostics.enqueued_jobs, 1);
        service.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn playback_prefetch_tops_up_only_remaining_in_flight_window_slots() {
        let service = AppUiPreviewService::new_without_workers_for_test();
        let (mut state, _, root) = state_with_invalid_video_asset();
        state.play();
        let sequence = state.sequence.as_ref().expect("sequence");
        let (width, height) = preview_dimensions_for_sequence(sequence);
        let prefetch_window =
            media_preview_forward_prefetch_window_frames(sequence.settings.frame_rate)
                .expect("valid sequence frame rate");
        let _prefetch = service.worker_activity.begin(
            MediaPreviewWorkerLane::Playback,
            MediaPreviewRequestPriority::Prefetch,
            PreviewDecodeAccessMode::PlaybackCursor,
        );

        service.schedule_media_prefetches(&state, sequence, state.current_frame(), width, height);

        let diagnostics = service.diagnostics();
        assert_eq!(diagnostics.prefetch_skipped_prefetch_backlog, 0);
        assert_eq!(
            diagnostics.worker_queue.queued_prefetch_jobs,
            prefetch_window.saturating_sub(1)
        );
        assert_eq!(diagnostics.worker_activity.in_flight_prefetch_jobs, 1);
        assert_eq!(
            diagnostics.enqueued_jobs,
            prefetch_window.saturating_sub(1) as u64
        );
        service.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn playback_prefetch_tops_up_by_actual_jobs_across_tracks() {
        let service = AppUiPreviewService::new_without_workers_for_test();
        let (mut state, root) = state_with_two_invalid_video_assets();
        state.play();
        let sequence = state.sequence.as_ref().expect("sequence");
        let (width, height) = preview_dimensions_for_sequence(sequence);
        let prefetch_window =
            media_preview_forward_prefetch_window_frames(sequence.settings.frame_rate)
                .expect("valid sequence frame rate");

        assert_eq!(
            service.jobs.enqueue(MediaPreviewJob {
                key: test_media_key(400),
                source_secs: 0.0,
                generation: 1,
                priority: MediaPreviewRequestPriority::Prefetch,
                access_mode: PreviewDecodeAccessMode::PlaybackCursor,
                adaptive_hints: PreviewDecodeAdaptiveHints::default(),
                enqueued_at: Instant::now(),
                deadline_at: None,
            }),
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        );

        service.schedule_media_prefetches(&state, sequence, state.current_frame(), width, height);

        let diagnostics = service.diagnostics();
        assert_eq!(diagnostics.prefetch_skipped_prefetch_backlog, 0);
        assert_eq!(
            diagnostics.worker_queue.queued_prefetch_jobs,
            prefetch_window
        );
        assert_eq!(
            diagnostics.enqueued_jobs, 1,
            "one remaining prefetch slot must admit only one media job even if the next frame has multiple active tracks"
        );
        service.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn decode_media_preview_missing_file_reports_failure_without_frame() {
        let key = MediaPreviewKey {
            asset_id: AssetId::new(),
            path: PathBuf::from("E:/definitely-missing/mondrian-preview.mov"),
            fingerprint: None,
            source_frame: 12,
            source_micros: source_micros(0.5),
            target_width: 320,
            target_height: 180,
            input_color_space: ColorSpace::Rec709,
            working_color_space: ColorSpace::Rec709,
            tone_map: false,
            engine: ColorEngine::MondrianSmart,
        };

        let result = decode_media_preview(
            MediaPreviewJob {
                key: key.clone(),
                source_secs: 0.5,
                generation: 7,
                priority: MediaPreviewRequestPriority::Current,
                access_mode: PreviewDecodeAccessMode::ScrubCursor,
                adaptive_hints: PreviewDecodeAdaptiveHints::default(),
                enqueued_at: Instant::now(),
                deadline_at: None,
            },
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
    fn decode_media_preview_cancellation_is_not_a_media_failure() {
        let key = MediaPreviewKey {
            asset_id: AssetId::new(),
            path: PathBuf::from("E:/definitely-missing/canceled-preview.mov"),
            fingerprint: None,
            source_frame: 12,
            source_micros: source_micros(0.5),
            target_width: 320,
            target_height: 180,
            input_color_space: ColorSpace::Rec709,
            working_color_space: ColorSpace::Rec709,
            tone_map: false,
            engine: ColorEngine::MondrianSmart,
        };

        let result = decode_media_preview(
            MediaPreviewJob {
                key: key.clone(),
                source_secs: 0.5,
                generation: 7,
                priority: MediaPreviewRequestPriority::Prefetch,
                access_mode: PreviewDecodeAccessMode::PlaybackCursor,
                adaptive_hints: PreviewDecodeAdaptiveHints::default(),
                enqueued_at: Instant::now(),
                deadline_at: None,
            },
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
    fn media_preview_worker_reports_queue_dropped_expired_playback_current() {
        let (job_tx, job_rx) = media_preview_job_queue(2);
        let (result_tx, result_rx) = mpsc::channel();
        let scheduler = MediaPreviewScheduler::default();
        let worker_activity = Arc::new(PreviewWorkerActivity::default());
        let shutdown = Arc::new(AtomicBool::new(false));
        let key = test_media_key(42);
        let generation = scheduler.begin_generation();
        assert_eq!(
            scheduler.request(
                key.clone(),
                generation,
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::PlaybackCursor,
            ),
            MediaPreviewRequestStatus::Scheduled { evicted_prefetch: None, evicted_still: None }
        );
        assert_eq!(
            job_tx.enqueue(MediaPreviewJob {
                key: key.clone(),
                source_secs: 42.0,
                generation,
                priority: MediaPreviewRequestPriority::Current,
                access_mode: PreviewDecodeAccessMode::PlaybackCursor,
                adaptive_hints: PreviewDecodeAdaptiveHints::default(),
                enqueued_at: Instant::now(),
                deadline_at: Some(Instant::now() - Duration::from_millis(1)),
            }),
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        );

        let worker_scheduler = scheduler.clone();
        let worker_activity_for_thread = Arc::clone(&worker_activity);
        let worker_shutdown = Arc::clone(&shutdown);
        let worker = thread::spawn(move || {
            media_preview_worker(
                MediaPreviewWorkerLane::Playback,
                job_rx,
                result_tx,
                worker_scheduler,
                worker_activity_for_thread,
                worker_shutdown,
            );
        });
        let result = result_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("expired playback current should produce a canceled result");
        job_tx.close();
        worker.join().expect("preview worker should stop after queue close");

        assert_eq!(result.key, key);
        assert!(result.canceled);
        assert_eq!(
            result.cancel_reason,
            Some(MediaPreviewCancelReason::PlaybackDeadline)
        );
        assert_eq!(result.access_mode, PreviewDecodeAccessMode::PlaybackCursor);
        assert_eq!(result.priority, MediaPreviewRequestPriority::Current);
        assert_eq!(result.decode_elapsed_us, 0);
        assert_eq!(result.cancel_observed_elapsed_us, Some(0));
        assert_eq!(
            job_tx.diagnostics().dropped_expired_playback_current_jobs,
            1
        );
        assert_eq!(worker_activity.snapshot().in_flight_jobs, 0);
    }

    #[test]
    fn preview_service_poll_releases_expired_playback_deadline_without_preview_refresh() {
        let service = AppUiPreviewService::new_without_workers_for_test();
        let result_tx = install_preview_result_channel_for_test(&service);
        let key = test_media_key(77);
        let generation = service.scheduler.begin_generation();
        assert_eq!(
            service.scheduler.request(
                key.clone(),
                generation,
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::PlaybackCursor,
            ),
            MediaPreviewRequestStatus::Scheduled { evicted_prefetch: None, evicted_still: None }
        );
        assert_eq!(service.scheduler.diagnostics().pending_requests, 1);

        let result = media_preview_canceled_result(
            MediaPreviewJob {
                key: key.clone(),
                source_secs: 77.0,
                generation,
                priority: MediaPreviewRequestPriority::Current,
                access_mode: PreviewDecodeAccessMode::PlaybackCursor,
                adaptive_hints: PreviewDecodeAdaptiveHints::default(),
                enqueued_at: Instant::now(),
                deadline_at: Some(Instant::now() - Duration::from_millis(1)),
            },
            12_000,
            MediaPreviewCancelReason::PlaybackDeadline,
            0,
            Some(0),
        );
        result_tx.send(result).expect("send canceled result");

        let outcome = service.poll_finished_outcome_with_budget(8, Duration::from_millis(5));
        assert!(
            !outcome.visible_change,
            "deadline cancellation is scheduler/diagnostic evidence, not a new visible frame"
        );
        assert!(!outcome.needs_follow_up_poll);
        let diagnostics = service.diagnostics();
        assert_eq!(diagnostics.scheduler.pending_requests, 0);
        assert_eq!(diagnostics.decode_canceled_jobs, 1);
        assert_eq!(diagnostics.decode_canceled_playback_deadline_jobs, 1);
        assert_eq!(diagnostics.decode_canceled_playback_cursor_jobs, 1);
        assert_eq!(diagnostics.decode_queue_wait_max_us, 12_000);
        assert_eq!(diagnostics.playback_schedule.current_drop_late_decisions, 1);
        assert_eq!(
            diagnostics.playback_schedule.current_proxy_or_hardware_recommended_decisions,
            1
        );
        service.shutdown();
    }

    #[test]
    fn preview_service_poll_separates_canceled_backlog_from_visible_change() {
        let service = AppUiPreviewService::new_without_workers_for_test();
        let result_tx = install_preview_result_channel_for_test(&service);
        let generation = service.scheduler.begin_generation();

        for index in 0..2 {
            let key = test_media_key(80 + index);
            assert_eq!(
                service.scheduler.request(
                    key.clone(),
                    generation,
                    MediaPreviewRequestPriority::Current,
                    PreviewDecodeAccessMode::PlaybackCursor,
                ),
                MediaPreviewRequestStatus::Scheduled {
                    evicted_prefetch: None,
                    evicted_still: None
                }
            );
            result_tx
                .send(media_preview_canceled_result(
                    MediaPreviewJob {
                        key,
                        source_secs: index as f64,
                        generation,
                        priority: MediaPreviewRequestPriority::Current,
                        access_mode: PreviewDecodeAccessMode::PlaybackCursor,
                        adaptive_hints: PreviewDecodeAdaptiveHints::default(),
                        enqueued_at: Instant::now(),
                        deadline_at: Some(Instant::now() - Duration::from_millis(1)),
                    },
                    1_000,
                    MediaPreviewCancelReason::PlaybackDeadline,
                    0,
                    Some(0),
                ))
                .expect("send canceled result");
        }

        let outcome = service.poll_finished_outcome_with_budget(1, Duration::from_millis(5));

        assert!(!outcome.visible_change);
        assert!(
            outcome.needs_follow_up_poll,
            "count-budget exhaustion should keep draining without forcing preview refresh"
        );
        assert_eq!(service.scheduler.diagnostics().pending_requests, 1);
        assert_eq!(service.diagnostics().decode_canceled_jobs, 1);
        service.shutdown();
    }

    #[test]
    fn failed_current_media_preview_cache_does_not_leave_viewer_loading() {
        let (state, asset_id, root) = state_with_invalid_video_asset();
        let service = AppUiPreviewService::new();
        let sequence = state.sequence.as_ref().expect("sequence");
        let (width, height) = preview_dimensions_for_sequence(sequence);
        let color_context = sequence.settings.root_preview_color_context(
            &state.project_settings.color_management,
            ColorSpace::Rec709,
        );
        let (key, _) = service
            .media_preview_key_for_asset(
                &state,
                &asset_id,
                None,
                0,
                0.0,
                width,
                height,
                &color_context,
                true,
                false,
            )
            .expect("media preview key");
        service.media_failures.borrow_mut().insert(key);

        let preview = service.viewer_preview_for_state(&state);

        assert!(matches!(preview, ViewerPreviewState::Unavailable));
        assert!(
            !service.current_frame_pending.get(),
            "a cached decode failure is terminal evidence, not pending work"
        );
        service.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn playing_cached_media_preview_defers_sync_raster_composite() {
        let (mut state, asset_id, root) = state_with_invalid_video_asset();
        let service = AppUiPreviewService::new_without_workers_for_test();
        let sequence = state.sequence.as_ref().expect("sequence");
        let (width, height) = preview_dimensions_for_sequence(sequence);
        let color_context = sequence.settings.root_preview_color_context(
            &state.project_settings.color_management,
            ColorSpace::Rec709,
        );
        let (key, _) = service
            .media_preview_key_for_asset(
                &state,
                &asset_id,
                None,
                0,
                0.0,
                width,
                height,
                &color_context,
                true,
                false,
            )
            .expect("media preview key");
        service
            .media_cache
            .borrow_mut()
            .insert(key, test_media_frame_with_size(80, width, height, 123));

        state.play();
        let playing_preview = service.viewer_preview_for_state(&state);
        assert!(
            matches!(
                playing_preview,
                ViewerPreviewState::Loading | ViewerPreviewState::Stale(_)
            ),
            "playback must not synchronously CPU-composite cached media on the UI thread"
        );

        state.pause();
        let paused_preview = service.viewer_preview_for_state(&state);
        assert!(
            matches!(paused_preview, ViewerPreviewState::Ready(_)),
            "paused still-frame preview may use the CPU correctness path"
        );
        service.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    fn test_media_key(source_frame: i64) -> MediaPreviewKey {
        MediaPreviewKey {
            asset_id: AssetId::new(),
            path: PathBuf::from(format!("E:/media/{source_frame}.mov")),
            fingerprint: None,
            source_frame,
            source_micros: source_micros(source_frame as f64),
            target_width: 320,
            target_height: 180,
            input_color_space: ColorSpace::Rec709,
            working_color_space: ColorSpace::Rec709,
            tone_map: false,
            engine: ColorEngine::MondrianSmart,
        }
    }

    fn install_preview_result_channel_for_test(
        service: &AppUiPreviewService,
    ) -> mpsc::Sender<MediaPreviewResult> {
        let (result_tx, result_rx) = mpsc::channel();
        service.results.replace(result_rx);
        result_tx
    }

    fn test_successful_media_preview_result(
        key: MediaPreviewKey,
        generation: u64,
        seed: u8,
    ) -> MediaPreviewResult {
        MediaPreviewResult {
            key,
            frame: Some(test_media_frame(seed)),
            error: None,
            failure_reason: None,
            generation,
            priority: MediaPreviewRequestPriority::Current,
            access_mode: PreviewDecodeAccessMode::ScrubCursor,
            queue_wait_us: 0,
            decode_elapsed_us: 0,
            cancel_observed_elapsed_us: None,
            canceled: false,
            cancel_reason: None,
            decode_diagnostics: None,
            color_diagnostics: None,
            color_stage_diagnostics: None,
        }
    }

    fn test_media_frame(seed: u8) -> MediaPreviewFrame {
        test_media_frame_rgba(vec![seed, 0, 0, 255], 1, 1, seed as u64)
    }

    fn test_media_frame_with_size(
        seed: u8,
        width: u32,
        height: u32,
        signature: u64,
    ) -> MediaPreviewFrame {
        test_media_frame_rgba(
            std::iter::repeat_n([seed, 0, 0, 255], width as usize * height as usize)
                .flatten()
                .collect(),
            width,
            height,
            signature,
        )
    }

    fn test_media_frame_rgba(
        rgba: Vec<u8>,
        width: u32,
        height: u32,
        signature: u64,
    ) -> MediaPreviewFrame {
        let source = CpuEncodedColorFrame::source_rgba8(width, height, ColorSpace::Rec709, rgba);
        let input_transform =
            RenderInputTransform::to_working(ColorSpace::Rec709, false, ColorEngine::MondrianSmart);
        let frame =
            execute_cpu_input_stage(&source, &input_transform).expect("test media input transform");
        let frame = frame.result.frame;
        MediaPreviewFrame {
            width,
            height,
            frame: Some(frame),
            gpu_source: Some(MediaPreviewGpuSourceFrame::new(source, input_transform)),
            signature,
        }
    }

    fn test_media_frame_rgba8(frame: &MediaPreviewFrame) -> Vec<u8> {
        let working = frame.working_frame().expect("test media working frame");
        mondrian_renderer::execute_cpu_output_boundary(
            &working.frame,
            &RenderOutputColorBoundary::display(
                ColorSpace::Rec709,
                false,
                ColorEngine::MondrianSmart,
            ),
        )
        .expect("test media output transform")
        .result
        .frame
        .into_rgba()
    }

    fn stable_rgba_hash(rgba: &[u8]) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for byte in rgba {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    }

    #[test]
    fn media_preview_gpu_source_caches_lazy_cpu_working_transform() {
        let source =
            CpuEncodedColorFrame::source_rgba8(1, 1, ColorSpace::Rec709, vec![64, 128, 192, 255]);
        let input_transform =
            RenderInputTransform::to_working(ColorSpace::Rec709, false, ColorEngine::MondrianSmart);
        let frame = MediaPreviewFrame {
            width: 1,
            height: 1,
            frame: None,
            gpu_source: Some(MediaPreviewGpuSourceFrame::new(source, input_transform)),
            signature: 42,
        };

        let first = frame.working_frame().expect("first lazy working transform");
        let second = frame.working_frame().expect("cached lazy working transform");

        assert!(first.color_diagnostics.is_some());
        assert!(first.stage_diagnostics.cpu_input_stages > 0);
        assert!(second.color_diagnostics.is_none());
        assert_eq!(
            second.stage_diagnostics,
            RenderColorStageDiagnostics::default()
        );
        assert_eq!(
            first.frame.rgba_f32().data,
            second.frame.rgba_f32().data,
            "cached working transform must preserve the exact CPU fallback frame"
        );
    }

    fn test_proxy_config(cache_dir: PathBuf) -> mondrian_media::ProxyConfig {
        mondrian_media::ProxyConfig {
            cache_dir,
            ..mondrian_media::ProxyConfig::default()
        }
    }

    #[test]
    fn preview_media_decode_path_uses_existing_fresh_proxy() {
        let root = std::env::temp_dir().join(format!(
            "mondrian-preview-proxy-hit-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time")
                .as_nanos()
        ));
        let source = root.join("source.mp4");
        std::fs::create_dir_all(&root).expect("test root");
        std::fs::write(&source, b"source").expect("source");
        let proxy_config = test_proxy_config(root.join("proxy"));
        let proxy_path =
            mondrian_media::ProxyGenerator::new(proxy_config.clone()).proxy_path(&source);
        std::fs::create_dir_all(proxy_path.parent().expect("proxy parent")).expect("proxy root");
        std::fs::write(&proxy_path, b"proxy").expect("proxy");

        let resolved = resolve_preview_media_decode_path(true, &source, &proxy_config)
            .expect("fresh proxy path");

        assert_eq!(resolved.path, proxy_path);
        assert_eq!(resolved.resolution, PreviewMediaDecodePathResolution::Proxy);
        assert_eq!(resolved.fingerprint.len, Some(5));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn preview_media_decode_path_falls_back_when_proxy_missing() {
        let root = std::env::temp_dir().join(format!(
            "mondrian-preview-proxy-missing-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time")
                .as_nanos()
        ));
        let source = root.join("source.mp4");
        std::fs::create_dir_all(&root).expect("test root");
        std::fs::write(&source, b"source").expect("source");
        let proxy_config = test_proxy_config(root.join("proxy"));

        let resolved =
            resolve_preview_media_decode_path(true, &source, &proxy_config).expect("source path");

        assert_eq!(resolved.path, source);
        assert_eq!(
            resolved.resolution,
            PreviewMediaDecodePathResolution::ProxyMissing
        );
        assert_eq!(resolved.fingerprint.len, Some(6));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn preview_media_decode_path_rejects_stale_proxy() {
        let root = std::env::temp_dir().join(format!(
            "mondrian-preview-proxy-stale-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time")
                .as_nanos()
        ));
        let source = root.join("source.mp4");
        std::fs::create_dir_all(&root).expect("test root");
        let proxy_config = test_proxy_config(root.join("proxy"));
        let proxy_path =
            mondrian_media::ProxyGenerator::new(proxy_config.clone()).proxy_path(&source);
        std::fs::create_dir_all(proxy_path.parent().expect("proxy parent")).expect("proxy root");
        std::fs::write(&proxy_path, b"proxy").expect("proxy");
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&source, b"newer source").expect("source");

        let resolved = resolve_preview_media_decode_path(true, &source, &proxy_config)
            .expect("stale proxy falls back to source");

        assert_eq!(resolved.path, source);
        assert_eq!(
            resolved.resolution,
            PreviewMediaDecodePathResolution::ProxyStale
        );
        assert_eq!(resolved.fingerprint.len, Some(12));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn preview_media_decode_path_returns_none_when_source_missing() {
        let root = std::env::temp_dir().join(format!(
            "mondrian-preview-source-missing-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time")
                .as_nanos()
        ));
        let source = root.join("source.mp4");
        let proxy_config = test_proxy_config(root.join("proxy"));

        assert!(resolve_preview_media_decode_path(false, &source, &proxy_config).is_none());
        assert!(resolve_preview_media_decode_path(true, &source, &proxy_config).is_none());
    }

    #[test]
    fn preview_proxy_generation_requires_playback_proxy_mode_and_missing_proxy() {
        assert!(should_request_preview_proxy_generation(
            true,
            true,
            true,
            PreviewMediaDecodePathResolution::ProxyMissing
        ));
        assert!(should_request_preview_proxy_generation(
            true,
            true,
            true,
            PreviewMediaDecodePathResolution::ProxyStale
        ));
        assert!(!should_request_preview_proxy_generation(
            false,
            true,
            true,
            PreviewMediaDecodePathResolution::ProxyMissing
        ));
        assert!(!should_request_preview_proxy_generation(
            true,
            false,
            true,
            PreviewMediaDecodePathResolution::ProxyMissing
        ));
        assert!(!should_request_preview_proxy_generation(
            true,
            true,
            false,
            PreviewMediaDecodePathResolution::ProxyMissing
        ));
        assert!(!should_request_preview_proxy_generation(
            true,
            true,
            true,
            PreviewMediaDecodePathResolution::Proxy
        ));
        assert!(!should_request_preview_proxy_generation(
            true,
            true,
            true,
            PreviewMediaDecodePathResolution::Source
        ));
    }

    #[test]
    fn hardware_decode_request_is_playback_only_until_native_backend_supports_other_modes() {
        assert_eq!(
            preview_hardware_decode_request_for_access_mode(
                PreviewDecodeAccessMode::PlaybackCursor
            ),
            PreviewHardwareDecodeRequest::PreferGpuResident
        );
        assert_eq!(
            preview_hardware_decode_request_for_access_mode(PreviewDecodeAccessMode::ScrubCursor),
            PreviewHardwareDecodeRequest::Auto
        );
        assert_eq!(
            preview_hardware_decode_request_for_access_mode(
                PreviewDecodeAccessMode::RandomAccessStillFrame
            ),
            PreviewHardwareDecodeRequest::Auto
        );
    }

    #[test]
    fn scrub_adaptation_switches_for_hot_region_and_slow_latency() {
        let mut adaptation = PreviewScrubAdaptationState::default();
        let mut key = test_media_key(100);

        assert_eq!(
            adaptation.observe_request(&key).scrub_class,
            PreviewScrubAdaptiveClass::Normal
        );
        key.source_micros += 100_000;
        assert_eq!(
            adaptation.observe_request(&key).scrub_class,
            PreviewScrubAdaptiveClass::Normal
        );
        key.source_micros += 100_000;
        assert_eq!(
            adaptation.observe_request(&key).scrub_class,
            PreviewScrubAdaptiveClass::HotRegion
        );

        let slow_decode = PreviewDecodeDiagnostics {
            path: PreviewDecodePath::InProcessFfmpegCpuRgba,
            elapsed_us: PREVIEW_SCRUB_SLOW_LATENCY_US,
            cache_hit: false,
            access_mode: PreviewDecodeAccessMode::ScrubCursor,
            external_process: false,
            cpu_resident: true,
            seek_performed: true,
            seek_strategy: PreviewDecodeSeekStrategy::BoundedAnyFrame,
            forward_reuse_frame_window: 0,
            forward_decode_budget_frames: 0,
            any_seek_window_ms: 0,
            scrub_adaptive_class: PreviewScrubAdaptiveClass::Normal,
            hardware_decode_request: PreviewHardwareDecodeRequest::Auto,
            hardware_decode_decision: PreviewHardwareDecodeDecision::CpuRgbaNotRequested,
            hardware_decode_candidate_backend: None,
            hardware_decode_candidate_handle_kind: None,
            hardware_decode_adapter_available: false,
            session_reused: false,
            forward_reused: false,
            seek_index_available: false,
            seek_index_keyframes: 0,
            seek_index_observed_packets: 0,
            seek_index_source: PreviewSeekIndexSource::None,
            seek_index_used: false,
            seek_index_anchor_pts: None,
            decoded_frame_count: 0,
            threading_kind: PreviewDecodeThreadingKind::None,
            threading_count: 0,
            stage_durations: PreviewDecodeStageDurations::default(),
            hw_accel_backend: HwAccelBackend::None,
            hardware_decode_active: false,
            zero_copy_active: false,
            decoded_frame_residency: DecodedFrameResidency::CpuRgba,
            gpu_frame_handle_kind: None,
            renderer_import_ready: false,
            hardware_decode_blocker: PreviewHardwareDecodeBlocker::TextureResidencyNotConnected,
            decoded_surface_format: DecodedVideoSurfaceFormat::Unknown,
        };
        adaptation.observe_decode(slow_decode);
        adaptation.observe_decode(slow_decode);
        key.source_micros += 100_000;
        assert_eq!(
            adaptation.observe_request(&key).scrub_class,
            PreviewScrubAdaptiveClass::SlowLatency
        );
    }

    #[test]
    fn media_preview_cache_evicts_least_recently_used_frame() {
        let mut cache = MediaPreviewCache::new(2);
        let first = test_media_key(1);
        let second = test_media_key(2);
        let third = test_media_key(3);

        cache.insert(first.clone(), test_media_frame(1));
        cache.insert(second.clone(), test_media_frame(2));
        assert!(cache.get(&first).is_some());

        cache.insert(third.clone(), test_media_frame(3));

        assert_eq!(cache.len(), 2);
        assert!(cache.get(&first).is_some());
        assert!(cache.get(&second).is_none());
        assert!(cache.get(&third).is_some());
    }

    #[test]
    fn media_preview_cache_updates_existing_frame_without_growing() {
        let mut cache = MediaPreviewCache::new(2);
        let key = test_media_key(1);

        cache.insert(key.clone(), test_media_frame(1));
        cache.insert(key.clone(), test_media_frame(9));

        let frame = cache.get(&key).expect("updated frame");
        assert_eq!(cache.len(), 1);
        assert_eq!(test_media_frame_rgba8(&frame), vec![9, 0, 0, 255]);
    }

    #[test]
    fn media_preview_cache_clear_removes_frames_and_failures() {
        let mut cache = MediaPreviewCache::new(2);
        let mut failures = MediaPreviewFailureCache::new(2);
        let key = test_media_key(1);

        cache.insert(key.clone(), test_media_frame(1));
        failures.insert(key.clone());
        cache.clear();
        failures.clear();

        assert_eq!(cache.len(), 0);
        assert_eq!(failures.len(), 0);
        assert!(cache.get(&key).is_none());
        assert!(!failures.contains(&key));
    }

    #[test]
    fn media_preview_key_includes_file_length_in_identity() {
        let mut first = test_media_key(1);
        first.path = PathBuf::from("E:/media/replaced.mov");
        first.fingerprint = Some(PreviewFileFingerprint {
            len: Some(1_024),
            modified_secs: Some(10),
            modified_nanos: Some(20),
        });
        let mut second = first.clone();
        second.fingerprint = Some(PreviewFileFingerprint {
            len: Some(2_048),
            modified_secs: Some(10),
            modified_nanos: Some(20),
        });

        assert_ne!(first, second);
    }

    #[test]
    fn media_preview_cache_does_not_reuse_same_path_with_different_file_length() {
        let mut old_key = test_media_key(1);
        old_key.path = PathBuf::from("E:/media/replaced.mov");
        old_key.fingerprint = Some(PreviewFileFingerprint {
            len: Some(1_024),
            modified_secs: Some(10),
            modified_nanos: Some(20),
        });
        let mut new_key = old_key.clone();
        new_key.fingerprint = Some(PreviewFileFingerprint {
            len: Some(2_048),
            modified_secs: Some(10),
            modified_nanos: Some(20),
        });
        let mut cache = MediaPreviewCache::new(2);

        cache.insert(old_key.clone(), test_media_frame(1));

        assert!(cache.get(&new_key).is_none());
        assert!(cache.get(&old_key).is_some());
    }

    #[test]
    fn media_preview_failure_cache_evicts_least_recently_used_key() {
        let mut cache = MediaPreviewFailureCache::new(2);
        let first = test_media_key(1);
        let second = test_media_key(2);
        let third = test_media_key(3);

        cache.insert(first.clone());
        cache.insert(second.clone());
        assert!(cache.contains(&first));

        cache.insert(third.clone());

        assert_eq!(cache.len(), 2);
        assert!(cache.contains(&first));
        assert!(!cache.contains(&second));
        assert!(cache.contains(&third));
    }

    #[test]
    fn media_preview_failure_cache_updates_existing_key_without_growing() {
        let mut cache = MediaPreviewFailureCache::new(2);
        let key = test_media_key(1);

        cache.insert(key.clone());
        cache.insert(key.clone());

        assert_eq!(cache.len(), 1);
        assert!(cache.contains(&key));
    }

    #[test]
    fn preview_service_cancel_interactive_work_clears_pending_and_cached_state() {
        let service = AppUiPreviewService::new_without_workers_for_test();
        let key = test_media_key(1);
        let generation = service.scheduler.begin_generation();
        assert_eq!(
            service.scheduler.request(
                key.clone(),
                generation,
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::ScrubCursor,
            ),
            MediaPreviewRequestStatus::Scheduled { evicted_prefetch: None, evicted_still: None }
        );
        assert_eq!(
            service.jobs.enqueue(MediaPreviewJob {
                key: key.clone(),
                source_secs: 1.0,
                generation,
                priority: MediaPreviewRequestPriority::Current,
                access_mode: PreviewDecodeAccessMode::ScrubCursor,
                adaptive_hints: PreviewDecodeAdaptiveHints::default(),
                enqueued_at: Instant::now(),
                deadline_at: None,
            }),
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        );
        service.media_cache.borrow_mut().insert(key.clone(), test_media_frame(1));
        service.media_failures.borrow_mut().insert(key.clone());

        service.cancel_interactive_work();

        assert_eq!(service.scheduler.pending_len(), 0);
        assert!(!service.scheduler.is_decode_current(
            &key,
            generation,
            PreviewDecodeAccessMode::ScrubCursor
        ));
        let diagnostics = service.diagnostics();
        assert_eq!(diagnostics.interactive_cancel_requests, 1);
        assert_eq!(diagnostics.interactive_cancel_scheduler_requests, 1);
        assert_eq!(diagnostics.interactive_cancel_queued_jobs, 1);
        assert_eq!(diagnostics.queue_canceled_jobs, 1);
        assert_eq!(service.media_cache.borrow().len(), 0);
        assert_eq!(service.media_failures.borrow().len(), 0);
        service.shutdown();
    }

    #[test]
    fn preview_service_completion_poll_respects_result_count_budget() {
        let service = AppUiPreviewService::new_without_workers_for_test();
        let results = install_preview_result_channel_for_test(&service);
        let generation = service.scheduler.begin_generation();
        for index in 0..3 {
            let key = test_media_key(index);
            assert_eq!(
                service.scheduler.request(
                    key.clone(),
                    generation,
                    MediaPreviewRequestPriority::Current,
                    PreviewDecodeAccessMode::ScrubCursor,
                ),
                MediaPreviewRequestStatus::Scheduled {
                    evicted_prefetch: None,
                    evicted_still: None
                }
            );
            results
                .send(test_successful_media_preview_result(
                    key,
                    generation,
                    index as u8,
                ))
                .expect("send test preview result");
        }

        assert!(service.poll_finished_with_budget(2, Duration::from_secs(1)));
        let diagnostics = service.diagnostics();
        assert_eq!(diagnostics.decode_successes, 2);
        assert_eq!(diagnostics.completion_poll_calls, 1);
        assert_eq!(diagnostics.completion_poll_results, 2);
        assert_eq!(diagnostics.completion_poll_max_results_per_poll, 2);
        assert_eq!(diagnostics.completion_poll_count_budget_exhaustions, 1);
        assert_eq!(diagnostics.completion_poll_time_budget_exhaustions, 0);
        assert_eq!(service.scheduler.pending_len(), 1);

        assert!(service.poll_finished_with_budget(2, Duration::from_secs(1)));
        let diagnostics = service.diagnostics();
        assert_eq!(diagnostics.decode_successes, 3);
        assert_eq!(diagnostics.completion_poll_calls, 2);
        assert_eq!(diagnostics.completion_poll_results, 3);
        assert_eq!(diagnostics.completion_poll_count_budget_exhaustions, 1);
        assert_eq!(service.scheduler.pending_len(), 0);
        service.shutdown();
    }

    #[test]
    fn preview_service_completion_poll_respects_time_budget() {
        let service = AppUiPreviewService::new_without_workers_for_test();
        let results = install_preview_result_channel_for_test(&service);
        let generation = service.scheduler.begin_generation();
        for index in 0..2 {
            let key = test_media_key(index);
            assert_eq!(
                service.scheduler.request(
                    key.clone(),
                    generation,
                    MediaPreviewRequestPriority::Current,
                    PreviewDecodeAccessMode::ScrubCursor,
                ),
                MediaPreviewRequestStatus::Scheduled {
                    evicted_prefetch: None,
                    evicted_still: None
                }
            );
            results
                .send(test_successful_media_preview_result(
                    key,
                    generation,
                    index as u8,
                ))
                .expect("send test preview result");
        }

        assert!(service.poll_finished_with_budget(8, Duration::ZERO));

        let diagnostics = service.diagnostics();
        assert_eq!(diagnostics.decode_successes, 1);
        assert_eq!(diagnostics.completion_poll_calls, 1);
        assert_eq!(diagnostics.completion_poll_results, 1);
        assert_eq!(diagnostics.completion_poll_count_budget_exhaustions, 0);
        assert_eq!(diagnostics.completion_poll_time_budget_exhaustions, 1);
        assert_eq!(service.scheduler.pending_len(), 1);
        service.shutdown();
    }

    #[test]
    fn preview_worker_activity_tracks_in_flight_jobs_by_access_mode() {
        let activity = Arc::new(PreviewWorkerActivity::default());
        {
            let _playback = activity.begin(
                MediaPreviewWorkerLane::Playback,
                MediaPreviewRequestPriority::Prefetch,
                PreviewDecodeAccessMode::PlaybackCursor,
            );
            let _scrub = activity.begin(
                MediaPreviewWorkerLane::Scrub,
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::ScrubCursor,
            );
            let snapshot = activity.snapshot();
            assert_eq!(snapshot.in_flight_jobs, 2);
            assert_eq!(snapshot.in_flight_current_jobs, 1);
            assert_eq!(snapshot.in_flight_prefetch_jobs, 1);
            assert_eq!(snapshot.in_flight_playback_cursor_jobs, 1);
            assert_eq!(snapshot.in_flight_scrub_cursor_jobs, 1);
            assert_eq!(snapshot.in_flight_random_access_still_jobs, 0);
            assert_eq!(snapshot.in_flight_playback_lane_jobs, 1);
            assert_eq!(snapshot.in_flight_scrub_lane_jobs, 1);
            assert_eq!(snapshot.in_flight_still_lane_jobs, 0);
            assert_eq!(snapshot.in_flight_interactive_lane_jobs, 0);
            assert_eq!(snapshot.in_flight_cross_lane_current_jobs, 0);
        }

        assert_eq!(
            activity.snapshot(),
            PreviewWorkerActivityDiagnostics::default()
        );
    }

    #[test]
    fn preview_worker_activity_tracks_cross_lane_current_overflow() {
        let activity = Arc::new(PreviewWorkerActivity::default());
        {
            let _scrub_on_playback_lane = activity.begin(
                MediaPreviewWorkerLane::Playback,
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::ScrubCursor,
            );
            let snapshot = activity.snapshot();
            assert_eq!(snapshot.in_flight_jobs, 1);
            assert_eq!(snapshot.in_flight_current_jobs, 1);
            assert_eq!(snapshot.in_flight_scrub_cursor_jobs, 1);
            assert_eq!(snapshot.in_flight_playback_lane_jobs, 1);
            assert_eq!(snapshot.in_flight_cross_lane_current_jobs, 1);
        }

        assert_eq!(
            activity.snapshot(),
            PreviewWorkerActivityDiagnostics::default()
        );
    }

    #[test]
    fn media_preview_job_deadline_is_only_for_current_playback() {
        let now = Instant::now();

        assert!(media_preview_job_deadline_at(
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::PlaybackCursor,
            now,
            Some(33_333),
        )
        .is_some());
        assert_eq!(
            media_preview_job_deadline_at(
                MediaPreviewRequestPriority::Prefetch,
                PreviewDecodeAccessMode::PlaybackCursor,
                now,
                Some(33_333),
            ),
            None
        );
        assert_eq!(
            media_preview_job_deadline_at(
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::ScrubCursor,
                now,
                Some(33_333),
            ),
            None
        );
        assert_eq!(
            media_preview_job_deadline_at(
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::RandomAccessStillFrame,
                now,
                Some(33_333),
            ),
            None
        );
        assert_eq!(
            media_preview_job_deadline_at(
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::PlaybackCursor,
                now,
                None,
            ),
            None
        );
    }

    #[test]
    fn media_preview_playback_deadline_budget_uses_sequence_frame_duration() {
        assert_eq!(
            media_preview_playback_current_deadline_budget_us(Rational::FPS_25),
            Some(40_000)
        );
        assert_eq!(
            media_preview_playback_current_deadline_budget_us(Rational::FPS_30),
            Some(33_333)
        );
        assert_eq!(
            media_preview_playback_current_deadline_budget_us(Rational::FPS_60),
            Some(16_667)
        );
        assert_eq!(
            media_preview_playback_current_deadline_budget_us(Rational::new(240, 1)),
            Some(MEDIA_PREVIEW_PLAYBACK_CURRENT_MIN_DEADLINE_US)
        );
        assert_eq!(
            media_preview_playback_current_deadline_budget_us(Rational::FPS_10),
            Some(MEDIA_PREVIEW_PLAYBACK_CURRENT_MAX_DEADLINE_US)
        );
        assert_eq!(
            media_preview_playback_current_deadline_budget_us(Rational::new(0, 1)),
            None
        );
        assert_eq!(
            media_preview_playback_current_deadline_budget_us(Rational::new(24, 0)),
            None
        );
    }

    #[test]
    fn media_preview_forward_prefetch_window_uses_sequence_frame_rate() {
        assert_eq!(
            media_preview_forward_prefetch_window_frames(Rational::FPS_24),
            Some(2)
        );
        assert_eq!(
            media_preview_forward_prefetch_window_frames(Rational::FPS_30),
            Some(2)
        );
        assert_eq!(
            media_preview_forward_prefetch_window_frames(Rational::FPS_60),
            Some(5)
        );
        assert_eq!(
            media_preview_forward_prefetch_window_frames(Rational::new(240, 1)),
            Some(MEDIA_PREVIEW_FORWARD_PREFETCH_MAX_FRAMES)
        );
        assert_eq!(
            media_preview_forward_prefetch_window_frames(Rational::FPS_10),
            Some(MEDIA_PREVIEW_FORWARD_PREFETCH_MIN_FRAMES)
        );
        assert_eq!(
            media_preview_forward_prefetch_window_frames(Rational::new(0, 1)),
            None
        );
        assert_eq!(
            media_preview_forward_prefetch_window_frames(Rational::new(24, 0)),
            None
        );
    }

    #[test]
    fn media_preview_decode_cancellation_keeps_current_frame_unbudgeted() {
        assert_eq!(
            media_preview_cancel_reason(
                false,
                true,
                true,
                true,
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::ScrubCursor,
                Duration::from_micros(MEDIA_PREVIEW_PREFETCH_DECODE_BUDGET_US * 4),
                false,
            ),
            None,
        );
    }

    #[test]
    fn media_preview_decode_cancellation_reports_shutdown() {
        assert_eq!(
            media_preview_cancel_reason(
                true,
                true,
                true,
                true,
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::ScrubCursor,
                Duration::ZERO,
                false,
            ),
            Some(MediaPreviewCancelReason::Shutdown),
        );
    }

    #[test]
    fn preview_service_shutdown_does_not_block_on_busy_worker() {
        let service = AppUiPreviewService::new_without_workers_for_test();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();

        let handle = thread::spawn(move || {
            entered_tx.send(()).expect("signal worker started");
            let _ = release_rx.recv_timeout(Duration::from_secs(5));
        });
        service.workers.borrow_mut().push(handle);
        entered_rx.recv_timeout(Duration::from_secs(1)).expect("worker should start");

        let started_at = Instant::now();
        service.shutdown();
        assert!(
            started_at.elapsed() < Duration::from_millis(100),
            "preview shutdown must not block the UI event loop while decode workers exit"
        );

        release_tx.send(()).expect("release worker");
    }

    #[test]
    fn media_preview_decode_cancellation_budgets_prefetch_work() {
        assert_eq!(
            media_preview_cancel_reason(
                false,
                true,
                false,
                false,
                MediaPreviewRequestPriority::Prefetch,
                PreviewDecodeAccessMode::PlaybackCursor,
                Duration::from_micros(MEDIA_PREVIEW_PREFETCH_DECODE_BUDGET_US - 1),
                false,
            ),
            None,
        );
        assert_eq!(
            media_preview_cancel_reason(
                false,
                true,
                false,
                false,
                MediaPreviewRequestPriority::Prefetch,
                PreviewDecodeAccessMode::PlaybackCursor,
                Duration::from_micros(MEDIA_PREVIEW_PREFETCH_DECODE_BUDGET_US),
                false,
            ),
            Some(MediaPreviewCancelReason::PrefetchDeadline),
        );
        assert_eq!(
            media_preview_cancel_reason(
                false,
                true,
                true,
                true,
                MediaPreviewRequestPriority::Prefetch,
                PreviewDecodeAccessMode::ScrubCursor,
                Duration::from_micros(MEDIA_PREVIEW_PREFETCH_DECODE_BUDGET_US * 4),
                false,
            ),
            None,
        );
    }

    #[test]
    fn media_preview_decode_cancellation_preempts_prefetch_for_current_work() {
        assert_eq!(
            media_preview_cancel_reason(
                false,
                true,
                true,
                true,
                MediaPreviewRequestPriority::Prefetch,
                PreviewDecodeAccessMode::PlaybackCursor,
                Duration::ZERO,
                false,
            ),
            Some(MediaPreviewCancelReason::PrefetchPreemptedByCurrent),
        );
        assert_eq!(
            media_preview_cancel_reason(
                false,
                true,
                false,
                false,
                MediaPreviewRequestPriority::Prefetch,
                PreviewDecodeAccessMode::PlaybackCursor,
                Duration::from_micros(MEDIA_PREVIEW_PREFETCH_DECODE_BUDGET_US - 1),
                false,
            ),
            None,
        );
    }

    #[test]
    fn media_preview_decode_cancellation_preempts_still_for_realtime_current_work() {
        assert_eq!(
            media_preview_cancel_reason(
                false,
                true,
                true,
                true,
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::RandomAccessStillFrame,
                Duration::ZERO,
                false,
            ),
            Some(MediaPreviewCancelReason::StillPreemptedByRealtimeCurrent),
        );
        assert_eq!(
            media_preview_cancel_reason(
                false,
                true,
                true,
                false,
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::RandomAccessStillFrame,
                Duration::from_micros(MEDIA_PREVIEW_PREFETCH_DECODE_BUDGET_US * 4),
                false,
            ),
            None,
        );
    }

    #[test]
    fn media_preview_decode_cancellation_stops_stale_work() {
        assert_eq!(
            media_preview_cancel_reason(
                false,
                false,
                true,
                true,
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::ScrubCursor,
                Duration::ZERO,
                false,
            ),
            Some(MediaPreviewCancelReason::Obsolete),
        );
        assert_eq!(
            media_preview_cancel_reason(
                false,
                false,
                true,
                true,
                MediaPreviewRequestPriority::Prefetch,
                PreviewDecodeAccessMode::PlaybackCursor,
                Duration::ZERO,
                false,
            ),
            Some(MediaPreviewCancelReason::Obsolete),
        );
    }

    #[test]
    fn media_preview_decode_cancellation_drops_late_playback_current_work() {
        assert_eq!(
            media_preview_cancel_reason(
                false,
                true,
                false,
                false,
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::PlaybackCursor,
                Duration::ZERO,
                true,
            ),
            Some(MediaPreviewCancelReason::PlaybackDeadline),
        );
        assert_eq!(
            media_preview_cancel_reason(
                false,
                true,
                false,
                false,
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::ScrubCursor,
                Duration::ZERO,
                true,
            ),
            None,
        );
    }
}
