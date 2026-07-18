//! Viewer preview service for the app UI host.
//!
//! The service coordinates private timeline-evaluation, media-adaptation,
//! Viewer-plan, media-execution, and diagnostics Modules through one host-facing
//! Interface. Panels stay read-only and only consume renderer-ready Viewer
//! frame content.

use std::cell::{Cell, RefCell};
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
#[cfg(test)]
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use mondrian_assets::AssetKind;
use mondrian_core::display_contract::{DisplayOutputSnapshot, MonitorProfileStatus};
use mondrian_core::timeline_data::{AlphaInterpretation, AssetMediaInterpretation};
#[cfg(test)]
use mondrian_core::types::ColorEngine;
use mondrian_core::types::{AssetId, BlendMode, ColorSpace, Rational, SequenceId};
use mondrian_core::{MondrianError, Resolution, WorkingColorSpace};
use mondrian_effects::{
    get_or_lower_effect_graph_to_gpu_plan, CompiledEffectGraph, EffectCachePolicy,
};
use mondrian_media::{
    decode_preview_frame_cancellable, preview_decode_cpu_budget, DecodedFrameResidency,
    DecodedGpuFrameHandleKind, DecodedVideoRange, DecodedVideoRangeContract, DecodedVideoSampling,
    DecodedVideoSurfaceFormat, HwAccelBackend, HwAccelDeviceSelector, PreviewDecodeAccessMode,
    PreviewDecodeAdaptiveHints, PreviewDecodeCpuBudget, PreviewDecodeDiagnostics,
    PreviewDecodeExecutionPath, PreviewDecodeOutcome, PreviewDecodePath, PreviewDecodeRequest,
    PreviewDecodeSeekStrategy, PreviewDecodeStageDurations, PreviewDecodeThreadingKind,
    PreviewFileFingerprint, PreviewHardwareDecodeBlocker, PreviewHardwareDecodeCpuTransferStatus,
    PreviewHardwareDecodeDecision, PreviewHardwareDecodeRequest, PreviewNativeDecodedFrame,
    PreviewScrubAdaptiveClass, PreviewSeekIndexSource, PreviewSourceColorContract,
    VideoColorDiagnostic, VideoColorDiagnosticIssueSummary,
};
#[cfg(test)]
use mondrian_media::{DecodedVideoChromaLocation, PreviewNativeDecodedFrameHandle};
use mondrian_renderer::{
    color_report_vocab, composite_timeline_elements_color_frame_with_diagnostics,
    evaluate_timeline_render_plan, execute_cpu_program_monitor_boundary_rgba8,
    execute_cpu_source_input_stage, execute_cpu_working_transform,
    project_affine_to_sampled_extents, CpuColorFrame, CpuEncodedColorFrame, CpuSourceColorFrame,
    GpuCompositingBlockerReason, GpuCompositingDiagnostics, LinearFloatSource,
    RenderColorStageDiagnostics, RenderColorStageGpuBlockerBreakdown,
    RenderColorTransformDiagnostics, RenderColorTransformDirection, RenderInputTransform,
    RenderMonitorAdaptation, RenderOutputColorBoundary, TimelineAdjustmentLayer,
    TimelineCompositeColorPathSummary, TimelineCompositeDiagnostics,
    TimelineCompositeDomainBlockerBreakdown, TimelineCompositeElement,
    TimelineCompositeLegacyBreakdown, TimelineCompositeOptions, TimelineCompositeScratch,
    TimelineEffectColorRuntime, TimelineEvaluationRequest, TimelineMediaLayer,
    TimelineRenderPlanElement, TimelineSolidColorLayer,
};
#[cfg(test)]
use mondrian_renderer::{
    execute_cpu_input_stage, GpuNativeDecodedFrameImportSource, GpuNativeDecodedFrameTextureFormat,
    TimelineCompositeColorPath, ViewerGpuExecutionLayer,
};
use mondrian_timeline::sequence::{
    ColorContext, InputColorResolution, InputColorResolutionSource,
    InputColorResolutionSourceCounts, MissingColorMetadataPolicy, ResolvedInputColor, Sequence,
    MAX_NESTED_SEQUENCE_RENDER_DEPTH,
};
use mondrian_ui_widgets::{
    ViewerExternalTextureFrame, ViewerExternalTexturePresentation, ViewerFrameContent,
    ViewerFrameImage,
};

use crate::app::playback_preview::{PlaybackPreviewAdapter, PreviewVideoPreroll, PreviewWorkPoll};
use crate::app::preview_access_mode::{
    media_preview_access_mode_for_intent, media_preview_cancel_reason_at_checkpoint,
    media_preview_cancel_reason_from_execution, media_preview_cancel_request_to_observed_us,
    media_preview_frame_work_class, media_preview_viewer_access_intent, media_preview_worker_count,
    media_preview_worker_lane, MediaPreviewCancelReason, MediaPreviewJob,
    MediaPreviewJobQueueDiagnostics, MediaPreviewJobQueueReceive, MediaPreviewJobQueueReceiver,
    MediaPreviewJobQueueSender, MediaPreviewJobQueueWait, MediaPreviewKey,
    MediaPreviewNativeSurfaceHint, MediaPreviewRequestPriority, MediaPreviewRequestStatus,
    MediaPreviewScheduler, MediaPreviewSchedulerDiagnostics, MediaPreviewWorkerLane,
    MEDIA_PREVIEW_DECODE_SESSION_IDLE_TIMEOUT,
};
#[cfg(test)]
use crate::app::preview_access_mode::{
    media_preview_cancel_reason, MediaPreviewJobEnqueueStatus,
    MEDIA_PREVIEW_PREFETCH_DECODE_BUDGET_US,
};
use crate::app::preview_scheduler_policy::{
    media_preview_forward_prefetch_window_frames, playback_frame_delivery_kind,
    playback_hardware_recovery_signals, preview_decode_presentation_quality,
    MediaPreviewFailureReason, PlaybackDecodeExecution, PlaybackPressureState,
    PlaybackPressureTransition, PreviewScrubAdaptationState,
    MEDIA_PREVIEW_FORWARD_PREFETCH_HORIZON_US, MEDIA_PREVIEW_FORWARD_PREFETCH_MAX_FRAMES,
    MEDIA_PREVIEW_FORWARD_PREFETCH_MIN_FRAMES,
};
#[cfg(test)]
use crate::app::preview_scheduler_policy::{
    MEDIA_PREVIEW_PLAYBACK_PRESSURE_LATE_STREAK_THRESHOLD, PREVIEW_SCRUB_SLOW_LATENCY_US,
};
use crate::app::proxy_generation::{request_proxy_generation, resolve_asset_proxy_color_contract};
use crate::app::AppState;
use crate::app_ui::native_video_import::AppUiPlaybackHardwareDecodeAdmission;
use crate::app_ui::panels::{
    ViewerColorPipelineStatus, ViewerPreviewColorRejectionModel, ViewerPreviewSource,
    ViewerPreviewState,
};
use crate::app_ui::preview_frame_store::PreviewCpuFrameStore;
#[cfg(test)]
use crate::app_ui::preview_frame_store::PreviewCpuFrameStoreConfig;
use crate::app_ui::preview_gpu_output_blocker::PreviewGpuOutputBlocker;
use crate::app_ui::preview_scale::normalize_preview_resolution_scale;

const MEDIA_PREVIEW_PLAYBACK_BUFFERING_STALL_TIMEOUT_US: u64 = 250_000;
const MEDIA_PREVIEW_MAX_COMPLETED_RESULTS_PER_POLL: usize = 8;
const MEDIA_PREVIEW_COMPLETED_RESULTS_POLL_BUDGET_US: u64 = 2_000;

/// Host-owned preview renderer used by the app UI viewer panel.
///
/// This first path renders solid-color render-plan elements through the shared
/// renderer compositor. Media and nested-sequence decode can attach here without
/// changing panel models or widget APIs.
pub struct AppUiPreviewService {
    jobs: MediaPreviewJobQueueSender,
    results: RefCell<mpsc::Receiver<MediaPreviewResult>>,
    workers: RefCell<Vec<JoinHandle<()>>>,
    shutdown: Arc<PreviewShutdownSignal>,
    frame_store: RefCell<PreviewCpuFrameStore>,
    requested_proxy_generations: RefCell<HashSet<PreviewProxyGenerationRequestKey>>,
    scrub_adaptation: RefCell<PreviewScrubAdaptationState>,
    external_viewer_frame: RefCell<Option<ScopedExternalViewerFrame>>,
    current_presentation_quality: Cell<mondrian_playback::FramePresentationQuality>,
    playback_pressure: Cell<PlaybackPressureState>,
    next_gpu_preview_candidate_id: Cell<u64>,
    scheduler: MediaPreviewScheduler,
    scratch: RefCell<TimelineCompositeScratch>,
    current_generation: Cell<u64>,
    current_frame_pending: Cell<bool>,
    last_color_rejection: RefCell<Option<AppUiPreviewColorRejection>>,
    display_snapshot: RefCell<Option<DisplayOutputSnapshot>>,
    last_generation_key: RefCell<Option<ViewerPreviewGenerationKey>>,
    playback_hardware_decode_request: Cell<PreviewHardwareDecodeRequest>,
    playback_hardware_decode_device_selector: Cell<Option<HwAccelDeviceSelector>>,
    playback_hardware_decode_renderer_import_known: Cell<bool>,
    playback_hardware_decode_renderer_import_ready: Cell<bool>,
    playback_hardware_decode_platform_import_ready: Cell<bool>,
    playback_hardware_decode_native_import_admission_ready: Cell<bool>,
    playback_hardware_decode_admission_blocker:
        Cell<Option<AppUiPreviewHardwareDecodeAdmissionBlocker>>,
    playback_hardware_decode_platform_discovery_available: Cell<bool>,
    playback_hardware_decode_platform_zero_copy_supported: Cell<bool>,
    playback_hardware_decode_platform_low_copy_fallback_supported: Cell<bool>,
    playback_hardware_decode_renderer_supported_handle_kinds: Cell<u8>,
    playback_hardware_decode_renderer_supported_source_texture_formats: Cell<u8>,
    playback_hardware_decode_renderer_supports_nv12: Cell<bool>,
    playback_hardware_decode_renderer_supports_p010: Cell<bool>,
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
        let scheduler = MediaPreviewScheduler::default();
        let (job_tx, job_rx) = scheduler.job_queue();
        let (result_tx, result_rx) = mpsc::channel::<MediaPreviewResult>();
        let shutdown = Arc::new(PreviewShutdownSignal::default());
        let mut decode_worker_count = 0;
        let mut workers = Vec::new();
        for worker_index in 0..worker_count {
            let worker_jobs = job_rx.clone();
            let worker_results = result_tx.clone();
            let worker_scheduler = scheduler.clone();
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
            frame_store: RefCell::new(PreviewCpuFrameStore::default()),
            requested_proxy_generations: RefCell::new(HashSet::new()),
            scrub_adaptation: RefCell::new(PreviewScrubAdaptationState::default()),
            external_viewer_frame: RefCell::new(None),
            current_presentation_quality: Cell::new(
                mondrian_playback::FramePresentationQuality::Ready,
            ),
            playback_pressure: Cell::new(PlaybackPressureState::default()),
            next_gpu_preview_candidate_id: Cell::new(0),
            scheduler,
            scratch: RefCell::new(TimelineCompositeScratch::default()),
            current_generation: Cell::new(0),
            current_frame_pending: Cell::new(false),
            last_color_rejection: RefCell::new(None),
            display_snapshot: RefCell::new(None),
            last_generation_key: RefCell::new(None),
            playback_hardware_decode_request: Cell::new(PreviewHardwareDecodeRequest::Auto),
            playback_hardware_decode_device_selector: Cell::new(None),
            playback_hardware_decode_renderer_import_known: Cell::new(false),
            playback_hardware_decode_renderer_import_ready: Cell::new(false),
            playback_hardware_decode_platform_import_ready: Cell::new(false),
            playback_hardware_decode_native_import_admission_ready: Cell::new(false),
            playback_hardware_decode_admission_blocker: Cell::new(None),
            playback_hardware_decode_platform_discovery_available: Cell::new(false),
            playback_hardware_decode_platform_zero_copy_supported: Cell::new(false),
            playback_hardware_decode_platform_low_copy_fallback_supported: Cell::new(false),
            playback_hardware_decode_renderer_supported_handle_kinds: Cell::new(0),
            playback_hardware_decode_renderer_supported_source_texture_formats: Cell::new(0),
            playback_hardware_decode_renderer_supports_nv12: Cell::new(false),
            playback_hardware_decode_renderer_supports_p010: Cell::new(false),
            decode_cpu_budget,
            decode_worker_count,
            metrics: AppUiPreviewMetrics::default(),
        }
    }

    /// Set playback hardware-decode admission selected by the app runtime.
    ///
    /// The default is `Auto` until renderer/platform readiness is reported. The
    /// runtime may raise playback to `PreferHardwareDecode` for FFmpeg
    /// CPU-transfer fallback or to `PreferGpuResident` once native video import
    /// support is actually ready.
    pub(crate) fn set_playback_hardware_decode_admission(
        &self,
        admission: AppUiPlaybackHardwareDecodeAdmission,
    ) {
        self.playback_hardware_decode_request.set(admission.request);
        self.playback_hardware_decode_device_selector
            .set(admission.hardware_decode_device_selector);
        self.playback_hardware_decode_renderer_import_known.set(true);
        self.playback_hardware_decode_renderer_import_ready
            .set(admission.renderer_native_import_ready);
        self.playback_hardware_decode_platform_import_ready
            .set(admission.platform_native_import_ready);
        self.playback_hardware_decode_native_import_admission_ready
            .set(admission.native_import_admission_ready);
        self.playback_hardware_decode_admission_blocker.set(admission.admission_blocker);
        self.playback_hardware_decode_platform_discovery_available
            .set(admission.platform_discovery_available);
        self.playback_hardware_decode_platform_zero_copy_supported
            .set(admission.platform_zero_copy_supported);
        self.playback_hardware_decode_platform_low_copy_fallback_supported
            .set(admission.platform_low_copy_fallback_supported);
        self.playback_hardware_decode_renderer_supported_handle_kinds
            .set(admission.renderer_supported_handle_kinds);
        self.playback_hardware_decode_renderer_supported_source_texture_formats
            .set(admission.renderer_supported_source_texture_formats);
        self.playback_hardware_decode_renderer_supports_nv12
            .set(admission.renderer_supports_nv12);
        self.playback_hardware_decode_renderer_supports_p010
            .set(admission.renderer_supports_p010);
    }

    #[cfg(test)]
    pub(crate) fn playback_hardware_decode_request_for_test(&self) -> PreviewHardwareDecodeRequest {
        self.playback_hardware_decode_request.get()
    }

    fn hardware_decode_admission_diagnostics(
        &self,
    ) -> AppUiPreviewHardwareDecodeAdmissionDiagnostics {
        AppUiPreviewHardwareDecodeAdmissionDiagnostics {
            playback_request: self.playback_hardware_decode_request.get(),
            renderer_native_import_support_known: self
                .playback_hardware_decode_renderer_import_known
                .get(),
            renderer_native_import_ready: self.playback_hardware_decode_renderer_import_ready.get(),
            platform_native_import_ready: self.playback_hardware_decode_platform_import_ready.get(),
            native_import_admission_ready: self
                .playback_hardware_decode_native_import_admission_ready
                .get(),
            admission_blocker: self.playback_hardware_decode_admission_blocker.get(),
            platform_discovery_available: self
                .playback_hardware_decode_platform_discovery_available
                .get(),
            platform_zero_copy_supported: self
                .playback_hardware_decode_platform_zero_copy_supported
                .get(),
            platform_low_copy_fallback_supported: self
                .playback_hardware_decode_platform_low_copy_fallback_supported
                .get(),
            renderer_supported_handle_kinds: self
                .playback_hardware_decode_renderer_supported_handle_kinds
                .get(),
            renderer_supported_source_texture_formats: self
                .playback_hardware_decode_renderer_supported_source_texture_formats
                .get(),
        }
    }

    fn hardware_decode_request_for_access_mode(
        &self,
        _access_mode: PreviewDecodeAccessMode,
    ) -> PreviewHardwareDecodeRequest {
        self.playback_hardware_decode_request.get()
    }

    fn hardware_decode_request_for_key(
        &self,
        access_mode: PreviewDecodeAccessMode,
        key: &MediaPreviewKey,
    ) -> PreviewHardwareDecodeRequest {
        let request = self.hardware_decode_request_for_access_mode(access_mode);
        if request != PreviewHardwareDecodeRequest::PreferGpuResident {
            return request;
        }
        let supported = match key.native_surface_hint {
            Some(MediaPreviewNativeSurfaceHint::Nv12) => {
                self.playback_hardware_decode_renderer_supports_nv12.get()
            }
            Some(MediaPreviewNativeSurfaceHint::P010) => {
                self.playback_hardware_decode_renderer_supports_p010.get()
            }
            None => true,
        };
        if supported {
            request
        } else {
            PreviewHardwareDecodeRequest::PreferHardwareDecode
        }
    }

    fn hardware_decode_device_selector_for_access_mode(
        &self,
        _access_mode: PreviewDecodeAccessMode,
    ) -> Option<HwAccelDeviceSelector> {
        self.playback_hardware_decode_device_selector.get()
    }

    #[cfg(test)]
    pub(crate) fn seed_pending_preview_work_for_test(&self) {
        self.seed_pending_preview_work_with_access_mode_for_test(
            PreviewDecodeAccessMode::ScrubCursor,
            None,
        );
    }

    #[cfg(test)]
    pub(crate) fn seed_pending_playback_current_preview_work_for_test(
        &self,
        demand_identity: mondrian_playback::FrameDemandIdentity,
    ) {
        self.seed_pending_preview_work_with_access_mode_for_test(
            PreviewDecodeAccessMode::PlaybackCursor,
            Some(demand_identity),
        );
    }

    #[cfg(test)]
    fn test_frame_demand_identity() -> mondrian_playback::FrameDemandIdentity {
        let mut engine = mondrian_playback::PlaybackEngine::new(
            Rational::new(1, 25),
            mondrian_playback::PlaybackPolicy::default(),
        )
        .expect("playback engine");
        engine
            .play_timeline(
                mondrian_playback::PlaybackTimelineBinding::new(None, 0, Rational::new(1, 25), 10)
                    .expect("timeline binding"),
                mondrian_core::FramePosition::new(0, Rational::new(1, 25)),
                mondrian_playback::MonotonicTimestamp::ZERO,
            )
            .expect("playback demand");
        engine.frame_demand().expect("frame demand").identity()
    }

    #[cfg(test)]
    fn seed_pending_preview_work_with_access_mode_for_test(
        &self,
        access_mode: PreviewDecodeAccessMode,
        demand_identity: Option<mondrian_playback::FrameDemandIdentity>,
    ) {
        let key = MediaPreviewKey {
            asset_id: AssetId::new(),
            path: PathBuf::from("E:/media/pending-preview.mov"),
            fingerprint: None,
            source_frame: 0,
            source_micros: 0,
            target_width: 1920,
            target_height: 1080,
            source_width: 1920,
            source_height: 1080,
            input_color_space: ColorSpace::Srgb,
            input_video_range: DecodedVideoRangeContract::Automatic {
                probed_range: DecodedVideoRange::Full,
            },
            native_surface_hint: None,
            source_has_alpha: false,
            alpha_interpretation: AlphaInterpretation::Straight,
            working_color_space: WorkingColorSpace::LinearRec709,
            tone_map: false,
            engine: ColorEngine::mondrian_standard(),
            ocio_generation: mondrian_core::ocio_config_generation(),
        };
        let generation = self.scheduler.begin_generation();
        let _ = self.scheduler.request_with_demand_identity(
            key.clone(),
            generation,
            MediaPreviewRequestPriority::Current,
            access_mode,
            demand_identity,
        );
        let hardware_decode_request = self.hardware_decode_request_for_key(access_mode, &key);
        let _ = self.jobs.enqueue(MediaPreviewJob {
            key,
            source_secs: 0.0,
            generation,
            priority: MediaPreviewRequestPriority::Current,
            access_mode,
            adaptive_hints: PreviewDecodeAdaptiveHints::default(),
            hardware_decode_request,
            hardware_decode_device_selector: self
                .hardware_decode_device_selector_for_access_mode(access_mode),
            enqueued_at: Instant::now(),
            deadline_at: None,
            demand_identity,
            execution_id: None,
        });
        self.current_generation.set(generation);
        self.current_frame_pending.set(true);
    }

    /// Return a point-in-time snapshot of preview scheduling and cache health.
    pub fn diagnostics(&self) -> AppUiPreviewDiagnostics {
        let scheduler = self.scheduler.diagnostics();
        let frame_store = self.frame_store.borrow().diagnostics();
        let decode_cancellation = self.metrics.decode_cancellation.borrow().report();
        let cancellation = decode_cancellation.all;
        let mut decode_access_mode_profiles = self.metrics.decode_access_mode_profiles.get();
        decode_access_mode_profiles.apply_cancellation(decode_cancellation);
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
            hardware_decode_admission: self.hardware_decode_admission_diagnostics(),
            decode_successes: self.metrics.decode_successes.get(),
            decode_startup_preroll_frames: self.metrics.decode_startup_preroll_frames.get(),
            decode_startup_preroll_total_duration_us: self
                .metrics
                .decode_startup_preroll_total_duration_us
                .get(),
            decode_startup_preroll_max_duration_us: self
                .metrics
                .decode_startup_preroll_max_duration_us
                .get(),
            decode_startup_preroll_queue_wait_total_us: self
                .metrics
                .decode_startup_preroll_queue_wait_total_us
                .get(),
            decode_startup_preroll_queue_wait_max_us: self
                .metrics
                .decode_startup_preroll_queue_wait_max_us
                .get(),
            decode_failures: self.metrics.decode_failures.get(),
            decode_timeout_failures: self.metrics.decode_timeout_failures.get(),
            decode_budget_exhausted_failures: self.metrics.decode_budget_exhausted_failures.get(),
            decode_cancellation,
            decode_canceled_jobs: cancellation.cancellations,
            decode_canceled_shutdown_jobs: cancellation.shutdown,
            decode_canceled_obsolete_jobs: cancellation.superseded,
            decode_canceled_prefetch_deadline_jobs: cancellation.prefetch_deadline,
            decode_canceled_playback_deadline_jobs: cancellation.playback_deadline,
            decode_canceled_prefetch_preempted_jobs: cancellation.prefetch_preempted_by_current,
            decode_canceled_still_preempted_jobs: cancellation.still_preempted_by_realtime_current,
            decode_canceled_unknown_jobs: cancellation.unknown,
            decode_canceled_total_duration_us: cancellation.execution.total_us,
            decode_canceled_max_duration_us: cancellation.execution.max_us,
            decode_canceled_last_duration_us: cancellation.execution.last_us,
            decode_cancel_observation_samples: cancellation.request_to_checkpoint.samples,
            decode_cancel_observation_total_us: cancellation.request_to_checkpoint.total_us,
            decode_cancel_observation_max_us: cancellation.request_to_checkpoint.max_us,
            decode_cancel_observation_last_us: cancellation.request_to_checkpoint.last_us,
            decode_canceled_return_latency_total_us: cancellation.checkpoint_to_return.total_us,
            decode_canceled_return_latency_max_us: cancellation.checkpoint_to_return.max_us,
            decode_canceled_return_latency_last_us: cancellation.checkpoint_to_return.last_us,
            decode_in_process_cpu_frames: self.metrics.decode_in_process_cpu_frames.get(),
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
            decode_canceled_playback_cursor_jobs: decode_cancellation.playback.cancellations,
            decode_canceled_scrub_cursor_jobs: decode_cancellation.interactive.cancellations,
            decode_canceled_random_access_still_jobs: decode_cancellation.still.cancellations,
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
            decode_access_mode_profiles,
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
            prefetch_skipped_current_work: self.metrics.prefetch_skipped_current_work.get(),
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
            scheduler,
            playback_schedule: self.playback_schedule_diagnostics(),
            viewer_frame_cache_hits: self.metrics.viewer_frame_cache_hits.get(),
            viewer_frame_cache_misses: self.metrics.viewer_frame_cache_misses.get(),
            viewer_frame_cache_entries: frame_store.viewer_entries,
            media_cache_entries: frame_store.media_entries,
            media_failure_entries: frame_store.failure_entries,
            media_cache_reserved_bytes: frame_store.media_reserved_bytes,
            media_cache_byte_budget: frame_store.media_byte_budget,
            media_cache_resource_units: frame_store.media_resource_units,
            media_cache_resource_unit_budget: frame_store.media_resource_unit_budget,
            media_cache_evictions: frame_store.media_evictions,
            media_cache_oversize_rejections: frame_store.media_oversize_rejections,
            viewer_frame_cache_reserved_bytes: frame_store.viewer_reserved_bytes,
            viewer_frame_cache_byte_budget: frame_store.viewer_byte_budget,
            viewer_frame_cache_evictions: frame_store.viewer_evictions,
            viewer_frame_cache_oversize_rejections: frame_store.viewer_oversize_rejections,
            pinned_viewer_frame_bytes: frame_store.pinned_viewer_bytes,
            pinned_media_frame_bytes: frame_store.pinned_media_bytes,
            media_failure_evictions: frame_store.failure_evictions,
            color_input_transform_calls: self.metrics.color_input_transform_calls.get(),
            color_input_transform_pixels: self.metrics.color_input_transform_pixels.get(),
            color_output_transform_calls: self.metrics.color_output_transform_calls.get(),
            color_output_transform_pixels: self.metrics.color_output_transform_pixels.get(),
            color_intermediate_transform_calls: self
                .metrics
                .color_intermediate_transform_calls
                .get(),
            color_intermediate_transform_pixels: self
                .metrics
                .color_intermediate_transform_pixels
                .get(),
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
            color_composite_blocked_domains: self.metrics.color_composite_blocked_domains.get(),
            color_composite_blocked_media_effect_domain: self
                .metrics
                .color_composite_blocked_media_effect_domain
                .get(),
            color_composite_blocked_solid_effect_domain: self
                .metrics
                .color_composite_blocked_solid_effect_domain
                .get(),
            color_composite_blocked_adjustment_effect_domain: self
                .metrics
                .color_composite_blocked_adjustment_effect_domain
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
        let (generation, queued_jobs) = self.scheduler.cancel_all();
        let queued_jobs = queued_jobs as u64;
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
        self.external_viewer_frame.replace(None);
        self.frame_store.borrow_mut().clear_all();
    }

    /// Shut down preview workers for application exit.
    pub fn shutdown(&self) {
        self.jobs.close();
        let already_shutdown = self.shutdown.request();
        self.cancel_interactive_work();
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
    pub fn poll_finished(
        &self,
        pending_playback_demand: Option<mondrian_playback::FrameDemandIdentity>,
    ) -> bool {
        self.poll_finished_outcome(pending_playback_demand).visible_change
    }

    fn poll_finished_outcome(
        &self,
        pending_playback_demand: Option<mondrian_playback::FrameDemandIdentity>,
    ) -> PreviewWorkPoll {
        self.poll_finished_outcome_with_budget(
            MEDIA_PREVIEW_MAX_COMPLETED_RESULTS_PER_POLL,
            Duration::from_micros(MEDIA_PREVIEW_COMPLETED_RESULTS_POLL_BUDGET_US),
            pending_playback_demand,
        )
    }

    fn expire_stalled_realtime_current(
        &self,
        pending_playback_demand: Option<mondrian_playback::FrameDemandIdentity>,
    ) -> PreviewWorkPoll {
        self.expire_stalled_realtime_current_with_timeout(
            Duration::from_micros(MEDIA_PREVIEW_PLAYBACK_BUFFERING_STALL_TIMEOUT_US),
            pending_playback_demand,
        )
    }

    fn expire_stalled_realtime_current_with_timeout(
        &self,
        timeout: Duration,
        pending_playback_demand: Option<mondrian_playback::FrameDemandIdentity>,
    ) -> PreviewWorkPoll {
        let expired = self.scheduler.expire_realtime_current_older_than(timeout);
        if expired.is_empty() {
            return PreviewWorkPoll::default();
        }
        let canceled_queued_jobs = expired.len() as u64;
        // Scheduler-current work can outlive the Playback Session demand that
        // created it (for example after a GPU presentation completed first).
        // Only the still-pending Playback identity has terminal authority, and
        // multiple redundant jobs for it collapse to one Late observation.
        let frame_deliveries = pending_playback_demand
            .and_then(|pending_identity| {
                expired.iter().find_map(|request| {
                    (request.access_mode == PreviewDecodeAccessMode::PlaybackCursor
                        && request.demand_identity == Some(pending_identity))
                    .then_some(pending_identity)
                })
            })
            .map(|identity| {
                mondrian_playback::FrameDelivery::for_demand(
                    identity,
                    mondrian_playback::FrameDeliveryKind::Late,
                )
            })
            .into_iter()
            .collect::<Vec<_>>();
        add_cell(
            &self.metrics.playback_current_stalled_expirations,
            frame_deliveries.len() as u64,
        );
        self.record_playback_current_late_drop(frame_deliveries.len() as u64);
        add_cell(&self.metrics.queue_canceled_jobs, canceled_queued_jobs);
        self.current_frame_pending.set(false);
        PreviewWorkPoll {
            visible_change: false,
            transport_change: true,
            needs_follow_up_poll: false,
            frame_deliveries,
        }
    }

    #[cfg(test)]
    fn poll_finished_with_budget(
        &self,
        max_results: usize,
        time_budget: Duration,
        pending_playback_demand: Option<mondrian_playback::FrameDemandIdentity>,
    ) -> bool {
        self.poll_finished_outcome_with_budget(max_results, time_budget, pending_playback_demand)
            .visible_change
    }

    fn poll_finished_outcome_with_budget(
        &self,
        max_results: usize,
        time_budget: Duration,
        pending_playback_demand: Option<mondrian_playback::FrameDemandIdentity>,
    ) -> PreviewWorkPoll {
        let poll_started = Instant::now();
        bump(&self.metrics.completion_poll_calls);
        self.metrics
            .completion_poll_max_results_per_poll
            .set(self.metrics.completion_poll_max_results_per_poll.get().max(max_results as u64));
        let mut outcome = PreviewWorkPoll::default();
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
            let completion_resolution = if let Some(execution_id) = result.execution_id {
                self.scheduler.resolve_execution(execution_id, !result.canceled)
            } else {
                self.scheduler.resolve_unleased(
                    &result.key,
                    result.generation,
                    result.access_mode,
                    result.demand_identity,
                    !result.canceled,
                )
            };
            let completion = completion_resolution.status;
            let completion_demand_identity =
                completion_resolution.demand_identity.or(result.demand_identity);
            let owns_pending_playback_demand = completion.is_current()
                && completion_demand_identity.is_some()
                && completion_demand_identity == pending_playback_demand;
            let presentation_current = completion.is_current()
                && (result.access_mode != PreviewDecodeAccessMode::PlaybackCursor
                    || owns_pending_playback_demand);
            let startup_preroll = media_preview_result_is_startup_preroll(&result);
            if !startup_preroll {
                self.record_preview_decode_queue_wait(
                    result.priority,
                    result.access_mode,
                    result.queue_wait_us,
                );
            }
            if result.canceled {
                if result.cancellation_phase == Some(MediaPreviewCancellationPhase::Queued) {
                    // A request that expired before codec execution is a
                    // scheduler deadline drop, not cooperative-cancellation
                    // latency evidence. Complete the matching demand as Late
                    // so playback can advance without waiting for its stall
                    // timeout; broker expiry counters retain the diagnosis.
                    if owns_pending_playback_demand {
                        if let Some(identity) = completion_demand_identity {
                            outcome.frame_deliveries.push(
                                mondrian_playback::FrameDelivery::for_demand(
                                    identity,
                                    mondrian_playback::FrameDeliveryKind::Late,
                                ),
                            );
                        }
                        self.record_playback_current_late_drop(1);
                    }
                    continue;
                }
                self.record_preview_decode_cancel(
                    result.access_mode,
                    result.cancel_reason,
                    result.decode_elapsed_us,
                    result.cancel_observed_elapsed_us,
                    result.cancel_request_to_observed_us,
                    owns_pending_playback_demand,
                );
                // Decode work cancellation is scheduler evidence, not a frame
                // presentation. Emitting it as a terminal FrameDelivery races
                // the Playback Session's newer demand and turns expected
                // latest-wins cleanup into a rejected stale delivery.
                // A settled scrub/still request can be cooperatively canceled
                // while the render generation changes. Its completion releases
                // the scheduler entry, so request one more render pass to submit
                // the stable current frame. Playback deadline cancellation is
                // intentionally excluded to avoid a realtime retry loop.
                if result.priority == MediaPreviewRequestPriority::Current
                    && result.access_mode != PreviewDecodeAccessMode::PlaybackCursor
                {
                    outcome.visible_change = true;
                }
                continue;
            }
            let completed_after_playback_deadline =
                completion_resolution.deadline_status.is_missed();
            if presentation_current {
                if let Some(identity) = completion_demand_identity {
                    let kind = playback_frame_delivery_kind(
                        completed_after_playback_deadline,
                        result.frame.is_some(),
                        result.priority,
                        result.decode_diagnostics.as_ref().map(PlaybackDecodeExecution::from),
                    );
                    match kind {
                        mondrian_playback::FrameDeliveryKind::Ready => {
                            // Successful decode is non-terminal: the Presentation Adapter
                            // finishes the demand after its output is actually usable.
                        }
                        mondrian_playback::FrameDeliveryKind::Degraded => {}
                        _ => outcome
                            .frame_deliveries
                            .push(mondrian_playback::FrameDelivery::for_demand(identity, kind)),
                    }
                }
            }
            if let Some(diagnostics) = result.decode_diagnostics {
                self.scrub_adaptation.borrow_mut().observe_decode(diagnostics);
                if startup_preroll {
                    self.record_startup_preroll_decode(diagnostics, result.queue_wait_us);
                } else {
                    self.record_preview_decode(
                        diagnostics,
                        result.priority,
                        result.queue_wait_us,
                        !completed_after_playback_deadline,
                    );
                }
            }
            if completed_after_playback_deadline && owns_pending_playback_demand {
                self.record_playback_current_late_drop(1);
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
                    if completed_after_playback_deadline {
                        continue;
                    }
                    if completion.should_cache() {
                        let mut frame_store = self.frame_store.borrow_mut();
                        frame_store.insert_media_frame(
                            result.key.clone(),
                            frame,
                            presentation_current,
                        );
                        frame_store.forget_failure(&result.key);
                    }
                    outcome.visible_change |= presentation_current;
                }
                None => {
                    if let Some(reason) = result.failure_reason {
                        self.scrub_adaptation
                            .borrow_mut()
                            .observe_failure(result.access_mode, reason);
                    }
                    let terminal_failure =
                        result.failure_reason == Some(MediaPreviewFailureReason::DecodeError);
                    if presentation_current && terminal_failure {
                        self.frame_store.borrow_mut().clear_pinned_media_frame();
                    }
                    self.record_preview_decode_failure(result.access_mode, result.failure_reason);
                    if let Some(error) = result.error {
                        tracing::debug!(
                            asset_id = %result.key.asset_id,
                            path = %result.key.path.display(),
                            "viewer preview decode failed: {error}"
                        );
                    }
                    if completed_after_playback_deadline {
                        continue;
                    }
                    if completion.should_cache() && terminal_failure {
                        self.frame_store.borrow_mut().remember_failure(result.key);
                    }
                    // A current retryable failure changed adaptive decode policy.
                    // Request one refresh so the same target can recover through
                    // the conservative keyframe path instead of stalling forever.
                    outcome.visible_change |= presentation_current;
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
            self.frame_store.borrow_mut().clear_pinned_viewer_frame();
            bump(&self.metrics.unavailable_frames);
            return ViewerPreviewState::Unavailable;
        };
        let frame = state.current_frame().max(0);
        let (width, height) = preview_dimensions_for_state(state, sequence);
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
                self.frame_store.borrow_mut().clear_pinned_viewer_frame();
                bump(&self.metrics.unavailable_frames);
                return ViewerPreviewState::Unavailable;
            }
        };
        let color_context = sequence
            .settings
            .root_program_color_context(&state.project_settings.color_management);
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
                self.current_presentation_quality
                    .set(resolved_preview_presentation_quality(&resolved.elements));
                let final_cache_lookup_started_at = Instant::now();
                // GPU viewer outputs are display-referred resources. Keep the
                // raster cache identity independent of the monitor contract,
                // but compare external textures with the same adapted identity
                // used when the GPU candidate was registered.
                let external_cache_key = resolved.cache_key.as_ref().and_then(|cache_key| {
                    let program_output_color_space =
                        resolved.color_context.output_color_space.color()?;
                    RenderMonitorAdaptation::new(
                        program_output_color_space,
                        display_color_space,
                        resolved.color_context.engine.clone(),
                    )
                    .ok()
                    .map(|adaptation| cache_key.with_monitor_adaptation(&adaptation))
                });
                if let Some(frame) = external_cache_key
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
                    self.frame_store.borrow_mut().pin_viewer_frame(ScopedViewerFrame {
                        sequence_id: sequence.id,
                        width,
                        height,
                        frame: frame.clone(),
                    });
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
                    self.stale_viewer_content_for_sequence(sequence, width, height)
                        .map(ViewerPreviewState::Stale)
                        .unwrap_or(ViewerPreviewState::Loading)
                } else {
                    render_stage_durations.final_cache_lookup_us =
                        app_duration_us(final_cache_lookup_started_at.elapsed());
                    let raster_contract =
                        match cpu_raster_presentation_contract(&resolved.color_context) {
                            Ok(contract) => contract,
                            Err(output_color_space) => {
                                tracing::warn!(
                                    output_color_space = ?output_color_space,
                                    "CPU raster viewer has no compatible presentation contract"
                                );
                                return ViewerPreviewState::Unavailable;
                            }
                        };
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
                    if let Some(diagnostics) = output.monitor_color_diagnostics {
                        self.record_color_transform(diagnostics);
                    }
                    self.record_color_stage(output.color_stage_diagnostics);
                    let rgba = output.rgba;
                    let frame_packaging_started_at = Instant::now();
                    let key =
                        resolved.cache_key.as_ref().map(viewer_raster_frame_key).unwrap_or_else(
                            || uncached_viewer_raster_frame_key(sequence.id, frame, width, height),
                        );
                    match ViewerFrameImage::new(
                        key,
                        width,
                        height,
                        raster_contract.raster_color_space,
                        rgba,
                    ) {
                        Some(frame) => {
                            if let Some(cache_key) = resolved.cache_key {
                                self.frame_store
                                    .borrow_mut()
                                    .insert_viewer_frame(cache_key, frame.clone());
                            }
                            render_stage_durations.frame_packaging_us =
                                app_duration_us(frame_packaging_started_at.elapsed());
                            self.record_render_stage_durations(
                                app_duration_us(render_started_at.elapsed()),
                                render_stage_durations,
                            );
                            self.frame_store.borrow_mut().pin_viewer_frame(ScopedViewerFrame {
                                sequence_id: sequence.id,
                                width,
                                height,
                                frame: frame.clone(),
                            });
                            ViewerPreviewState::Ready(ViewerFrameContent::Raster(frame))
                        }
                        None => ViewerPreviewState::Unavailable,
                    }
                }
            }
            None if self.current_frame_pending.get() => self
                .stale_viewer_content_for_sequence(sequence, width, height)
                .map(ViewerPreviewState::Stale)
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
        let (width, height) = preview_dimensions_for_state(state, sequence);
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
        let color_context = sequence
            .settings
            .root_program_color_context(&state.project_settings.color_management);
        let Some(program_output_color_space) = color_context.output_color_space.color() else {
            self.record_preview_gpu_output_blocker(&PreviewGpuOutputBlocker::UnsupportedFeature {
                feature: "program_output_identity".to_owned(),
                reason: format!(
                    "Program Output {:?} is not an encoded color identity",
                    color_context.output_color_space
                ),
            });
            self.scheduler.prune_obsolete();
            self.external_viewer_frame.replace(None);
            bump(&self.metrics.gpu_preview_candidate_unavailable);
            return AppUiGpuPreviewFrameState::Unavailable;
        };
        let monitor_adaptation = match RenderMonitorAdaptation::new(
            program_output_color_space,
            display_color_space,
            color_context.engine.clone(),
        ) {
            Ok(adaptation) => adaptation,
            Err(error) => {
                self.record_preview_gpu_output_blocker(
                    &PreviewGpuOutputBlocker::UnsupportedFeature {
                        feature: "monitor_adaptation".to_owned(),
                        reason: error.to_string(),
                    },
                );
                self.scheduler.prune_obsolete();
                self.external_viewer_frame.replace(None);
                bump(&self.metrics.gpu_preview_candidate_unavailable);
                return AppUiGpuPreviewFrameState::Unavailable;
            }
        };
        self.activate_preview_generation(ViewerPreviewGenerationKey::from_state(
            state,
            sequence,
            frame,
            width,
            height,
            display_color_space,
        ));
        let mut resolved = match self.resolve_sequence_elements(
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
        if let Some(cache_key) = resolved.cache_key.as_mut() {
            *cache_key = cache_key.with_monitor_adaptation(&monitor_adaptation);
        }
        self.current_presentation_quality
            .set(resolved_preview_presentation_quality(&resolved.elements));
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
        let Ok(program_output_boundary) =
            output_boundary_from_color_context(&resolved.color_context)
        else {
            bump(&self.metrics.gpu_preview_candidate_unavailable);
            return AppUiGpuPreviewFrameState::Unavailable;
        };
        let decode_execution = resolved_preview_decode_execution(&resolved.elements);
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
            program_output_boundary,
            monitor_adaptation,
            preview_candidate_id: candidate_id,
            presentation_ticket: self.playback_presentation_ticket(state),
            decode_execution,
        }))
    }

    /// Build the exact ticket that a Presentation Adapter may complete only
    /// after it makes the current output usable.
    pub(crate) fn playback_presentation_ticket(
        &self,
        state: &AppState,
    ) -> Option<mondrian_playback::FramePresentationTicket> {
        state.playback_frame_presentation_ticket(self.current_presentation_quality.get())
    }

    /// Mark a GPU preview output texture as the current viewer frame for its resolved plan.
    pub(crate) fn set_external_viewer_frame(
        &self,
        frame: &AppUiGpuPreviewFrame,
        texture_key: impl Into<String>,
        presentation: ViewerExternalTexturePresentation,
    ) -> bool {
        let Some(content) = ViewerExternalTextureFrame::new_spatial(texture_key, presentation)
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
        self.frame_store.borrow_mut().clear_viewer_frames();
        self.external_viewer_frame.replace(None);
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
        self.current_presentation_quality
            .set(mondrian_playback::FramePresentationQuality::Ready);
        let generation = self.scheduler.begin_generation();
        self.current_generation.set(generation);
    }

    fn cached_viewer_frame(&self, key: &ViewerPreviewCacheKey) -> Option<ViewerFrameImage> {
        let frame = self.frame_store.borrow_mut().viewer_frame(key);
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

    fn stale_viewer_content_for_sequence(
        &self,
        sequence: &Sequence,
        width: u32,
        height: u32,
    ) -> Option<ViewerFrameContent> {
        self.external_viewer_frame
            .borrow()
            .as_ref()
            .filter(|frame| frame.cache_key.sequence_id == sequence.id)
            .map(|frame| ViewerFrameContent::ExternalTexture(frame.content.clone()))
            .or_else(|| {
                self.stale_frame_for_sequence(sequence, width, height)
                    .map(ViewerFrameContent::Raster)
            })
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
            RenderColorTransformDirection::Intermediate => {
                bump(&self.metrics.color_intermediate_transform_calls);
                add_cell(
                    &self.metrics.color_intermediate_transform_pixels,
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
        count_playback_current_success: bool,
    ) {
        if count_playback_current_success {
            self.record_playback_current_success(priority, diagnostics.access_mode);
        }
        self.record_playback_current_hardware_recovery(priority, &diagnostics);
        match diagnostics.path {
            PreviewDecodePath::InProcessFfmpegCpuRgba
            | PreviewDecodePath::InProcessFfmpegCpuFloat => {
                bump(&self.metrics.decode_in_process_cpu_frames);
            }
            PreviewDecodePath::InProcessFfmpegNative => {}
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

    fn record_startup_preroll_decode(
        &self,
        diagnostics: PreviewDecodeDiagnostics,
        queue_wait_us: u64,
    ) {
        bump(&self.metrics.decode_startup_preroll_frames);
        add_cell(
            &self.metrics.decode_startup_preroll_total_duration_us,
            diagnostics.elapsed_us,
        );
        self.metrics.decode_startup_preroll_max_duration_us.set(
            self.metrics
                .decode_startup_preroll_max_duration_us
                .get()
                .max(diagnostics.elapsed_us),
        );
        add_cell(
            &self.metrics.decode_startup_preroll_queue_wait_total_us,
            queue_wait_us,
        );
        self.metrics
            .decode_startup_preroll_queue_wait_max_us
            .set(self.metrics.decode_startup_preroll_queue_wait_max_us.get().max(queue_wait_us));
    }

    fn record_preview_decode_cancel(
        &self,
        access_mode: PreviewDecodeAccessMode,
        reason: Option<MediaPreviewCancelReason>,
        elapsed_us: u64,
        observed_elapsed_us: Option<u64>,
        request_to_observed_us: Option<u64>,
        owns_pending_playback_demand: bool,
    ) {
        let reason = reason.unwrap_or(MediaPreviewCancelReason::Unknown);
        if reason == MediaPreviewCancelReason::PlaybackDeadline && owns_pending_playback_demand {
            self.record_playback_current_late_drop(1);
        }
        self.metrics.decode_cancellation.borrow_mut().observe(
            mondrian_playback::FrameCancellationObservation {
                work_class: media_preview_frame_work_class(access_mode),
                cause: reason.playback_cause(),
                execution_duration: Duration::from_micros(elapsed_us),
                execution_to_checkpoint: observed_elapsed_us.map(Duration::from_micros),
                request_to_checkpoint: request_to_observed_us.map(Duration::from_micros),
            },
        );
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
            current_sustained_pressure_skips: self
                .metrics
                .playback_current_sustained_pressure_skips
                .get(),
            current_proxy_or_hardware_recommended_decisions: self
                .metrics
                .playback_current_proxy_or_hardware_recommended_decisions
                .get(),
            current_native_import_unavailable_decisions: self
                .metrics
                .playback_current_native_import_unavailable_decisions
                .get(),
            current_hardware_fallback_not_engaged_decisions: self
                .metrics
                .playback_current_hardware_fallback_not_engaged_decisions
                .get(),
            current_proxy_generation_requests: self.metrics.media_proxy_generation_requests.get(),
            current_proxy_generation_request_dedupes: self
                .metrics
                .media_proxy_generation_request_dedupes
                .get(),
            current_late_streak: self.playback_pressure.get().late_streak(),
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
        let mut pressure = self.playback_pressure.get();
        let transition = pressure.observe_late(count);
        self.playback_pressure.set(pressure);
        if transition == PlaybackPressureTransition::Entered {
            bump(&self.metrics.playback_sustained_pressure_events);
        }
    }

    fn record_playback_current_sustained_pressure_skip(&self) {
        bump(&self.metrics.playback_current_sustained_pressure_skips);
        bump(&self.metrics.playback_current_proxy_or_hardware_recommended_decisions);
    }

    fn playback_realtime_work_pending(&self) -> bool {
        let queue = self.jobs.diagnostics();
        queue.queued_current_jobs > 0
            || queue.in_flight_current_jobs > 0
            || queue.queued_playback_cursor_jobs > 0
            || queue.in_flight_playback_cursor_jobs > 0
    }

    fn record_playback_current_success(
        &self,
        priority: MediaPreviewRequestPriority,
        access_mode: PreviewDecodeAccessMode,
    ) {
        let mut pressure = self.playback_pressure.get();
        let transition = pressure.observe_success(priority, access_mode);
        self.playback_pressure.set(pressure);
        if transition == PlaybackPressureTransition::Recovered {
            bump(&self.metrics.playback_sustained_pressure_recoveries);
        }
    }

    fn record_playback_current_hardware_recovery(
        &self,
        priority: MediaPreviewRequestPriority,
        diagnostics: &PreviewDecodeDiagnostics,
    ) {
        let signals = playback_hardware_recovery_signals(
            priority,
            self.playback_hardware_decode_request.get(),
            self.playback_hardware_decode_native_import_admission_ready.get(),
            PlaybackDecodeExecution::from(diagnostics),
        );
        if signals.native_import_unavailable {
            bump(&self.metrics.playback_current_native_import_unavailable_decisions);
        }
        if signals.hardware_fallback_not_engaged {
            bump(&self.metrics.playback_current_hardware_fallback_not_engaged_decisions);
        }
        if signals.recovery_recommended() {
            bump(&self.metrics.playback_current_proxy_or_hardware_recommended_decisions);
        }
    }

    fn playback_sustained_pressure_active(&self) -> bool {
        self.playback_pressure.get().is_active()
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
        add_cell(
            &self.metrics.color_composite_blocked_domains,
            diagnostics.blocked_color_domain_composites,
        );
        add_cell(
            &self.metrics.color_composite_blocked_media_effect_domain,
            diagnostics.blocked_media_effect_domain,
        );
        add_cell(
            &self.metrics.color_composite_blocked_solid_effect_domain,
            diagnostics.blocked_solid_effect_domain,
        );
        add_cell(
            &self.metrics.color_composite_blocked_adjustment_effect_domain,
            diagnostics.blocked_adjustment_effect_domain,
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
        self.frame_store.borrow().stale_viewer_frame(sequence.id, width, height)
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MediaPrerollFrameReadiness {
    has_media: bool,
    ready: bool,
}

impl MediaPrerollFrameReadiness {
    const fn required_not_ready() -> Self {
        Self { has_media: true, ready: false }
    }

    fn merge(&mut self, other: Self) {
        if other.has_media {
            self.has_media = true;
            self.ready &= other.ready;
        }
    }
}

impl Default for MediaPrerollFrameReadiness {
    fn default() -> Self {
        Self { has_media: false, ready: true }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ViewerPreviewGenerationKey {
    sequence_id: SequenceId,
    /// Playback Epoch for a running cursor. `None` identifies an idle/still
    /// cursor, whose exact frame remains part of the generation identity.
    playback_epoch: Option<mondrian_playback::PlaybackEpoch>,
    still_frame: Option<i64>,
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
        let playing = state.is_playing();
        Self {
            sequence_id: sequence.id,
            playback_epoch: playing.then(|| state.playback_epoch()),
            still_frame: (!playing).then_some(frame),
            width,
            height,
            display_color_space,
            playing,
            seek_source: state.last_timeline_seek_source,
        }
    }
}

mod diagnostics;
pub use diagnostics::*;
mod media_adapter;
use media_adapter::PreviewProxyGenerationRequestKey;
#[cfg(test)]
use media_adapter::{
    resolve_preview_media_decode_path, should_request_preview_proxy_generation, source_micros,
    PreviewMediaDecodePathResolution,
};
mod timeline_evaluation;
mod viewer_plan;

pub(crate) use viewer_plan::ViewerPreviewCacheKey;
use viewer_plan::{
    gpu_composite_layers_for_resolved, preview_elements_require_deferred_composite,
    resolved_preview_decode_execution, resolved_preview_presentation_quality,
    uncached_viewer_raster_frame_key, viewer_preview_cache_key_for_resolved_plan,
    viewer_raster_frame_key, ResolvedPreviewElement,
};
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
    pub working_color_space: WorkingColorSpace,
    /// Working-space input that enters the GPU output boundary.
    pub working_input: AppUiGpuPreviewWorkingInput,
    /// Program Output boundary shared with delivery/export semantics.
    pub program_output_boundary: RenderOutputColorBoundary,
    /// Preview-only Program Output to local-monitor adaptation.
    pub monitor_adaptation: RenderMonitorAdaptation,
    /// Monotonic identifier for this working-frame candidate.
    preview_candidate_id: u64,
    /// Exact terminal delivery to emit only after presentation succeeds.
    presentation_ticket: Option<mondrian_playback::FramePresentationTicket>,
    /// Decode provenance of the exact media layers entering this candidate.
    #[cfg_attr(not(test), allow(dead_code))]
    decode_execution: AppUiPreviewDecodeExecutionSummary,
}

/// Working-space input for the app-window GPU output path.
pub(crate) enum AppUiGpuPreviewWorkingInput {
    /// Layer stack to composite directly on GPU before the GPU output transform.
    GpuComposite {
        /// Layers in bottom-to-top order.
        layers: Vec<mondrian_renderer::ViewerGpuExecutionLayer>,
    },
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

    /// Exact playback delivery authorized for successful presentation.
    pub(crate) const fn presentation_ticket(
        &self,
    ) -> Option<mondrian_playback::FramePresentationTicket> {
        self.presentation_ticket
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) const fn decode_execution(&self) -> AppUiPreviewDecodeExecutionSummary {
        self.decode_execution
    }
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

impl PlaybackPreviewAdapter for AppUiPreviewService {
    fn poll_playback_work(
        &self,
        pending_demand: Option<mondrian_playback::FrameDemandIdentity>,
    ) -> PreviewWorkPoll {
        let mut outcome = self.poll_finished_outcome(pending_demand);
        outcome.merge(self.expire_stalled_realtime_current(pending_demand));
        outcome
    }

    fn video_preroll(&self, state: &AppState) -> Option<PreviewVideoPreroll> {
        self.playback_video_preroll_readiness(state)
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
pub(crate) struct MediaPreviewFrame {
    frame: Option<CpuColorFrame>,
    gpu_source: Option<MediaPreviewGpuSourceFrame>,
    native_source: Option<MediaPreviewNativeSourceFrame>,
    width: u32,
    height: u32,
    logical_width: u32,
    logical_height: u32,
    signature: u64,
    presentation_quality: mondrian_playback::FramePresentationQuality,
    decode_execution: AppUiPreviewDecodeExecutionSummary,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub(crate) struct AppUiPreviewDecodeExecutionSummary {
    pub media_layers: u32,
    pub software_cpu_layers: u32,
    pub hardware_cpu_transfer_layers: u32,
    pub hardware_native_layers: u32,
    pub p010_10_bit_hardware_layers: u32,
}

impl AppUiPreviewDecodeExecutionSummary {
    fn from_path(path: PreviewDecodeExecutionPath) -> Self {
        let mut summary = Self { media_layers: 1, ..Self::default() };
        match path {
            PreviewDecodeExecutionPath::SoftwareCpu => summary.software_cpu_layers = 1,
            PreviewDecodeExecutionPath::HardwareCpuTransfer { surface, sampling, .. } => {
                summary.hardware_cpu_transfer_layers = 1;
                if surface == DecodedVideoSurfaceFormat::P010 && sampling.bit_depth == 10 {
                    summary.p010_10_bit_hardware_layers = 1;
                }
            }
            PreviewDecodeExecutionPath::HardwareNative { surface, sampling, .. } => {
                summary.hardware_native_layers = 1;
                if surface == DecodedVideoSurfaceFormat::P010 && sampling.bit_depth == 10 {
                    summary.p010_10_bit_hardware_layers = 1;
                }
            }
        }
        summary
    }

    pub(crate) fn accumulate(&mut self, other: Self) {
        self.media_layers = self.media_layers.saturating_add(other.media_layers);
        self.software_cpu_layers =
            self.software_cpu_layers.saturating_add(other.software_cpu_layers);
        self.hardware_cpu_transfer_layers = self
            .hardware_cpu_transfer_layers
            .saturating_add(other.hardware_cpu_transfer_layers);
        self.hardware_native_layers =
            self.hardware_native_layers.saturating_add(other.hardware_native_layers);
        self.p010_10_bit_hardware_layers = self
            .p010_10_bit_hardware_layers
            .saturating_add(other.p010_10_bit_hardware_layers);
    }
}

impl MediaPreviewFrame {
    pub(crate) fn reserved_cpu_bytes(&self) -> usize {
        let linear_bytes = self
            .frame
            .as_ref()
            .map(|frame| std::mem::size_of_val(frame.rgba_f32().data.as_slice()))
            .unwrap_or(0);
        let source_and_lazy_working_bytes = self
            .gpu_source
            .as_ref()
            .map(|source| {
                let working_reservation = (self.width as usize)
                    .saturating_mul(self.height as usize)
                    .saturating_mul(4)
                    .saturating_mul(std::mem::size_of::<f32>());
                source.source.retained_bytes().saturating_add(working_reservation)
            })
            .unwrap_or(0);
        linear_bytes.saturating_add(source_and_lazy_working_bytes)
    }

    pub(super) fn decoder_resource_units(&self) -> usize {
        usize::from(self.native_source.is_some())
    }

    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn logical_resolution(&self) -> Resolution {
        Resolution {
            width: self.logical_width,
            height: self.logical_height,
        }
    }

    fn presentation_quality(&self) -> mondrian_playback::FramePresentationQuality {
        self.presentation_quality
    }

    fn decode_execution(&self) -> AppUiPreviewDecodeExecutionSummary {
        self.decode_execution
    }

    fn gpu_source(&self) -> Option<mondrian_renderer::ViewerGpuMediaSource> {
        self.gpu_source.as_ref().map(|source| mondrian_renderer::ViewerGpuMediaSource {
            source: Arc::clone(&source.source),
            input_transform: source.input_transform.clone(),
            decoder_residency: source.decoder_residency,
            decoder_handle_kind: source.decoder_handle_kind,
            decoded_surface_format: source.decoded_surface_format,
            decoded_video_sampling: source.decoded_video_sampling,
        })
    }

    fn native_source(&self) -> Option<mondrian_renderer::ViewerGpuNativeSource> {
        self.native_source
            .as_ref()
            .map(|source| mondrian_renderer::ViewerGpuNativeSource {
                source_color_space: source.source_color_space,
                input_transform: source.input_transform.clone(),
                native_frame: source.native_frame.clone(),
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
            if let Some(native) = self.native_source.as_ref() {
                return Err(format!(
                    "media preview frame is native GPU decoded ({} {:?}) and requires renderer native import; no CPU working fallback exists",
                    native.native_frame.handle_kind().as_str(),
                    native.native_frame.surface_format
                ));
            }
            return Err("media preview frame has no CPU working frame or GPU source".to_owned());
        };
        let cached_before = source.working_cache.get().is_some();
        let entry = source
            .working_cache
            .get_or_init(|| {
                execute_cpu_source_input_stage(source.source.as_ref(), &source.input_transform)
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

fn project_preview_media_transform(
    transform: [f32; 6],
    frame: &MediaPreviewFrame,
    output_authoring: Resolution,
    output_sampled: Resolution,
) -> Option<[f32; 6]> {
    project_affine_to_sampled_extents(
        transform,
        frame.logical_resolution(),
        Resolution { width: frame.width(), height: frame.height() },
        output_authoring,
        output_sampled,
    )
}

struct MediaPreviewWorkingFrame {
    frame: CpuColorFrame,
    color_diagnostics: Option<RenderColorTransformDiagnostics>,
    stage_diagnostics: RenderColorStageDiagnostics,
}

#[derive(Debug, Clone)]
struct MediaPreviewGpuSourceFrame {
    source: Arc<CpuSourceColorFrame>,
    input_transform: RenderInputTransform,
    decoder_residency: DecodedFrameResidency,
    decoder_handle_kind: Option<DecodedGpuFrameHandleKind>,
    decoded_surface_format: DecodedVideoSurfaceFormat,
    decoded_video_sampling: DecodedVideoSampling,
    working_cache: Arc<OnceLock<Result<MediaPreviewWorkingFrameCacheEntry, String>>>,
}

#[derive(Debug, Clone)]
struct MediaPreviewNativeSourceFrame {
    source_color_space: ColorSpace,
    input_transform: RenderInputTransform,
    native_frame: Arc<PreviewNativeDecodedFrame>,
}

impl MediaPreviewNativeSourceFrame {
    fn from_native_frame(
        native_frame: PreviewNativeDecodedFrame,
        source_color_space: ColorSpace,
        input_transform: RenderInputTransform,
    ) -> Self {
        Self {
            source_color_space,
            input_transform,
            native_frame: Arc::new(native_frame),
        }
    }
}

impl MediaPreviewGpuSourceFrame {
    #[cfg(test)]
    fn new(source: impl Into<CpuSourceColorFrame>, input_transform: RenderInputTransform) -> Self {
        Self {
            source: Arc::new(source.into()),
            input_transform,
            decoder_residency: DecodedFrameResidency::CpuRgba,
            decoder_handle_kind: None,
            decoded_surface_format: DecodedVideoSurfaceFormat::Unknown,
            decoded_video_sampling: DecodedVideoSampling::default(),
            working_cache: Arc::new(OnceLock::new()),
        }
    }

    fn from_decode_diagnostics(
        source: impl Into<CpuSourceColorFrame>,
        input_transform: RenderInputTransform,
        diagnostics: PreviewDecodeDiagnostics,
    ) -> Self {
        Self {
            source: Arc::new(source.into()),
            input_transform,
            decoder_residency: diagnostics.decoded_frame_residency,
            decoder_handle_kind: diagnostics.gpu_frame_handle_kind,
            decoded_surface_format: diagnostics.decoded_surface_format,
            decoded_video_sampling: diagnostics.decoded_video_sampling,
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
pub(crate) struct ScopedViewerFrame {
    pub(crate) sequence_id: SequenceId,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) frame: ViewerFrameImage,
}

#[derive(Debug, Clone)]
struct ScopedExternalViewerFrame {
    cache_key: ViewerPreviewCacheKey,
    content: ViewerExternalTextureFrame,
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
    deadline_at: Option<Instant>,
    cancel_observed_elapsed_us: Option<u64>,
    cancel_request_to_observed_us: Option<u64>,
    canceled: bool,
    cancellation_phase: Option<MediaPreviewCancellationPhase>,
    cancel_reason: Option<MediaPreviewCancelReason>,
    decode_diagnostics: Option<PreviewDecodeDiagnostics>,
    color_diagnostics: Option<RenderColorTransformDiagnostics>,
    color_stage_diagnostics: Option<RenderColorStageDiagnostics>,
    demand_identity: Option<mondrian_playback::FrameDemandIdentity>,
    execution_id: Option<mondrian_playback::FrameExecutionId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MediaPreviewCancellationPhase {
    /// Deadline or obsolescence was resolved before codec work began.
    Queued,
    /// A worker lease began and cancellation was observed cooperatively.
    Executing,
}

#[derive(Default)]
struct PreviewShutdownSignal {
    requested: AtomicBool,
}

impl PreviewShutdownSignal {
    fn request(&self) -> bool {
        self.requested.swap(true, Ordering::AcqRel)
    }

    fn is_requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
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
        if worker_queue.queued_current_jobs > 0 || worker_queue.in_flight_current_jobs > 0 {
            bump(&self.metrics.prefetch_skipped_current_work);
            return;
        }
        let prefetch_pressure = worker_queue
            .queued_prefetch_jobs
            .saturating_add(worker_queue.in_flight_prefetch_jobs);
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
        let preroll_deadline_at = state
            .is_playback_priming()
            .then(|| state.playback_frame_deadline_at(Instant::now()))
            .flatten();
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
                preroll_deadline_at,
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
        preroll_deadline_at: Option<Instant>,
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
        let Ok(evaluation) = evaluation else {
            return;
        };

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
                        media.alpha_interpretation,
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
                            preroll_deadline_at,
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
                            preview_dimensions_for_state(state, nested_sequence);
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
                            preroll_deadline_at,
                        );
                    }
                }
                TimelineRenderPlanElement::SolidColor(_)
                | TimelineRenderPlanElement::Adjustment(_) => {}
            }
        }
    }

    /// Inspect the same bounded forward media window used by playback prefetch.
    ///
    /// This does not claim that a Viewer output is presented. It reports only
    /// whether immediate future frames have media payloads and how much of the
    /// media-bearing prefix is resident; the Playback Engine separately
    /// requires current-frame presentation before releasing its clock anchor.
    fn playback_video_preroll_readiness(&self, state: &AppState) -> Option<PreviewVideoPreroll> {
        if !state.is_playback_priming() {
            return None;
        }
        let sequence = state.sequence.as_ref()?;
        let current_frame = state.current_frame().max(0);
        let end_frame = state.last_content_frame().ok()?.max(0);
        if current_frame >= end_frame {
            return Some(PreviewVideoPreroll { ready_media_frames: 0, available_media_frames: 0 });
        }
        let (width, height) = preview_dimensions_for_state(state, sequence);
        let display_snapshot = self.display_snapshot.borrow();
        let display_color_space = preview_display_color_space(
            sequence,
            &state.project_settings.color_management,
            display_snapshot.as_ref(),
        )
        .ok()?;
        let color_context = sequence.settings.root_preview_color_context(
            &state.project_settings.color_management,
            display_color_space,
        );
        let window = media_preview_forward_prefetch_window_frames(sequence.settings.frame_rate)?;
        let mut ready_media_frames = 0usize;
        let mut available_media_frames = 0usize;
        let mut ready_prefix = true;
        for offset in 1..=window as i64 {
            let future_frame = current_frame.saturating_add(offset);
            if future_frame > end_frame {
                break;
            }
            let readiness = self.media_preroll_frame_readiness(
                state,
                sequence,
                future_frame,
                width,
                height,
                0,
                color_context.clone(),
            );
            if !readiness.has_media {
                continue;
            }
            available_media_frames = available_media_frames.saturating_add(1);
            ready_prefix &= readiness.ready;
            if ready_prefix {
                ready_media_frames = ready_media_frames.saturating_add(1);
            }
        }
        Some(PreviewVideoPreroll { ready_media_frames, available_media_frames })
    }

    fn media_preroll_frame_readiness(
        &self,
        state: &AppState,
        sequence: &Sequence,
        frame: i64,
        target_width: u32,
        target_height: u32,
        depth: usize,
        color_context: ColorContext,
    ) -> MediaPrerollFrameReadiness {
        if depth > MAX_NESTED_SEQUENCE_RENDER_DEPTH {
            return MediaPrerollFrameReadiness::required_not_ready();
        }
        let Ok(evaluation) = evaluate_timeline_render_plan(
            sequence,
            TimelineEvaluationRequest::preview(
                frame.max(0),
                normalize_preview_resolution_scale(sequence.settings.preview.resolution_scale),
            ),
        ) else {
            return MediaPrerollFrameReadiness::required_not_ready();
        };

        let mut readiness = MediaPrerollFrameReadiness::default();
        for element in evaluation.elements {
            match element {
                TimelineRenderPlanElement::Media(media) => {
                    readiness.has_media = true;
                    let cached = self
                        .media_preview_key_for_asset(
                            state,
                            &media.asset_id,
                            media.color_space_override,
                            media.alpha_interpretation,
                            media.source_frame,
                            media.source_secs,
                            target_width,
                            target_height,
                            &color_context,
                            false,
                            false,
                        )
                        .is_some_and(|(key, _)| {
                            self.frame_store.borrow_mut().media_frame(&key).is_some()
                        });
                    readiness.ready &= cached;
                }
                TimelineRenderPlanElement::NestedSequence(nested) => {
                    let Some(nested_sequence) = state.sequence_by_id(nested.sequence_id) else {
                        return MediaPrerollFrameReadiness::required_not_ready();
                    };
                    let (nested_width, nested_height) =
                        preview_dimensions_for_state(state, nested_sequence);
                    let nested_context =
                        nested_sequence.settings.nested_render_color_context(color_context.clone());
                    readiness.merge(self.media_preroll_frame_readiness(
                        state,
                        nested_sequence,
                        nested.source_frame,
                        nested_width,
                        nested_height,
                        depth + 1,
                        nested_context,
                    ));
                }
                TimelineRenderPlanElement::SolidColor(_)
                | TimelineRenderPlanElement::Adjustment(_) => {}
            }
        }
        readiness
    }

    fn preview_decode_adaptive_hints(
        &self,
        access_mode: PreviewDecodeAccessMode,
        key: &MediaPreviewKey,
    ) -> PreviewDecodeAdaptiveHints {
        if access_mode != PreviewDecodeAccessMode::ScrubCursor {
            return PreviewDecodeAdaptiveHints::default();
        }
        let hints = self.scrub_adaptation.borrow_mut().observe_request(
            key.asset_id,
            key.source_micros,
            Instant::now(),
        );
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
        playback_current_deadline_at: Option<Instant>,
        demand_identity: Option<mondrian_playback::FrameDemandIdentity>,
        adaptive_hints: PreviewDecodeAdaptiveHints,
    ) -> bool {
        let generation = self.current_generation.get();
        let is_current_playback = priority == MediaPreviewRequestPriority::Current
            && access_mode == PreviewDecodeAccessMode::PlaybackCursor;
        if is_current_playback
            && self.playback_sustained_pressure_active()
            && self.playback_realtime_work_pending()
        {
            self.record_playback_current_sustained_pressure_skip();
            tracing::trace!(
                asset_id = %key.asset_id,
                source_frame = key.source_frame,
                "viewer preview skipped current playback decode while sustained pressure recovery has realtime work pending"
            );
            return false;
        }
        if is_current_playback {
            self.record_playback_current_deadline_budget(playback_deadline_remaining_us(
                playback_current_deadline_at,
            ));
        }
        let hardware_decode_request = self.hardware_decode_request_for_key(access_mode, &key);
        let hardware_decode_device_selector =
            self.hardware_decode_device_selector_for_access_mode(access_mode);
        let submission = self.scheduler.submit_job(MediaPreviewJob {
            key: key.clone(),
            source_secs,
            generation,
            priority,
            access_mode,
            adaptive_hints,
            hardware_decode_request,
            hardware_decode_device_selector,
            enqueued_at: Instant::now(),
            deadline_at: if access_mode == PreviewDecodeAccessMode::PlaybackCursor {
                playback_current_deadline_at
            } else {
                None
            },
            demand_identity,
            execution_id: None,
        });
        match submission {
            MediaPreviewRequestStatus::Scheduled { evicted_prefetch, evicted_still } => {
                bump(&self.metrics.enqueued_jobs);
                if evicted_prefetch.is_some() {
                    bump(&self.metrics.queue_evicted_prefetch_jobs);
                    bump(&self.metrics.queue_canceled_jobs);
                }
                if evicted_still.is_some() {
                    bump(&self.metrics.queue_evicted_still_jobs);
                    bump(&self.metrics.queue_canceled_jobs);
                }
                true
            }
            MediaPreviewRequestStatus::UpdatedQueued {
                priority_promoted,
                access_mode_changed: _,
                generation_changed: _,
            } => {
                if priority_promoted {
                    bump(&self.metrics.queue_promoted_current_jobs);
                }
                false
            }
            MediaPreviewRequestStatus::ReusedInFlight => false,
            #[cfg(test)]
            MediaPreviewRequestStatus::AlreadyPending { .. } => false,
            MediaPreviewRequestStatus::DroppedBackpressure => {
                bump(&self.metrics.queue_full_drops);
                tracing::trace!(
                    asset_id = %key.asset_id,
                    source_frame = key.source_frame,
                    "viewer preview request dropped by backpressure"
                );
                false
            }
            MediaPreviewRequestStatus::DroppedInvalidAccessMode => {
                bump(&self.metrics.queue_invalid_access_mode_drops);
                tracing::warn!(
                    asset_id = %key.asset_id,
                    source_frame = key.source_frame,
                    priority = ?priority,
                    access_mode = access_mode.as_str(),
                    "viewer preview request dropped because priority/access-mode pair is invalid"
                );
                false
            }
            MediaPreviewRequestStatus::Closed => {
                bump(&self.metrics.worker_disconnected_drops);
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
    )
    .map_err(|error| error.to_string())?;
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
    decode_startup_preroll_frames: Cell<u64>,
    decode_startup_preroll_total_duration_us: Cell<u64>,
    decode_startup_preroll_max_duration_us: Cell<u64>,
    decode_startup_preroll_queue_wait_total_us: Cell<u64>,
    decode_startup_preroll_queue_wait_max_us: Cell<u64>,
    decode_failures: Cell<u64>,
    decode_timeout_failures: Cell<u64>,
    decode_budget_exhausted_failures: Cell<u64>,
    decode_cancellation: RefCell<mondrian_playback::FrameCancellationEvidenceCollector>,
    decode_in_process_cpu_frames: Cell<u64>,
    decode_external_ffmpeg_cpu_rgba_frames: Cell<u64>,
    decode_playback_session_ring_hit_frames: Cell<u64>,
    decode_cache_hit_frames: Cell<u64>,
    decode_playback_cursor_frames: Cell<u64>,
    decode_scrub_cursor_frames: Cell<u64>,
    decode_random_access_still_frames: Cell<u64>,
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
    prefetch_skipped_current_work: Cell<u64>,
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
    playback_current_sustained_pressure_skips: Cell<u64>,
    playback_current_proxy_or_hardware_recommended_decisions: Cell<u64>,
    playback_current_native_import_unavailable_decisions: Cell<u64>,
    playback_current_hardware_fallback_not_engaged_decisions: Cell<u64>,
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
    color_intermediate_transform_calls: Cell<u64>,
    color_intermediate_transform_pixels: Cell<u64>,
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
    color_composite_blocked_domains: Cell<u64>,
    color_composite_blocked_media_effect_domain: Cell<u64>,
    color_composite_blocked_solid_effect_domain: Cell<u64>,
    color_composite_blocked_adjustment_effect_domain: Cell<u64>,
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
    working: &CpuColorFrame,
) -> u64 {
    let mut hasher = DefaultHasher::new();
    sequence_id.hash(&mut hasher);
    frame.hash(&mut hasher);
    width.hash(&mut hasher);
    height.hash(&mut hasher);
    working.descriptor().hash(&mut hasher);
    for pixel in &working.rgba_f32().data {
        for channel in pixel {
            channel.to_bits().hash(&mut hasher);
        }
    }
    hasher.finish()
}

#[cfg(test)]
fn preview_dimensions_for_sequence(sequence: &Sequence) -> (u32, u32) {
    preview_dimensions_for_sequence_at_runtime_scale(
        sequence,
        mondrian_playback::PreviewResolutionScale::Full,
    )
}

fn preview_dimensions_for_state(state: &AppState, sequence: &Sequence) -> (u32, u32) {
    preview_dimensions_for_sequence_at_runtime_scale(
        sequence,
        state.playback_preview_resolution_scale(),
    )
}

fn preview_dimensions_for_sequence_at_runtime_scale(
    sequence: &Sequence,
    runtime_scale: mondrian_playback::PreviewResolutionScale,
) -> (u32, u32) {
    let resolution = sequence.settings.resolution;
    let scale = normalize_preview_resolution_scale(sequence.settings.preview.resolution_scale);
    let divisor = runtime_scale.dimension_divisor();
    let width = ((resolution.width as f32 * scale).round() as u32).max(1).div_ceil(divisor);
    let height = ((resolution.height as f32 * scale).round() as u32).max(1).div_ceil(divisor);
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
        MonitorProfileStatus::ManagedIccCalibration { source_color_space, .. } => {
            Ok(source_color_space)
        }
        MonitorProfileStatus::ManagedColorSpace {
            source: mondrian_core::display_contract::MonitorProfileSource::OsIccProfile,
            ..
        } => Err(
            crate::app_ui::preview_gpu_output_blocker::PreviewGpuOutputBlocker::UnsupportedFeature {
                feature: "icc_monitor_calibration_processor".to_owned(),
                reason: "OS ICC profile was marked managed without a renderer calibration processor; preview refuses the uncalibrated output".to_owned(),
            },
        ),
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
    monitor_color_diagnostics: Option<RenderColorTransformDiagnostics>,
    color_stage_diagnostics: RenderColorStageDiagnostics,
    render_stage_durations: AppUiPreviewRenderStageDurations,
}

struct PreviewWorkingCompositeOutput {
    frame: CpuColorFrame,
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
        TimelineEffectColorRuntime::new(&color_context.engine, color_context.working_color_space),
        scratch,
    );
    let cpu_composite_us = app_duration_us(cpu_composite_started_at.elapsed());
    Ok(PreviewWorkingCompositeOutput {
        frame: composite.frame,
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

#[derive(Debug, Clone, PartialEq)]
struct CpuRasterPresentationContract {
    raster_color_space: mondrian_ui_core::RasterImageColorSpace,
}

fn cpu_raster_presentation_contract(
    requested: &ColorContext,
) -> Result<CpuRasterPresentationContract, String> {
    let program_output = requested.output_color_space.color().ok_or_else(|| {
        format!(
            "Program Output {:?} is not an encoded color identity",
            requested.output_color_space
        )
    })?;
    RenderMonitorAdaptation::new(program_output, ColorSpace::Srgb, requested.engine.clone())
        .map_err(|error| error.to_string())?;
    Ok(CpuRasterPresentationContract {
        raster_color_space: mondrian_ui_core::RasterImageColorSpace::Srgb,
    })
}

fn output_boundary_from_color_context(
    color_context: &ColorContext,
) -> Result<RenderOutputColorBoundary, String> {
    let output_color_space = color_context.output_color_space.color().ok_or_else(|| {
        format!(
            "preview output identity {:?} is not an encoded color space",
            color_context.output_color_space
        )
    })?;
    RenderOutputColorBoundary::from_intent(
        mondrian_renderer::RenderOutputColorBoundaryTarget::Display,
        output_color_space,
        &color_context.output_transform,
        color_context.tone_map,
        color_context.engine.clone(),
    )
    .map_err(|error| error.to_string())
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
    let boundary = output_boundary_from_color_context(color_context)
        .map_err(|error| format!("unsupported preview output transform: {error}"))?;
    let program_output = boundary.output_color_space;
    let adaptation = RenderMonitorAdaptation::new(
        program_output,
        ColorSpace::Srgb,
        color_context.engine.clone(),
    )
    .map_err(|error| format!("unsupported CPU raster monitor adaptation: {error}"))?;
    execute_cpu_program_monitor_boundary_rgba8(&composite.frame, &boundary, &adaptation)
        .map(|output| {
            render_stage_durations.cpu_output_boundary_us =
                app_duration_us(output_boundary_started_at.elapsed());
            PreviewCompositeOutput {
                rgba: output.rgba,
                composite_diagnostics: composite.composite_diagnostics,
                color_diagnostics: output.program_output.color_diagnostics,
                monitor_color_diagnostics: output.monitor_color_diagnostics,
                color_stage_diagnostics: output.stage_diagnostics,
                render_stage_durations,
            }
        })
        .map_err(|err| format!("viewer preview final color transform failed: {err}"))
}

fn playback_deadline_remaining_us(deadline_at: Option<Instant>) -> Option<u64> {
    deadline_at.map(|deadline| {
        deadline
            .saturating_duration_since(Instant::now())
            .as_micros()
            .min(u128::from(u64::MAX)) as u64
    })
}

fn media_preview_result_is_startup_preroll(result: &MediaPreviewResult) -> bool {
    result.priority == MediaPreviewRequestPriority::Prefetch
        && result.access_mode == PreviewDecodeAccessMode::PlaybackCursor
        && result.deadline_at.is_some()
}

mod media_execution;
use media_execution::*;
#[cfg(test)]
mod tests {
    use super::*;

    fn tt(frame: i64, time_base: mondrian_core::Rational) -> mondrian_core::TimelineTime {
        let numerator = frame.checked_mul(time_base.num).expect("test time fits i64");
        mondrian_core::TimelineTime::new(numerator, time_base.den).expect("valid test time")
    }

    fn cancellation_evidence(
        work_class: mondrian_playback::FrameWorkClass,
        cause: mondrian_playback::FrameCancellationCause,
        execution_us: u64,
        execution_to_checkpoint_us: Option<u64>,
        request_to_checkpoint_us: Option<u64>,
    ) -> mondrian_playback::FrameCancellationEvidenceReport {
        let mut collector = mondrian_playback::FrameCancellationEvidenceCollector::default();
        collector.observe(mondrian_playback::FrameCancellationObservation {
            work_class,
            cause,
            execution_duration: Duration::from_micros(execution_us),
            execution_to_checkpoint: execution_to_checkpoint_us.map(Duration::from_micros),
            request_to_checkpoint: request_to_checkpoint_us.map(Duration::from_micros),
        });
        collector.report()
    }

    use mondrian_assets::AssetLibrary;
    use mondrian_core::types::{AssetId, Rational};
    use mondrian_core::{ensure_mondrian_default_ocio_loaded, Color, ProjectColorManagement};
    use mondrian_effects::{get_or_compile_scheduled_effect_graph, EffectRenderPlan};
    use mondrian_media::info::{PixelFormat, VideoCodec};
    use mondrian_media::{
        DetectedColorInterpretation, HwAccelPixelFormat, MediaInfo, VideoColorDetectionMethod,
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
            .add_clip(
                Clip::new_solid_color(AssetId::new(), color, tt(0, tb), tt(24, tb))
                    .expect("valid clip"),
            )
            .expect("solid clip should be insertable");
        state.sequence = Some(sequence);
        state.seek(4);
        state
    }

    #[test]
    fn playback_generation_survives_frame_advance_but_not_discontinuity() {
        let mut state = state_with_solid_color_clip(Color::from_rgba8(12, 34, 56, 255));
        let sequence = state.sequence.as_ref().expect("sequence").clone();
        state.play();

        let current = ViewerPreviewGenerationKey::from_state(
            &state,
            &sequence,
            4,
            960,
            540,
            ColorSpace::Srgb,
        );
        let advanced = ViewerPreviewGenerationKey::from_state(
            &state,
            &sequence,
            5,
            960,
            540,
            ColorSpace::Srgb,
        );
        assert_eq!(
            current, advanced,
            "ordinary playback must retain forward prefetch work"
        );

        state.seek(6);
        let after_seek = ViewerPreviewGenerationKey::from_state(
            &state,
            &sequence,
            6,
            960,
            540,
            ColorSpace::Srgb,
        );
        assert_ne!(
            current, after_seek,
            "seek must invalidate the prior playback epoch"
        );

        state.pause();
        let idle_a = ViewerPreviewGenerationKey::from_state(
            &state,
            &sequence,
            6,
            960,
            540,
            ColorSpace::Srgb,
        );
        let idle_b = ViewerPreviewGenerationKey::from_state(
            &state,
            &sequence,
            7,
            960,
            540,
            ColorSpace::Srgb,
        );
        assert_ne!(
            idle_a, idle_b,
            "idle current-frame work remains latest-wins"
        );
    }

    #[test]
    fn app_frame_store_residency_covers_the_prefetch_window() {
        let diagnostics = PreviewCpuFrameStore::default().diagnostics();
        assert!(
            diagnostics.media_resource_unit_budget >= MEDIA_PREVIEW_FORWARD_PREFETCH_MAX_FRAMES
        );
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
                duration: Some(Duration::from_secs(2)),
                codec_profile: mondrian_media::VideoCodecProfile::HevcMain10,
                width: 3840,
                height: 2160,
                frame_rate: Rational::new(25, 1),
                frame_rate_proven: true,
                pixel_format: PixelFormat::Yuv420p10le,
                pixel_format_proven: true,
                color_range: DecodedVideoRange::Limited,
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
            .add_clip(Clip::new(asset_id, tt(0, tb), tt(50, tb)).expect("valid clip"))
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
                .add_clip(Clip::new(asset_id, tt(0, tb), tt(50, tb)).expect("valid clip"))
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

    fn calibrated_icc_display_snapshot(color_space: ColorSpace) -> DisplayOutputSnapshot {
        let mut snapshot = mondrian_core::display_probe::FakeDisplayProbe::sdr_pass().snapshot;
        snapshot.monitor_profile_status = MonitorProfileStatus::ManagedIccCalibration {
            source_color_space: color_space,
            profile_fingerprint:
                mondrian_core::display_calibration::IccProfileFingerprint::from_bytes(
                    b"test-monitor-profile",
                ),
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
    fn cpu_raster_presentation_contract_encodes_sdr_video_for_srgb_atlas() {
        let requested = test_color_context(ColorSpace::Rec709);

        let contract = cpu_raster_presentation_contract(&requested)
            .expect("Rec.709 viewer output has an sRGB raster presentation contract");

        assert_eq!(requested.output_color_space, ColorSpace::Rec709.into());
        assert_eq!(
            contract.raster_color_space,
            mondrian_ui_core::RasterImageColorSpace::Srgb
        );
    }

    #[test]
    fn cpu_raster_presentation_contract_adapts_wide_gamut_sdr_and_rejects_hdr() {
        let p3 = test_color_context(ColorSpace::DisplayP3);
        assert!(cpu_raster_presentation_contract(&p3).is_ok());

        for output in [ColorSpace::Rec2100Pq, ColorSpace::Rec2100Hlg] {
            let requested = test_color_context(output);
            let error = cpu_raster_presentation_contract(&requested)
                .expect_err("HDR to sRGB raster requires an explicit rendering policy");
            assert!(error.contains("dynamic-range class"));
        }
    }

    #[test]
    fn cpu_raster_preview_retains_program_output_before_srgb_adaptation() {
        let effect_graph = get_or_compile_scheduled_effect_graph(&EffectRenderPlan::default())
            .expect("default effect graph");
        let resolved = [ResolvedPreviewElement::SolidColor(
            TimelineSolidColorLayer {
                color: Color::from_rgba8(48, 96, 192, 255),
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_graph,
                frame_seed: 0,
            },
        )];
        let color_context = test_color_context(ColorSpace::Rec709);
        let service = AppUiPreviewService::new();
        let mut scratch = TimelineCompositeScratch::default();

        let output =
            composite_resolved_preview(&service, 2, 2, &resolved, &color_context, &mut scratch)
                .expect("CPU raster Program Output and monitor adaptation");

        assert_eq!(
            output.color_diagnostics.output.color_space,
            ColorSpace::Rec709.into()
        );
        let monitor = output.monitor_color_diagnostics.expect("sRGB adaptation diagnostics");
        assert_eq!(monitor.input.color_space, ColorSpace::Rec709.into());
        assert_eq!(monitor.output.color_space, ColorSpace::Srgb.into());
        assert_eq!(
            monitor.direction,
            RenderColorTransformDirection::Intermediate
        );
        assert!(!monitor.used_rgba8_boundary);
        assert_eq!(output.color_stage_diagnostics.cpu_output_stages, 2);
        assert_eq!(output.rgba.len(), 2 * 2 * 4);
    }

    #[test]
    fn preview_display_color_space_resolves_managed_monitor_profile() {
        let mut sequence = Sequence::new("p3-preview");
        sequence.settings.color_management.inherit = false;
        sequence.settings.color_management.display_management =
            mondrian_core::DisplayManagementPolicy {
                monitor_profile: mondrian_core::MonitorProfileReference::ColorSpace(
                    ColorSpace::DisplayP3,
                ),
                viewer_mode: mondrian_core::ViewerDisplayMode::Sdr,
                tone_map_policy: mondrian_core::DisplayToneMapPolicy::Automatic,
            };

        assert_eq!(
            preview_display_color_space(&sequence, &ProjectColorManagement::default(), None)
                .expect("display color space"),
            ColorSpace::DisplayP3
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
    fn preview_display_color_space_rejects_uncalibrated_managed_icc_status() {
        let mut sequence = Sequence::new("icc-preview");
        sequence.settings.color_management.inherit = false;
        sequence.settings.color_management.display_management =
            mondrian_core::DisplayManagementPolicy {
                monitor_profile: mondrian_core::MonitorProfileReference::IccProfile {
                    profile_id: "os-default".to_owned(),
                },
                viewer_mode: mondrian_core::ViewerDisplayMode::Sdr,
                tone_map_policy: mondrian_core::DisplayToneMapPolicy::Automatic,
            };
        let snapshot = managed_icc_display_snapshot(ColorSpace::DisplayP3);

        let err = preview_display_color_space(
            &sequence,
            &ProjectColorManagement::default(),
            Some(&snapshot),
        )
        .expect_err("ICC status without a renderer calibration processor must fail closed");
        assert!(matches!(
            err,
            crate::app_ui::preview_gpu_output_blocker::PreviewGpuOutputBlocker::UnsupportedFeature {
                ref feature,
                ..
            } if feature == "icc_monitor_calibration_processor"
        ));
    }

    #[test]
    fn preview_display_color_space_accepts_calibrated_icc_status() {
        let mut sequence = Sequence::new("icc-preview");
        sequence.settings.color_management.inherit = false;
        sequence.settings.color_management.display_management =
            mondrian_core::DisplayManagementPolicy {
                monitor_profile: mondrian_core::MonitorProfileReference::IccProfile {
                    profile_id: "os-default".to_owned(),
                },
                viewer_mode: mondrian_core::ViewerDisplayMode::Sdr,
                tone_map_policy: mondrian_core::DisplayToneMapPolicy::Automatic,
            };
        let snapshot = calibrated_icc_display_snapshot(ColorSpace::Srgb);

        assert_eq!(
            preview_display_color_space(
                &sequence,
                &ProjectColorManagement::default(),
                Some(&snapshot),
            )
            .expect("calibrated ICC display source"),
            ColorSpace::Srgb
        );
    }

    #[test]
    fn gpu_preview_frame_for_icc_policy_rejects_uncalibrated_monitor_profile() {
        let service = AppUiPreviewService::new();
        let state = state_with_icc_display_policy(Color::from_rgba8(24, 80, 160, 255));
        let snapshot = managed_icc_display_snapshot(ColorSpace::DisplayP3);
        service.set_display_output_snapshot(Some(&snapshot));

        assert!(matches!(
            service.gpu_preview_frame_for_state(&state),
            AppUiGpuPreviewFrameState::Unavailable
        ));
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
    fn paused_gpu_candidate_carries_untimed_presentation_authority() {
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
        assert_eq!(frame.working_color_space, WorkingColorSpace::LinearRec2020);
        match &frame.working_input {
            AppUiGpuPreviewWorkingInput::GpuComposite { layers } => {
                assert_eq!(layers.len(), 1);
                assert!(matches!(
                    layers[0],
                    ViewerGpuExecutionLayer::SolidColor { .. }
                ));
            }
        }
        assert!(frame.external_texture_key().starts_with("app-ui.viewer.gpu:"));
        assert_eq!(frame.preview_candidate_id(), 1);
        let ticket = frame.presentation_ticket().expect("paused current-frame presentation ticket");
        assert_eq!(ticket.deadline(), None);
        assert_eq!(ticket.identity().target_frame, state.current_frame());

        let diagnostics = service.diagnostics();
        assert_eq!(diagnostics.gpu_preview_candidate_requests, 1);
        assert_eq!(diagnostics.gpu_preview_candidate_ready, 1);
        assert_eq!(diagnostics.gpu_preview_candidate_current, 0);
        assert_eq!(diagnostics.gpu_preview_candidate_loading, 0);
        assert_eq!(diagnostics.gpu_preview_candidate_unavailable, 0);
        assert_eq!(diagnostics.gpu_preview_candidate_pixels, 960_u64 * 540);
    }

    #[test]
    fn gpu_candidate_separates_program_output_from_monitor_identity() {
        let service = AppUiPreviewService::new();
        let mut state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
        let baseline_frame = match service.gpu_preview_frame_for_state(&state) {
            AppUiGpuPreviewFrameState::Ready(frame) => frame,
            _ => panic!("expected baseline GPU preview candidate"),
        };
        state.project_settings.color_management.display_management.monitor_profile =
            mondrian_core::MonitorProfileReference::IccProfile {
                profile_id: "test-monitor".to_owned(),
            };
        let snapshot = calibrated_icc_display_snapshot(ColorSpace::Srgb);
        service.set_display_output_snapshot(Some(&snapshot));

        let frame = match service.gpu_preview_frame_for_state(&state) {
            AppUiGpuPreviewFrameState::Ready(frame) => frame,
            _ => panic!("expected ready GPU preview candidate"),
        };

        assert_eq!(
            frame.program_output_boundary.output_color_space,
            ColorSpace::Rec709
        );
        assert_eq!(
            frame.monitor_adaptation.program_output_color_space(),
            ColorSpace::Rec709
        );
        assert_eq!(
            frame.monitor_adaptation.monitor_color_space(),
            ColorSpace::Srgb
        );
        assert!(frame.monitor_adaptation.requires_pass());
        assert_ne!(frame.cache_key, baseline_frame.cache_key);
    }

    #[test]
    fn playing_gpu_candidate_carries_exact_presentation_ticket() {
        let service = AppUiPreviewService::new();
        let mut state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
        state.play();
        let identity =
            state.pending_playback_frame_demand_identity().expect("frame demand identity");

        let frame = match service.gpu_preview_frame_for_state(&state) {
            AppUiGpuPreviewFrameState::Ready(frame) => frame,
            _ => panic!("expected ready GPU preview candidate"),
        };

        let ticket = frame.presentation_ticket().expect("presentation ticket");
        assert_eq!(ticket.identity(), identity);
        assert!(
            !state.complete_frame_presentation(ticket, Instant::now()),
            "presenting the current frame must hold priming until lookahead is observed"
        );
        assert!(
            state.observe_video_preroll(0, 0),
            "procedural playback has no future media payload to preroll"
        );
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

        let layers =
            gpu_composite_layers_for_resolved(960, 540, &elements, WorkingColorSpace::LinearRec709)
                .expect("affine transformed media should stay on GPU composite path");

        assert_eq!(layers.len(), 1);
        match &layers[0] {
            ViewerGpuExecutionLayer::Media { opacity, transform: actual_transform, .. } => {
                assert_eq!(*opacity, 0.85);
                assert_eq!(*actual_transform, transform);
            }
            ViewerGpuExecutionLayer::SolidColor { .. }
            | ViewerGpuExecutionLayer::Adjustment { .. } => {
                panic!("expected media layer")
            }
        }
    }

    #[test]
    fn preview_transform_projection_does_not_double_apply_resolution_scale() {
        let mut media = test_media_frame_with_size(180, 960, 540, 42);
        media.logical_width = 3840;
        media.logical_height = 2160;

        let projected = project_preview_media_transform(
            [0.5, 0.0, 0.0, 0.0, 0.5, 0.0],
            &media,
            Resolution { width: 1920, height: 1080 },
            Resolution { width: 960, height: 540 },
        )
        .expect("valid preview projection");

        assert_eq!(projected, [1.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
    }

    #[test]
    fn gpu_composite_layers_lower_supported_working_effects() {
        let media = test_media_frame_with_size(180, 320, 180, 43);
        let mut graph = mondrian_effects::EffectGraphBuilderState::new();
        graph.append_unary(mondrian_effects::EffectRenderOp::WhiteBalance {
            temperature: 0.2,
            tint: -0.1,
        });
        graph.append_unary(mondrian_effects::EffectRenderOp::Vignette {
            intensity: 0.6,
            feather: 0.7,
        });
        let effect_graph = mondrian_effects::get_or_compile_scheduled_render_graph(graph.finish())
            .expect("compile supported effect graph");
        let elements = vec![ResolvedPreviewElement::Media {
            frame: media,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_graph,
            frame_seed: 19,
        }];

        let layers =
            gpu_composite_layers_for_resolved(320, 180, &elements, WorkingColorSpace::LinearRec709)
                .expect("supported effects should stay on GPU composite path");

        match &layers[0] {
            ViewerGpuExecutionLayer::Media { effect_plan, frame_seed, .. } => {
                assert_eq!(effect_plan.operations().len(), 2);
                assert_eq!(*frame_seed, 19);
            }
            ViewerGpuExecutionLayer::SolidColor { .. }
            | ViewerGpuExecutionLayer::Adjustment { .. } => {
                panic!("expected media layer")
            }
        }
    }

    #[test]
    fn gpu_composite_layers_lower_solid_and_adjustment_effects() {
        let mut graph = mondrian_effects::EffectGraphBuilderState::new();
        graph.append_unary(mondrian_effects::EffectRenderOp::ColorAdjust {
            exposure: 0.2,
            contrast: 1.1,
            saturation: 0.9,
        });
        let effect_graph = mondrian_effects::get_or_compile_scheduled_render_graph(graph.finish())
            .expect("compile supported effect graph");
        let elements = vec![
            ResolvedPreviewElement::SolidColor(TimelineSolidColorLayer {
                color: Color { r: 0.2, g: 0.4, b: 0.6, a: 1.0 },
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_graph: Arc::clone(&effect_graph),
                frame_seed: 11,
            }),
            ResolvedPreviewElement::Adjustment(TimelineAdjustmentLayer {
                effect_graph,
                opacity: 0.6,
                blend_mode: None,
                frame_seed: 13,
            }),
        ];

        let layers =
            gpu_composite_layers_for_resolved(320, 180, &elements, WorkingColorSpace::LinearRec709)
                .expect("solid and adjustment point effects should remain GPU-native");

        assert_eq!(layers.len(), 2);
        match &layers[0] {
            ViewerGpuExecutionLayer::SolidColor { effect_plan, .. } => {
                assert_eq!(effect_plan.operations().len(), 1);
            }
            _ => panic!("expected solid layer"),
        }
        match &layers[1] {
            ViewerGpuExecutionLayer::Adjustment {
                effect_plan,
                opacity,
                blend_mode,
                frame_seed,
            } => {
                assert_eq!(effect_plan.operations().len(), 1);
                assert_eq!(*opacity, 0.6);
                assert_eq!(*blend_mode, BlendMode::Normal);
                assert_eq!(*frame_seed, 13);
            }
            _ => panic!("expected adjustment layer"),
        }
    }

    #[test]
    fn gpu_composite_layers_skip_leading_adjustment_before_layer_limit() {
        let mut graph = mondrian_effects::EffectGraphBuilderState::new();
        graph.append_unary(mondrian_effects::EffectRenderOp::Grain { amount: 0.1 });
        let effect_graph = mondrian_effects::get_or_compile_scheduled_render_graph(graph.finish())
            .expect("compile supported effect graph");
        let mut elements = (0..5)
            .map(|seed| {
                ResolvedPreviewElement::Adjustment(TimelineAdjustmentLayer {
                    effect_graph: Arc::clone(&effect_graph),
                    opacity: 1.0,
                    blend_mode: None,
                    frame_seed: seed,
                })
            })
            .collect::<Vec<_>>();
        elements.push(ResolvedPreviewElement::SolidColor(
            TimelineSolidColorLayer {
                color: Color { r: 0.1, g: 0.2, b: 0.3, a: 1.0 },
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_graph: mondrian_effects::identity_compiled_effect_graph()
                    .expect("identity compiled graph"),
                frame_seed: 0,
            },
        ));

        let layers =
            gpu_composite_layers_for_resolved(320, 180, &elements, WorkingColorSpace::LinearRec709)
                .expect("non-rendering leading adjustments should not consume GPU layer capacity");

        assert_eq!(layers.len(), 1);
        assert!(matches!(
            layers[0],
            ViewerGpuExecutionLayer::SolidColor { .. }
        ));
    }

    #[test]
    fn gpu_composite_layers_accept_source_only_media_frame() {
        let source = CpuEncodedColorFrame::source_rgba8(
            320,
            180,
            ColorSpace::Rec709,
            vec![0; 320 * 180 * 4],
        );
        let input_transform = RenderInputTransform::to_working(
            WorkingColorSpace::LinearRec709,
            false,
            ColorEngine::mondrian_standard(),
        );
        let media = MediaPreviewFrame {
            width: 320,
            height: 180,
            logical_width: 320,
            logical_height: 180,
            frame: None,
            gpu_source: Some(MediaPreviewGpuSourceFrame::new(source, input_transform)),
            native_source: None,
            signature: 44,
            presentation_quality: mondrian_playback::FramePresentationQuality::Ready,
            decode_execution: AppUiPreviewDecodeExecutionSummary::from_path(
                PreviewDecodeExecutionPath::SoftwareCpu,
            ),
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

        let layers =
            gpu_composite_layers_for_resolved(960, 540, &elements, WorkingColorSpace::LinearRec709)
                .expect("source-only media should stay on GPU input/composite path");

        match &layers[0] {
            ViewerGpuExecutionLayer::Media { frame, gpu_source, native_source, .. } => {
                assert!(frame.is_none());
                assert!(gpu_source.is_some());
                assert!(native_source.is_none());
            }
            ViewerGpuExecutionLayer::SolidColor { .. }
            | ViewerGpuExecutionLayer::Adjustment { .. } => {
                panic!("expected media layer")
            }
        }
    }

    #[test]
    fn gpu_composite_layers_preserve_native_source_only_media_frame() {
        let media = MediaPreviewFrame {
            width: 320,
            height: 180,
            logical_width: 320,
            logical_height: 180,
            frame: None,
            gpu_source: None,
            native_source: Some(test_native_source_frame(320, 180)),
            signature: 45,
            presentation_quality: mondrian_playback::FramePresentationQuality::Ready,
            decode_execution: AppUiPreviewDecodeExecutionSummary::default(),
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

        let layers =
            gpu_composite_layers_for_resolved(960, 540, &elements, WorkingColorSpace::LinearRec709)
                .expect("native source-only media should reach GPU composite admission");

        match &layers[0] {
            ViewerGpuExecutionLayer::Media { frame, gpu_source, native_source, .. } => {
                assert!(frame.is_none());
                assert!(gpu_source.is_none());
                let native_source = native_source.as_ref().expect("native source");
                assert_eq!(
                    native_source.input_transform.backend,
                    mondrian_renderer::RenderColorTransformBackend::OcioGpuShaderPlan
                );
                assert_eq!(
                    native_source.native_frame.handle_kind(),
                    DecodedGpuFrameHandleKind::D3D11Texture2D
                );
                assert_eq!(
                    native_source.native_frame.surface_format,
                    DecodedVideoSurfaceFormat::P010
                );
                assert_eq!(native_source.native_frame.handle.id().get(), 7);
                assert_eq!(
                    native_source.native_frame.diagnostics.decoded_video_sampling,
                    DecodedVideoSampling {
                        matrix: mondrian_media::DecodedVideoMatrix::Bt709,
                        range: DecodedVideoRange::Limited,
                        chroma_location: DecodedVideoChromaLocation::Left,
                        bit_depth: 10,
                    }
                );
                let descriptor = native_source
                    .native_frame
                    .native_decoded_frame_source_descriptor()
                    .expect("validated media payload maps to renderer source descriptor");
                assert_eq!((descriptor.width, descriptor.height), (320, 180));
                assert_eq!(
                    descriptor.handle_kind,
                    DecodedGpuFrameHandleKind::D3D11Texture2D
                );
                assert_eq!(
                    descriptor.source_texture_format,
                    GpuNativeDecodedFrameTextureFormat::P010
                );
            }
            ViewerGpuExecutionLayer::SolidColor { .. }
            | ViewerGpuExecutionLayer::Adjustment { .. } => {
                panic!("expected media layer")
            }
        }
    }

    #[test]
    fn native_source_only_media_frame_fails_cpu_working_fallback() {
        let frame = MediaPreviewFrame {
            width: 320,
            height: 180,
            logical_width: 320,
            logical_height: 180,
            frame: None,
            gpu_source: None,
            native_source: Some(test_native_source_frame(320, 180)),
            signature: 46,
            presentation_quality: mondrian_playback::FramePresentationQuality::Ready,
            decode_execution: AppUiPreviewDecodeExecutionSummary::default(),
        };

        let err = match frame.working_frame() {
            Ok(_) => panic!("native source must not be reinterpreted as CPU RGBA"),
            Err(err) => err,
        };

        assert!(err.contains("native GPU decoded"));
        assert!(err.contains("requires renderer native import"));
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

        let err = match gpu_composite_layers_for_resolved(
            960,
            540,
            &elements,
            WorkingColorSpace::LinearRec709,
        ) {
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

        assert!(service.set_external_viewer_frame(
            &frame,
            key.clone(),
            ViewerExternalTexturePresentation::full_frame(frame.width, frame.height)
                .expect("full-frame presentation"),
        ));
        match service.gpu_preview_frame_for_state(&state) {
            AppUiGpuPreviewFrameState::Current => {}
            _ => panic!("expected current external GPU preview frame"),
        }
        match service.viewer_preview_for_state(&state) {
            ViewerPreviewState::Ready(ViewerFrameContent::ExternalTexture(frame)) => {
                assert_eq!(frame.key, key);
                assert_eq!(frame.width, 960);
                assert_eq!(frame.height, 540);
                assert_eq!(
                    frame.presentation,
                    ViewerExternalTexturePresentation::full_frame(960, 540)
                );
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
    fn pending_replacement_prefers_last_presented_gpu_frame() {
        let service = AppUiPreviewService::new();
        let state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
        let frame = match service.gpu_preview_frame_for_state(&state) {
            AppUiGpuPreviewFrameState::Ready(frame) => frame,
            _ => panic!("expected ready GPU preview candidate"),
        };
        assert!(service.set_external_viewer_frame(
            &frame,
            "viewer:last-presented",
            ViewerExternalTexturePresentation::full_frame(frame.width, frame.height)
                .expect("full-frame presentation"),
        ));
        let sequence = state.sequence.as_ref().expect("test sequence");

        match service.stale_viewer_content_for_sequence(sequence, frame.width, frame.height) {
            Some(ViewerFrameContent::ExternalTexture(stale)) => {
                assert_eq!(stale.key, "viewer:last-presented");
            }
            other => panic!("expected retained external frame, got {other:?}"),
        }
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
        assert_eq!(diagnostics.input_color_resolution_missing_assume_rec709, 0);
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
        assert_eq!(diagnostics.color_intermediate_transform_calls, 1);
        assert_eq!(
            diagnostics.color_intermediate_transform_pixels,
            960_u64 * 540
        );
        assert_eq!(diagnostics.color_rgba8_boundary_calls, 0);
        assert_eq!(diagnostics.color_stage_plans, 1);
        assert_eq!(diagnostics.color_stage_total_stages, 2);
        assert_eq!(diagnostics.color_stage_cpu_input_stages, 0);
        assert_eq!(diagnostics.color_stage_cpu_output_stages, 2);
        assert_eq!(diagnostics.color_stage_gpu_color_stages, 0);
        assert_eq!(diagnostics.color_stage_upload_stages, 0);
        assert_eq!(diagnostics.color_stage_readback_stages, 0);
        assert_eq!(diagnostics.color_stage_gpu_blockers, 0);
        assert_eq!(diagnostics.color_stage_gpu_shader_module_blockers, 0);
        assert_eq!(diagnostics.color_stage_gpu_ocio_resource_blockers, 0);
        assert_eq!(diagnostics.color_stage_gpu_wrapper_blockers, 0);
        assert_eq!(diagnostics.color_stage_gpu_render_pipeline_blockers, 0);
        assert_eq!(diagnostics.color_stage_pixels, 2 * 960_u64 * 540);
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

    fn test_preview_decode_diagnostics(
        access_mode: PreviewDecodeAccessMode,
        hardware_decode_decision: PreviewHardwareDecodeDecision,
        hardware_decode_blocker: PreviewHardwareDecodeBlocker,
    ) -> PreviewDecodeDiagnostics {
        PreviewDecodeDiagnostics {
            path: PreviewDecodePath::InProcessFfmpegCpuRgba,
            elapsed_us: 1_000,
            cache_hit: false,
            access_mode,
            external_process: false,
            cpu_resident: true,
            seek_performed: false,
            requested_pts: None,
            selected_pts: None,
            temporal_approximation: false,
            seek_strategy: PreviewDecodeSeekStrategy::KeyframeBefore,
            forward_reuse_frame_window: 0,
            forward_decode_budget_frames: 0,
            any_seek_window_ms: 0,
            scrub_adaptive_class: PreviewScrubAdaptiveClass::Normal,
            hardware_decode_request: PreviewHardwareDecodeRequest::PreferGpuResident,
            hardware_decode_decision,
            hardware_decode_candidate_backend: Some(HwAccelBackend::D3D11VA),
            hardware_decode_candidate_handle_kind: Some(DecodedGpuFrameHandleKind::D3D11Texture2D),
            hardware_decode_adapter_available: true,
            hardware_decode_ffmpeg_device_type_available: true,
            hardware_decode_ffmpeg_codec_config_available: true,
            hardware_decode_ffmpeg_hw_pixel_format: Some(HwAccelPixelFormat::D3D11),
            hardware_decode_ffmpeg_device_context_attempted: true,
            hardware_decode_ffmpeg_device_context_created: true,
            hardware_decode_ffmpeg_device_context_error_code: None,
            hardware_decode_cpu_transfer_configured: false,
            hardware_decode_cpu_transfer_observed: false,
            hardware_decode_cpu_transfer_status:
                PreviewHardwareDecodeCpuTransferStatus::NotAttempted,
            session_reused: false,
            forward_reused: false,
            seek_index_available: false,
            seek_index_keyframes: 0,
            seek_index_observed_packets: 0,
            seek_index_source: PreviewSeekIndexSource::None,
            seek_index_used: false,
            seek_index_anchor_pts: None,
            decoded_frame_count: 1,
            threading_kind: PreviewDecodeThreadingKind::Frame,
            threading_count: 4,
            stage_durations: PreviewDecodeStageDurations::default(),
            hw_accel_backend: HwAccelBackend::D3D11VA,
            hardware_decode_active: false,
            zero_copy_active: false,
            decoded_frame_residency: DecodedFrameResidency::CpuRgba,
            gpu_frame_handle_kind: None,
            hardware_decode_blocker,
            native_decode_fallback: None,
            decoded_surface_format: DecodedVideoSurfaceFormat::P010,
            decoded_video_sampling: DecodedVideoSampling::default(),
        }
    }

    #[test]
    fn nearest_keyframe_scrub_is_explicitly_degraded_until_settled() {
        let mut diagnostics = test_preview_decode_diagnostics(
            PreviewDecodeAccessMode::ScrubCursor,
            PreviewHardwareDecodeDecision::CpuRgbaNotRequested,
            PreviewHardwareDecodeBlocker::None,
        );
        diagnostics.hardware_decode_request = PreviewHardwareDecodeRequest::Auto;
        diagnostics.requested_pts = Some(1_000);
        diagnostics.selected_pts = Some(960);
        diagnostics.temporal_approximation = true;

        assert_eq!(
            preview_decode_presentation_quality(&diagnostics),
            mondrian_playback::FramePresentationQuality::Degraded
        );

        diagnostics.selected_pts = diagnostics.requested_pts;
        diagnostics.temporal_approximation = false;
        assert_eq!(
            preview_decode_presentation_quality(&diagnostics),
            mondrian_playback::FramePresentationQuality::Ready
        );
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
                requested_pts: Some(100),
                selected_pts: Some(100),
                temporal_approximation: false,
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
                hardware_decode_ffmpeg_device_type_available: false,
                hardware_decode_ffmpeg_codec_config_available: false,
                hardware_decode_ffmpeg_hw_pixel_format: None,
                hardware_decode_ffmpeg_device_context_attempted: false,
                hardware_decode_ffmpeg_device_context_created: false,
                hardware_decode_ffmpeg_device_context_error_code: None,
                hardware_decode_cpu_transfer_configured: false,
                hardware_decode_cpu_transfer_observed: false,
                hardware_decode_cpu_transfer_status:
                    PreviewHardwareDecodeCpuTransferStatus::NotAttempted,
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
                    hardware_transfer_us: 0,
                    swscale_us: 70,
                    rgba_copy_us: 30,
                    external_process_us: 0,
                },
                hw_accel_backend: HwAccelBackend::None,
                hardware_decode_active: false,
                zero_copy_active: false,
                decoded_frame_residency: DecodedFrameResidency::CpuRgba,
                gpu_frame_handle_kind: None,
                hardware_decode_blocker: PreviewHardwareDecodeBlocker::TextureResidencyNotConnected,
                native_decode_fallback: None,
                decoded_surface_format: DecodedVideoSurfaceFormat::P010,
                decoded_video_sampling: DecodedVideoSampling::default(),
            },
            MediaPreviewRequestPriority::Current,
            1_200,
            true,
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
                requested_pts: None,
                selected_pts: None,
                temporal_approximation: false,
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
                hardware_decode_ffmpeg_device_type_available: false,
                hardware_decode_ffmpeg_codec_config_available: false,
                hardware_decode_ffmpeg_hw_pixel_format: None,
                hardware_decode_ffmpeg_device_context_attempted: false,
                hardware_decode_ffmpeg_device_context_created: false,
                hardware_decode_ffmpeg_device_context_error_code: None,
                hardware_decode_cpu_transfer_configured: false,
                hardware_decode_cpu_transfer_observed: false,
                hardware_decode_cpu_transfer_status:
                    PreviewHardwareDecodeCpuTransferStatus::NotAttempted,
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
                    hardware_transfer_us: 0,
                    swscale_us: 0,
                    rgba_copy_us: 0,
                    external_process_us: 2_450,
                },
                hw_accel_backend: HwAccelBackend::None,
                hardware_decode_active: false,
                zero_copy_active: false,
                decoded_frame_residency: DecodedFrameResidency::CpuRgba,
                gpu_frame_handle_kind: None,
                hardware_decode_blocker: PreviewHardwareDecodeBlocker::TextureResidencyNotConnected,
                native_decode_fallback: None,
                decoded_surface_format: DecodedVideoSurfaceFormat::Unknown,
                decoded_video_sampling: DecodedVideoSampling::default(),
            },
            MediaPreviewRequestPriority::Current,
            400,
            true,
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
                requested_pts: None,
                selected_pts: None,
                temporal_approximation: false,
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
                hardware_decode_ffmpeg_device_type_available: false,
                hardware_decode_ffmpeg_codec_config_available: false,
                hardware_decode_ffmpeg_hw_pixel_format: None,
                hardware_decode_ffmpeg_device_context_attempted: false,
                hardware_decode_ffmpeg_device_context_created: false,
                hardware_decode_ffmpeg_device_context_error_code: None,
                hardware_decode_cpu_transfer_configured: false,
                hardware_decode_cpu_transfer_observed: false,
                hardware_decode_cpu_transfer_status:
                    PreviewHardwareDecodeCpuTransferStatus::NotAttempted,
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
                    hardware_transfer_us: 0,
                    swscale_us: 0,
                    rgba_copy_us: 0,
                    external_process_us: 0,
                },
                hw_accel_backend: HwAccelBackend::None,
                hardware_decode_active: false,
                zero_copy_active: false,
                decoded_frame_residency: DecodedFrameResidency::CpuRgba,
                gpu_frame_handle_kind: None,
                hardware_decode_blocker: PreviewHardwareDecodeBlocker::TextureResidencyNotConnected,
                native_decode_fallback: None,
                decoded_surface_format: DecodedVideoSurfaceFormat::Nv12,
                decoded_video_sampling: DecodedVideoSampling::default(),
            },
            MediaPreviewRequestPriority::Current,
            20,
            true,
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
                requested_pts: None,
                selected_pts: None,
                temporal_approximation: false,
                seek_strategy: PreviewDecodeSeekStrategy::KeyframeBefore,
                forward_reuse_frame_window: 3,
                forward_decode_budget_frames: 48,
                any_seek_window_ms: 0,
                scrub_adaptive_class: PreviewScrubAdaptiveClass::Normal,
                hardware_decode_request: PreviewHardwareDecodeRequest::PreferGpuResident,
                hardware_decode_decision: PreviewHardwareDecodeDecision::HardwareDecodeCpuTransfer,
                hardware_decode_candidate_backend: Some(HwAccelBackend::D3D11VA),
                hardware_decode_candidate_handle_kind: Some(
                    DecodedGpuFrameHandleKind::D3D11Texture2D,
                ),
                hardware_decode_adapter_available: false,
                hardware_decode_ffmpeg_device_type_available: true,
                hardware_decode_ffmpeg_codec_config_available: true,
                hardware_decode_ffmpeg_hw_pixel_format: Some(HwAccelPixelFormat::D3D11),
                hardware_decode_ffmpeg_device_context_attempted: true,
                hardware_decode_ffmpeg_device_context_created: true,
                hardware_decode_ffmpeg_device_context_error_code: None,
                hardware_decode_cpu_transfer_configured: true,
                hardware_decode_cpu_transfer_observed: true,
                hardware_decode_cpu_transfer_status:
                    PreviewHardwareDecodeCpuTransferStatus::Observed,
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
                    hardware_transfer_us: 42,
                    swscale_us: 0,
                    rgba_copy_us: 0,
                    external_process_us: 0,
                },
                hw_accel_backend: HwAccelBackend::D3D11VA,
                hardware_decode_active: true,
                zero_copy_active: false,
                decoded_frame_residency: DecodedFrameResidency::CpuRgba,
                gpu_frame_handle_kind: None,
                hardware_decode_blocker: PreviewHardwareDecodeBlocker::TextureResidencyNotConnected,
                native_decode_fallback: None,
                decoded_surface_format: DecodedVideoSurfaceFormat::Nv12,
                decoded_video_sampling: DecodedVideoSampling::default(),
            },
            MediaPreviewRequestPriority::Current,
            0,
            true,
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
            Some(50),
            false,
        );
        service.record_preview_decode_cancel(
            PreviewDecodeAccessMode::ScrubCursor,
            Some(MediaPreviewCancelReason::Obsolete),
            1_400,
            Some(1_000),
            Some(200),
            false,
        );
        service.record_preview_decode_cancel(
            PreviewDecodeAccessMode::RandomAccessStillFrame,
            Some(MediaPreviewCancelReason::Shutdown),
            20,
            Some(5),
            Some(5),
            false,
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
        assert_eq!(diagnostics.decode_cancel_observation_samples, 3);
        assert_eq!(diagnostics.decode_cancel_observation_total_us, 255);
        assert_eq!(diagnostics.decode_cancel_observation_max_us, 200);
        assert_eq!(diagnostics.decode_cancel_observation_last_us, 5);
        assert_eq!(diagnostics.decode_canceled_return_latency_total_us, 515);
        assert_eq!(diagnostics.decode_canceled_return_latency_max_us, 400);
        assert_eq!(diagnostics.decode_canceled_return_latency_last_us, 15);
        assert_eq!(diagnostics.decode_in_process_cpu_frames, 1);
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
        assert_eq!(diagnostics.decode_stage_durations.hardware_transfer_us, 42);
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
        assert_eq!(playback_profile.cancel_observation_samples, 1);
        assert_eq!(playback_profile.cancel_observation_total_us, 50);
        assert_eq!(playback_profile.cancel_observation_max_us, 50);
        assert_eq!(playback_profile.cancel_observation_last_us, 50);
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
        assert_eq!(playback_profile.hardware_decode_active_frames, 1);
        assert_eq!(playback_profile.zero_copy_active_frames, 0);
        assert_eq!(playback_profile.gpu_texture_resident_frames, 0);
        assert_eq!(playback_profile.decoded_nv12_surface_frames, 1);
        assert_eq!(playback_profile.decoded_p010_surface_frames, 0);
        assert_eq!(
            playback_profile.hardware_decode_texture_residency_blocker_frames,
            2
        );
        assert_eq!(playback_profile.hardware_decode_auto_requested_frames, 0);
        assert_eq!(
            playback_profile.hardware_decode_prefer_hardware_requested_frames,
            0
        );
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
            0
        );
        assert_eq!(playback_profile.hardware_decode_codec_unsupported_frames, 0);
        assert_eq!(
            playback_profile.hardware_decode_device_context_attempted_frames,
            1
        );
        assert_eq!(
            playback_profile.hardware_decode_device_context_created_frames,
            1
        );
        assert_eq!(
            playback_profile.hardware_decode_device_context_unavailable_frames,
            0
        );
        assert_eq!(playback_profile.hardware_decode_cpu_transfer_frames, 1);
        assert_eq!(
            playback_profile.hardware_decode_cpu_transfer_configured_frames,
            1
        );
        assert_eq!(
            playback_profile.hardware_decode_cpu_transfer_observed_frames,
            1
        );
        assert_eq!(playback_profile.hardware_decode_backend_boundary_frames, 1);
        assert_eq!(
            playback_profile.hardware_decode_gpu_resident_native_frames,
            0
        );
        assert_eq!(playback_profile.hardware_decode_candidate_d3d12va_frames, 0);
        assert_eq!(playback_profile.hardware_decode_candidate_d3d11va_frames, 1);
        assert_eq!(playback_profile.hardware_decode_candidate_dxva2_frames, 0);
        assert_eq!(
            playback_profile.hardware_decode_candidate_videotoolbox_frames,
            0
        );
        assert_eq!(playback_profile.hardware_decode_candidate_vaapi_frames, 0);
        assert_eq!(playback_profile.hardware_decode_candidate_vdpau_frames, 0);
        assert_eq!(playback_profile.hardware_decode_candidate_cuda_frames, 0);
        assert_eq!(
            playback_profile.hardware_decode_adapter_unavailable_frames,
            1
        );
        assert_eq!(playback_profile.seek_index_used_frames, 0);
        assert_eq!(playback_profile.seek_index_keyframes_max, 4);
        assert_eq!(playback_profile.seek_index_observed_packets_max, 128);
        assert_eq!(playback_profile.stage_durations.cache_lookup_us, 12);
        assert_eq!(playback_profile.stage_durations.hardware_transfer_us, 42);
        assert_eq!(
            playback_profile.max_frame_stage_durations.external_process_us,
            2_450
        );
        let scrub_profile = diagnostics.decode_access_mode_profiles.scrub_cursor;
        assert_eq!(scrub_profile.frames, 1);
        assert_eq!(scrub_profile.in_process_cpu_frames, 1);
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
        assert_eq!(
            scrub_profile.hardware_decode_prefer_hardware_requested_frames,
            0
        );
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
        assert_eq!(scrub_profile.cancel_observation_samples, 1);
        assert_eq!(scrub_profile.cancel_observation_total_us, 200);
        assert_eq!(scrub_profile.cancel_observation_max_us, 200);
        assert_eq!(scrub_profile.cancel_observation_last_us, 200);
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
        assert_eq!(still_profile.cancel_observation_samples, 1);
        assert_eq!(still_profile.cancel_observation_total_us, 5);
        assert_eq!(still_profile.cancel_observation_max_us, 5);
        assert_eq!(still_profile.cancel_observation_last_us, 5);
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
        assert_eq!(
            still_profile.hardware_decode_prefer_hardware_requested_frames,
            0
        );
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
            Some(10),
            false,
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
            Some(0),
            true,
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
            Some(30),
            false,
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
            decode_in_process_cpu_frames: 1,
            decode_scrub_cursor_frames: 1,
            decode_access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles {
                scrub_cursor: AppUiPreviewDecodeAccessModeProfile {
                    frames: 1,
                    in_process_cpu_frames: 1,
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
            decode_in_process_cpu_frames: 1,
            decode_scrub_cursor_frames: 1,
            decode_access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles {
                scrub_cursor: AppUiPreviewDecodeAccessModeProfile {
                    frames: 1,
                    in_process_cpu_frames: 1,
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
    fn startup_preroll_decode_evidence_is_separate_from_steady_state_latency() {
        let service = AppUiPreviewService::new_without_workers_for_test();
        let mut decode = test_preview_decode_diagnostics(
            PreviewDecodeAccessMode::PlaybackCursor,
            PreviewHardwareDecodeDecision::GpuResidentNative,
            PreviewHardwareDecodeBlocker::None,
        );
        decode.elapsed_us = 280_000;

        service.record_startup_preroll_decode(decode, 210_000);

        let diagnostics = service.diagnostics();
        assert_eq!(diagnostics.decode_startup_preroll_frames, 1);
        assert_eq!(
            diagnostics.decode_startup_preroll_total_duration_us,
            280_000
        );
        assert_eq!(diagnostics.decode_startup_preroll_max_duration_us, 280_000);
        assert_eq!(
            diagnostics.decode_startup_preroll_queue_wait_total_us,
            210_000
        );
        assert_eq!(
            diagnostics.decode_startup_preroll_queue_wait_max_us,
            210_000
        );
        assert_eq!(diagnostics.decode_total_duration_us, 0);
        assert_eq!(diagnostics.decode_queue_wait_total_us, 0);
        assert_eq!(
            diagnostics.decode_access_mode_profiles.playback_cursor.frames,
            0
        );

        service.shutdown();
    }

    #[test]
    fn playback_video_preroll_requires_next_media_payload_and_observes_cache_residency() {
        let (mut state, asset_id, root) = state_with_invalid_video_asset();
        state.play();
        let service = AppUiPreviewService::new_without_workers_for_test();
        let sequence = state.sequence.as_ref().expect("media sequence");
        let preroll_window =
            media_preview_forward_prefetch_window_frames(sequence.settings.frame_rate)
                .expect("valid media sequence frame rate");

        assert_eq!(
            service.playback_video_preroll_readiness(&state),
            Some(PreviewVideoPreroll {
                ready_media_frames: 0,
                available_media_frames: preroll_window,
            })
        );

        let frame = state.current_frame().saturating_add(1);
        let evaluation = evaluate_timeline_render_plan(
            sequence,
            TimelineEvaluationRequest::preview(
                frame,
                normalize_preview_resolution_scale(sequence.settings.preview.resolution_scale),
            ),
        )
        .expect("next frame render plan");
        let media = evaluation
            .elements
            .into_iter()
            .find_map(|element| match element {
                TimelineRenderPlanElement::Media(media) => Some(media),
                _ => None,
            })
            .expect("next frame media");
        assert_eq!(media.asset_id, asset_id);
        let (width, height) = preview_dimensions_for_state(&state, sequence);
        let display_color_space =
            preview_display_color_space(sequence, &state.project_settings.color_management, None)
                .expect("default display contract");
        let color_context = sequence.settings.root_preview_color_context(
            &state.project_settings.color_management,
            display_color_space,
        );
        let (key, _) = service
            .media_preview_key_for_asset(
                &state,
                &media.asset_id,
                media.color_space_override,
                media.alpha_interpretation,
                media.source_frame,
                media.source_secs,
                width,
                height,
                &color_context,
                false,
                false,
            )
            .expect("next media cache key");
        service
            .frame_store
            .borrow_mut()
            .insert_media_frame(key, test_media_frame(7), false);

        assert_eq!(
            service.playback_video_preroll_readiness(&state),
            Some(PreviewVideoPreroll {
                ready_media_frames: 1,
                available_media_frames: preroll_window,
            })
        );

        service.shutdown();
        drop(state);
        std::fs::remove_dir_all(root).expect("remove preview test root");
    }

    #[test]
    fn playback_video_preroll_does_not_delay_procedural_future_frames() {
        let mut state = state_with_solid_color_clip(Color::WHITE);
        state.seek(0);
        state.play();
        let service = AppUiPreviewService::new_without_workers_for_test();

        assert_eq!(
            service.playback_video_preroll_readiness(&state),
            Some(PreviewVideoPreroll { ready_media_frames: 0, available_media_frames: 0 })
        );

        service.shutdown();
    }

    #[test]
    fn preview_playback_schedule_counts_native_import_unavailable_current_frames() {
        let service = AppUiPreviewService::new();
        service.set_playback_hardware_decode_admission(AppUiPlaybackHardwareDecodeAdmission {
            request: PreviewHardwareDecodeRequest::PreferHardwareDecode,
            hardware_decode_device_selector: None,
            renderer_native_import_ready: false,
            platform_native_import_ready: false,
            native_import_admission_ready: false,
            admission_blocker: Some(
                AppUiPreviewHardwareDecodeAdmissionBlocker::RendererImportUnavailable,
            ),
            platform_discovery_available: true,
            platform_zero_copy_supported: false,
            platform_low_copy_fallback_supported: false,
            renderer_supported_handle_kinds: 0,
            renderer_supported_source_texture_formats: 0,
            renderer_supports_nv12: false,
            renderer_supports_p010: false,
        });

        service.record_preview_decode(
            test_preview_decode_diagnostics(
                PreviewDecodeAccessMode::PlaybackCursor,
                PreviewHardwareDecodeDecision::CpuRgbaHardwareUnavailable,
                PreviewHardwareDecodeBlocker::TextureResidencyNotConnected,
            ),
            MediaPreviewRequestPriority::Current,
            0,
            true,
        );
        service.record_preview_decode(
            test_preview_decode_diagnostics(
                PreviewDecodeAccessMode::ScrubCursor,
                PreviewHardwareDecodeDecision::CpuRgbaHardwareUnavailable,
                PreviewHardwareDecodeBlocker::TextureResidencyNotConnected,
            ),
            MediaPreviewRequestPriority::Current,
            0,
            true,
        );
        let mut effective_cpu_transfer = test_preview_decode_diagnostics(
            PreviewDecodeAccessMode::PlaybackCursor,
            PreviewHardwareDecodeDecision::HardwareDecodeCpuTransfer,
            PreviewHardwareDecodeBlocker::TextureResidencyNotConnected,
        );
        effective_cpu_transfer.hardware_decode_cpu_transfer_observed = true;
        service.record_preview_decode(
            effective_cpu_transfer,
            MediaPreviewRequestPriority::Current,
            0,
            true,
        );

        let diagnostics = service.diagnostics().playback_schedule;
        assert_eq!(diagnostics.current_native_import_unavailable_decisions, 2);
        assert_eq!(
            diagnostics.current_hardware_fallback_not_engaged_decisions,
            1
        );
        assert_eq!(
            diagnostics.current_proxy_or_hardware_recommended_decisions,
            2
        );
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
            decode_in_process_cpu_frames: 1,
            decode_access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles {
                random_access_still: AppUiPreviewDecodeAccessModeProfile {
                    frames: 1,
                    in_process_cpu_frames: 1,
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
            decode_in_process_cpu_frames: 1,
            decode_access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles {
                random_access_still: AppUiPreviewDecodeAccessModeProfile {
                    frames: 1,
                    in_process_cpu_frames: 1,
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
            decode_in_process_cpu_frames: 1,
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
                    in_process_cpu_frames: 1,
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
    fn preview_decode_performance_report_classifies_hardware_transfer_bound_frame() {
        let diagnostics = AppUiPreviewDiagnostics {
            decode_successes: 1,
            decode_playback_cursor_frames: 1,
            decode_playback_session_ring_hit_frames: 1,
            decode_total_duration_us: 90_000,
            decode_max_duration_us: 90_000,
            decode_last_duration_us: 90_000,
            decode_stage_durations: PreviewDecodeStageDurations {
                hardware_transfer_us: 70_000,
                swscale_us: 8_000,
                rgba_copy_us: 2_000,
                ..PreviewDecodeStageDurations::default()
            },
            decode_max_frame_stage_durations: PreviewDecodeStageDurations {
                hardware_transfer_us: 70_000,
                swscale_us: 8_000,
                rgba_copy_us: 2_000,
                ..PreviewDecodeStageDurations::default()
            },
            decode_access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles {
                playback_cursor: AppUiPreviewDecodeAccessModeProfile {
                    frames: 1,
                    playback_session_ring_hit_frames: 1,
                    total_duration_us: 90_000,
                    max_duration_us: 90_000,
                    last_duration_us: 90_000,
                    hardware_decode_cpu_transfer_observed_frames: 1,
                    stage_durations: PreviewDecodeStageDurations {
                        hardware_transfer_us: 70_000,
                        swscale_us: 8_000,
                        rgba_copy_us: 2_000,
                        ..PreviewDecodeStageDurations::default()
                    },
                    max_frame_stage_durations: PreviewDecodeStageDurations {
                        hardware_transfer_us: 70_000,
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
            "preview-decode-hw-transfer-test",
            50_000,
        );

        let summary = report.summary.expect("decode summary");
        assert_eq!(
            summary.primary_bottleneck,
            AppUiPreviewDecodeBottleneck::HardwareTransfer
        );
        assert!(report.root_causes.iter().any(|root| {
            root.code == "preview_decode_hardware_transfer_bound"
                && root.evidence.contains("hardware_transfer_us=70000")
                && root.evidence.contains("hardware_decode_cpu_transfer_observed_frames=1")
        }));
        assert!(report
            .actions
            .iter()
            .any(|action| action.code == "connect_native_decoder_surface_import"));
    }

    #[test]
    fn preview_decode_performance_report_flags_hardware_cpu_transfer_setup_failures() {
        let diagnostics = AppUiPreviewDiagnostics {
            decode_successes: 2,
            decode_playback_cursor_frames: 2,
            decode_access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles {
                playback_cursor: AppUiPreviewDecodeAccessModeProfile {
                    frames: 2,
                    hardware_decode_prefer_hardware_requested_frames: 2,
                    hardware_decode_device_context_attempted_frames: 2,
                    hardware_decode_device_context_created_frames: 1,
                    hardware_decode_backend_unavailable_frames: 2,
                    hardware_decode_cpu_transfer_setup_failed_frames: 1,
                    hardware_decode_cpu_transfer_decoder_open_failed_frames: 1,
                    hardware_decode_cpu_transfer_configured_frames: 1,
                    hardware_decode_cpu_transfer_observed_frames: 0,
                    ..AppUiPreviewDecodeAccessModeProfile::default()
                },
                ..AppUiPreviewDecodeAccessModeProfiles::default()
            },
            ..AppUiPreviewDiagnostics::default()
        };

        let report = build_preview_decode_performance_report(
            diagnostics.decode_performance_summary(50_000),
            "preview-decode-hw-transfer-setup-failure-test",
            50_000,
        );

        assert!(report.root_causes.iter().any(|root| {
            root.code == "preview_decode_hardware_cpu_transfer_setup_failed"
                && root.area == AppUiPreviewDecodePerformanceArea::CodecDecode
                && root.evidence.contains("setup_failed_frames=1")
                && root.evidence.contains("decoder_open_failed_frames=1")
                && root.evidence.contains("device_context_created_frames=1")
        }));
        assert!(report
            .actions
            .iter()
            .any(|action| action.code == "diagnose_ffmpeg_hardware_decode_setup"));
    }

    #[test]
    fn preview_decode_performance_report_flags_unengaged_playback_hardware_fallback() {
        let diagnostics = AppUiPreviewDiagnostics {
            decode_successes: 3,
            decode_playback_cursor_frames: 3,
            decode_access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles {
                playback_cursor: AppUiPreviewDecodeAccessModeProfile {
                    frames: 3,
                    hardware_decode_prefer_hardware_requested_frames: 3,
                    hardware_decode_backend_unavailable_frames: 1,
                    hardware_decode_codec_unsupported_frames: 1,
                    hardware_decode_device_context_attempted_frames: 1,
                    hardware_decode_device_context_unavailable_frames: 1,
                    hardware_decode_cpu_transfer_setup_failed_frames: 1,
                    hardware_decode_cpu_transfer_observed_frames: 0,
                    hardware_decode_gpu_resident_native_frames: 0,
                    hardware_decode_candidate_d3d12va_frames: 1,
                    hardware_decode_candidate_d3d11va_frames: 1,
                    ..AppUiPreviewDecodeAccessModeProfile::default()
                },
                ..AppUiPreviewDecodeAccessModeProfiles::default()
            },
            ..AppUiPreviewDiagnostics::default()
        };

        let report = build_preview_decode_performance_report(
            diagnostics.decode_performance_summary(50_000),
            "preview-decode-hw-fallback-not-engaged-test",
            50_000,
        );

        assert_eq!(report.verdict, AppUiPreviewDecodePerformanceVerdict::Warn);
        assert!(report.checks.iter().any(|check| {
            check.code == "preview_decode_playback_hardware_fallback_not_engaged"
                && check.severity == AppUiPreviewDecodePerformanceSeverity::Warn
                && check.observed == 3
                && check.limit == Some(0)
        }));
        assert!(report.root_causes.iter().any(|root| {
            root.code == "preview_decode_playback_hardware_fallback_not_engaged"
                && root.area == AppUiPreviewDecodePerformanceArea::CodecDecode
                && root.evidence.contains("requested_frames=3")
                && root.evidence.contains("effective_frames=0")
                && root.evidence.contains("not_engaged_frames=3")
                && root.evidence.contains("backend_unavailable_frames=1")
                && root.evidence.contains("codec_unsupported_frames=1")
                && root.evidence.contains("device_context_unavailable_frames=1")
                && root.evidence.contains("candidate_d3d12va_frames=1")
                && root.evidence.contains("candidate_d3d11va_frames=1")
        }));
        assert!(report
            .actions
            .iter()
            .any(|action| action.code == "recover_playback_hardware_decode_fallback"));
    }

    #[test]
    fn preview_decode_performance_report_accepts_engaged_playback_hardware_fallback() {
        let diagnostics = AppUiPreviewDiagnostics {
            decode_successes: 2,
            decode_playback_cursor_frames: 2,
            decode_access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles {
                playback_cursor: AppUiPreviewDecodeAccessModeProfile {
                    frames: 2,
                    hardware_decode_prefer_hardware_requested_frames: 2,
                    hardware_decode_cpu_transfer_observed_frames: 1,
                    hardware_decode_gpu_resident_native_frames: 1,
                    ..AppUiPreviewDecodeAccessModeProfile::default()
                },
                ..AppUiPreviewDecodeAccessModeProfiles::default()
            },
            ..AppUiPreviewDiagnostics::default()
        };

        let report = build_preview_decode_performance_report(
            diagnostics.decode_performance_summary(50_000),
            "preview-decode-hw-fallback-engaged-test",
            50_000,
        );

        assert!(report.checks.iter().any(|check| {
            check.code == "preview_decode_playback_hardware_fallback_not_engaged"
                && check.severity == AppUiPreviewDecodePerformanceSeverity::Pass
                && check.observed == 0
        }));
        assert!(!report
            .root_causes
            .iter()
            .any(|root| root.code == "preview_decode_playback_hardware_fallback_not_engaged"));
    }

    #[test]
    fn preview_decode_performance_report_flags_hardware_fallback_recovery_decisions() {
        let diagnostics = AppUiPreviewDiagnostics {
            decode_successes: 2,
            decode_playback_cursor_frames: 2,
            playback_schedule: AppUiPreviewPlaybackScheduleDiagnostics {
                current_hardware_fallback_not_engaged_decisions: 2,
                current_proxy_or_hardware_recommended_decisions: 2,
                current_drop_late_decisions: 1,
                current_proxy_generation_requests: 1,
                current_proxy_generation_request_dedupes: 1,
                ..AppUiPreviewPlaybackScheduleDiagnostics::default()
            },
            decode_access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles {
                playback_cursor: AppUiPreviewDecodeAccessModeProfile {
                    frames: 2,
                    hardware_decode_prefer_hardware_requested_frames: 2,
                    hardware_decode_backend_unavailable_frames: 1,
                    hardware_decode_codec_unsupported_frames: 1,
                    hardware_decode_cpu_transfer_observed_frames: 0,
                    hardware_decode_gpu_resident_native_frames: 0,
                    ..AppUiPreviewDecodeAccessModeProfile::default()
                },
                ..AppUiPreviewDecodeAccessModeProfiles::default()
            },
            ..AppUiPreviewDiagnostics::default()
        };

        let report = build_preview_decode_performance_report(
            diagnostics.decode_performance_summary(50_000),
            "preview-decode-hw-fallback-recovery-decisions-test",
            50_000,
        );

        assert_eq!(report.verdict, AppUiPreviewDecodePerformanceVerdict::Warn);
        assert!(report.checks.iter().any(|check| {
            check.code == "preview_decode_playback_hardware_fallback_not_engaged_decisions"
                && check.severity == AppUiPreviewDecodePerformanceSeverity::Warn
                && check.observed == 2
                && check.limit == Some(0)
        }));
        assert!(report.root_causes.iter().any(|root| {
            root.code == "preview_decode_playback_hardware_fallback_recovery_decisions"
                && root.area == AppUiPreviewDecodePerformanceArea::Scheduling
                && root.evidence.contains("current_hardware_fallback_not_engaged_decisions=2")
                && root.evidence.contains("current_proxy_generation_requests=1")
                && root.evidence.contains("current_proxy_generation_request_dedupes=1")
                && root.evidence.contains("playback_requested_frames=2")
                && root.evidence.contains("playback_effective_hardware_frames=0")
                && root.evidence.contains("playback_backend_unavailable_frames=1")
                && root.evidence.contains("playback_codec_unsupported_frames=1")
        }));
        assert!(report
            .actions
            .iter()
            .any(|action| action.code == "recover_playback_hardware_decode_fallback"));
    }

    #[test]
    fn preview_decode_performance_report_classifies_queue_wait_bound_frame() {
        let diagnostics = AppUiPreviewDiagnostics {
            decode_successes: 1,
            decode_in_process_cpu_frames: 1,
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
                in_flight_jobs: 2,
                in_flight_completed_jobs: 0,
                in_flight_cancellation_requested_jobs: 0,
                in_flight_max_age_us: 0,
                in_flight_cancellation_max_age_us: 0,
                queued_current_jobs: 2,
                in_flight_current_jobs: 1,
                queued_prefetch_jobs: 1,
                in_flight_prefetch_jobs: 1,
                queued_playback_cursor_jobs: 1,
                in_flight_playback_cursor_jobs: 1,
                queued_expired_playback_current_jobs: 1,
                queued_expired_jobs: 1,
                dropped_expired_playback_current_jobs: 0,
                dropped_expired_jobs: 0,
                queued_scrub_cursor_jobs: 1,
                in_flight_scrub_cursor_jobs: 1,
                queued_random_access_still_jobs: 1,
                in_flight_random_access_still_jobs: 0,
                in_flight_any_lane_jobs: 0,
                in_flight_playback_lane_jobs: 1,
                in_flight_scrub_lane_jobs: 1,
                in_flight_still_lane_jobs: 0,
                in_flight_non_playback_lane_jobs: 0,
                in_flight_cross_lane_current_jobs: 0,
                queued_any_lane_eligible_jobs: 3,
                queued_playback_lane_eligible_jobs: 1,
                queued_scrub_lane_eligible_jobs: 1,
                queued_still_lane_eligible_jobs: 1,
                queued_non_playback_lane_eligible_jobs: 2,
                closed: false,
            },
            decode_access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles {
                scrub_cursor: AppUiPreviewDecodeAccessModeProfile {
                    frames: 1,
                    in_process_cpu_frames: 1,
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
            summary.worker_queue.queued_non_playback_lane_eligible_jobs,
            2
        );
        assert_eq!(summary.worker_queue.in_flight_jobs, 2);
        assert_eq!(summary.worker_queue.in_flight_current_jobs, 1);
        assert_eq!(summary.worker_queue.in_flight_prefetch_jobs, 1);
        assert_eq!(summary.worker_queue.in_flight_playback_cursor_jobs, 1);
        assert_eq!(summary.worker_queue.in_flight_scrub_cursor_jobs, 1);
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
            && root.evidence.contains("queued_non_playback_lane_eligible_jobs=2")
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
            decode_in_process_cpu_frames: 1,
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
                in_flight_jobs: 1,
                in_flight_playback_cursor_jobs: 1,
                queued_expired_playback_current_jobs: 2,
                dropped_expired_playback_current_jobs: 1,
                queued_any_lane_eligible_jobs: 3,
                queued_playback_lane_eligible_jobs: 2,
                queued_non_playback_lane_eligible_jobs: 2,
                closed: false,
                ..MediaPreviewJobQueueDiagnostics::default()
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
    fn preview_decode_performance_report_flags_playback_current_stall_expiration() {
        let diagnostics = AppUiPreviewDiagnostics {
            playback_current_stalled_expirations: 1,
            queue_canceled_jobs: 1,
            scheduler: MediaPreviewSchedulerDiagnostics {
                canceled_requests: 1,
                ..MediaPreviewSchedulerDiagnostics::default()
            },
            worker_queue: MediaPreviewJobQueueDiagnostics {
                in_flight_jobs: 1,
                in_flight_current_jobs: 1,
                ..MediaPreviewJobQueueDiagnostics::default()
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
            check.code == "preview_decode_playback_current_stall_expirations"
                && check.severity == AppUiPreviewDecodePerformanceSeverity::Warn
                && check.observed == 1
                && check.limit == Some(0)
        }));
        assert!(report.root_causes.iter().any(|root| {
            root.code == "preview_decode_playback_current_stall_expirations"
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
                in_flight_prefetch_jobs: 1,
                ..MediaPreviewJobQueueDiagnostics::default()
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
    fn preview_decode_performance_report_flags_native_import_unavailable_playback_frames() {
        let diagnostics = AppUiPreviewDiagnostics {
            decode_successes: 1,
            decode_playback_cursor_frames: 1,
            decode_access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles {
                playback_cursor: AppUiPreviewDecodeAccessModeProfile {
                    frames: 1,
                    hardware_decode_prefer_gpu_requested_frames: 1,
                    ..AppUiPreviewDecodeAccessModeProfile::default()
                },
                ..AppUiPreviewDecodeAccessModeProfiles::default()
            },
            playback_schedule: AppUiPreviewPlaybackScheduleDiagnostics {
                current_native_import_unavailable_decisions: 1,
                current_proxy_or_hardware_recommended_decisions: 1,
                ..AppUiPreviewPlaybackScheduleDiagnostics::default()
            },
            ..AppUiPreviewDiagnostics::default()
        };

        let report = build_preview_decode_performance_report(
            diagnostics.decode_performance_summary(50_000),
            "preview-decode-native-import-unavailable-test",
            50_000,
        );

        assert_eq!(report.verdict, AppUiPreviewDecodePerformanceVerdict::Warn);
        assert!(report.checks.iter().any(|check| {
            check.code == "preview_decode_native_import_unavailable_playback_frames"
                && check.severity == AppUiPreviewDecodePerformanceSeverity::Warn
                && check.observed == 1
                && check.limit == Some(0)
        }));
        assert!(report.root_causes.iter().any(|root| {
            root.code == "preview_decode_native_import_unavailable_playback_frames"
                && root.severity == AppUiPreviewDecodePerformanceSeverity::Warn
                && root.evidence.contains("current_native_import_unavailable_decisions=1")
                && root.evidence.contains("playback_gpu_resident_native_frames=0")
        }));
        assert!(report
            .actions
            .iter()
            .any(|action| action.code == "enable_renderer_native_video_import"));
    }

    #[test]
    fn preview_decode_performance_report_flags_hardware_decode_admission_gate() {
        let diagnostics = AppUiPreviewDiagnostics {
            decode_successes: 1,
            decode_playback_cursor_frames: 1,
            playback_schedule: AppUiPreviewPlaybackScheduleDiagnostics {
                current_decode_decisions: 1,
                ..AppUiPreviewPlaybackScheduleDiagnostics::default()
            },
            hardware_decode_admission: AppUiPreviewHardwareDecodeAdmissionDiagnostics {
                playback_request: PreviewHardwareDecodeRequest::PreferHardwareDecode,
                renderer_native_import_support_known: true,
                renderer_native_import_ready: false,
                platform_native_import_ready: true,
                native_import_admission_ready: false,
                admission_blocker: Some(
                    AppUiPreviewHardwareDecodeAdmissionBlocker::RendererImportUnavailable,
                ),
                platform_discovery_available: true,
                platform_zero_copy_supported: false,
                platform_low_copy_fallback_supported: true,
                renderer_supported_handle_kinds: 0,
                renderer_supported_source_texture_formats: 0,
            },
            ..AppUiPreviewDiagnostics::default()
        };

        let report = build_preview_decode_performance_report(
            diagnostics.decode_performance_summary(50_000),
            "preview-decode-hardware-admission-gate-test",
            50_000,
        );

        assert_eq!(report.verdict, AppUiPreviewDecodePerformanceVerdict::Warn);
        assert!(report.checks.iter().any(|check| {
            check.code == "preview_decode_hardware_decode_admission_gated"
                && check.severity == AppUiPreviewDecodePerformanceSeverity::Warn
                && check.observed == 1
                && check.limit == Some(0)
        }));
        assert!(report.root_causes.iter().any(|root| {
            root.code == "preview_decode_hardware_decode_admission_gated"
                && root.severity == AppUiPreviewDecodePerformanceSeverity::Warn
                && root.evidence.contains("playback_hardware_decode_request=PreferHardwareDecode")
                && root.evidence.contains("admission_blocker=Some(RendererImportUnavailable)")
                && root.evidence.contains("renderer_native_import_support_known=true")
                && root.evidence.contains("renderer_native_import_ready=false")
                && root.evidence.contains("renderer_supported_handle_kinds=0")
                && root.evidence.contains("renderer_supported_source_texture_formats=0")
                && root.evidence.contains("platform_discovery_available=true")
                && root.evidence.contains("platform_zero_copy_supported=false")
                && root.evidence.contains("platform_low_copy_fallback_supported=true")
                && root.evidence.contains("platform_native_import_ready=true")
                && root.evidence.contains("native_import_admission_ready=false")
        }));
        assert!(report.actions.iter().any(|action| {
            action.code == "connect_native_import_before_enabling_hardware_decode_admission"
        }));
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
            decode_in_process_cpu_frames: 20,
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
                    in_process_cpu_frames: 20,
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
            decode_in_process_cpu_frames: 1,
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
            decode_in_process_cpu_frames: 1,
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
    fn preview_decode_performance_report_fails_broker_clock_regression() {
        let diagnostics = AppUiPreviewDiagnostics {
            decode_successes: 1,
            scheduler: MediaPreviewSchedulerDiagnostics {
                clock_regressions: 1,
                ..MediaPreviewSchedulerDiagnostics::default()
            },
            ..AppUiPreviewDiagnostics::default()
        };

        let report = build_preview_decode_performance_report(
            diagnostics.decode_performance_summary(50_000),
            "preview-decode-clock-regression-test",
            50_000,
        );

        assert_eq!(report.verdict, AppUiPreviewDecodePerformanceVerdict::Fail);
        assert!(report.checks.iter().any(|check| {
            check.code == "preview_decode_broker_clock_regressions"
                && check.severity == AppUiPreviewDecodePerformanceSeverity::Fail
                && check.observed == 1
        }));
        assert!(report.root_causes.iter().any(|root| {
            root.code == "preview_decode_broker_clock_regression"
                && root.evidence.contains("clock_regressions=1")
        }));
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
    fn preview_decode_performance_report_accepts_long_work_before_timely_cancellation() {
        let diagnostics = AppUiPreviewDiagnostics {
            decode_cancellation: cancellation_evidence(
                mondrian_playback::FrameWorkClass::Interactive,
                mondrian_playback::FrameCancellationCause::Superseded,
                85_000,
                Some(80_000),
                Some(1_000),
            ),
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

        assert!(report.checks.iter().any(|check| {
            check.code == "preview_decode_cancellation_gate"
                && check.severity == AppUiPreviewDecodePerformanceSeverity::Pass
        }));
        assert!(!report
            .root_causes
            .iter()
            .any(|root| root.code == "preview_decode_cancellation_gate_failed"));
        assert!(!report
            .actions
            .iter()
            .any(|action| action.code == "inspect_preview_decode_cancellation_points"));
    }

    #[test]
    fn preview_decode_performance_report_flags_slow_cancel_return_latency() {
        let diagnostics = AppUiPreviewDiagnostics {
            decode_cancellation: cancellation_evidence(
                mondrian_playback::FrameWorkClass::Interactive,
                mondrian_playback::FrameCancellationCause::Superseded,
                90_000,
                Some(20_000),
                Some(1_000),
            ),
            decode_canceled_jobs: 1,
            decode_canceled_obsolete_jobs: 1,
            decode_canceled_scrub_cursor_jobs: 1,
            decode_canceled_total_duration_us: 90_000,
            decode_canceled_max_duration_us: 90_000,
            decode_canceled_last_duration_us: 90_000,
            decode_canceled_return_latency_total_us: 70_000,
            decode_canceled_return_latency_max_us: 70_000,
            decode_canceled_return_latency_last_us: 70_000,
            decode_access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles {
                scrub_cursor: AppUiPreviewDecodeAccessModeProfile {
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

        assert_eq!(report.verdict, AppUiPreviewDecodePerformanceVerdict::Fail);
        assert!(report.checks.iter().any(|check| {
            check.code == "preview_decode_interactive_cancel_return_latency_max_us"
                && check.severity == AppUiPreviewDecodePerformanceSeverity::Fail
                && check.observed == 70_000
                && check.limit == Some(50_000)
        }));
        assert!(report.root_causes.iter().any(|root| {
            root.code == "preview_decode_cancellation_gate_failed"
                && root.evidence.contains("CheckpointToReturnExceeded")
        }));
        assert!(report
            .actions
            .iter()
            .any(|action| action.code == "inspect_preview_decode_cancellation_points"));
    }

    #[test]
    fn preview_decode_performance_report_flags_slow_cancel_observation_latency() {
        let diagnostics = AppUiPreviewDiagnostics {
            decode_cancellation: cancellation_evidence(
                mondrian_playback::FrameWorkClass::Interactive,
                mondrian_playback::FrameCancellationCause::Superseded,
                9_000,
                Some(8_000),
                Some(8_000),
            ),
            decode_canceled_jobs: 1,
            decode_canceled_obsolete_jobs: 1,
            decode_canceled_scrub_cursor_jobs: 1,
            decode_canceled_total_duration_us: 9_000,
            decode_canceled_max_duration_us: 9_000,
            decode_canceled_last_duration_us: 9_000,
            decode_cancel_observation_samples: 1,
            decode_cancel_observation_total_us: 8_000,
            decode_cancel_observation_max_us: 8_000,
            decode_cancel_observation_last_us: 8_000,
            decode_access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles {
                scrub_cursor: AppUiPreviewDecodeAccessModeProfile {
                    canceled_jobs: 1,
                    canceled_obsolete_jobs: 1,
                    canceled_total_duration_us: 9_000,
                    canceled_max_duration_us: 9_000,
                    canceled_last_duration_us: 9_000,
                    cancel_observation_samples: 1,
                    cancel_observation_total_us: 8_000,
                    cancel_observation_max_us: 8_000,
                    cancel_observation_last_us: 8_000,
                    ..AppUiPreviewDecodeAccessModeProfile::default()
                },
                ..AppUiPreviewDecodeAccessModeProfiles::default()
            },
            ..AppUiPreviewDiagnostics::default()
        };

        let report = build_preview_decode_performance_report(
            diagnostics.decode_performance_summary(50_000),
            "preview-decode-slow-cancel-observation-test",
            50_000,
        );

        assert_eq!(report.verdict, AppUiPreviewDecodePerformanceVerdict::Fail);
        assert!(report.checks.iter().any(|check| {
            check.code == "preview_decode_cancel_observation_max_us"
                && check.severity == AppUiPreviewDecodePerformanceSeverity::Fail
                && check.observed == 8_000
                && check.limit
                    == Some(
                        mondrian_playback::FrameCancellationPolicy::default()
                            .max_request_to_checkpoint
                            .as_micros() as u64,
                    )
        }));
        assert!(report.root_causes.iter().any(|root| {
            root.code == "preview_decode_cancellation_gate_failed"
                && root.evidence.contains("RequestToCheckpointExceeded")
        }));
        assert!(report
            .actions
            .iter()
            .any(|action| action.code == "inspect_preview_decode_cancellation_points"));
    }

    #[test]
    fn preview_decode_performance_report_classifies_prefetch_deadline_cancellations() {
        let diagnostics = AppUiPreviewDiagnostics {
            decode_successes: 1,
            decode_canceled_jobs: 3,
            decode_canceled_prefetch_deadline_jobs: 2,
            decode_canceled_prefetch_preempted_jobs: 1,
            decode_canceled_playback_cursor_jobs: 3,
            decode_in_process_cpu_frames: 1,
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
                    in_process_cpu_frames: 1,
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
            decode_in_process_cpu_frames: 1,
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
                    in_process_cpu_frames: 1,
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
            decode_cancellation: cancellation_evidence(
                mondrian_playback::FrameWorkClass::Still,
                mondrian_playback::FrameCancellationCause::Unknown,
                1_000,
                None,
                None,
            ),
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

        assert_eq!(report.verdict, AppUiPreviewDecodePerformanceVerdict::Fail);
        assert!(report.root_causes.iter().any(|root| {
            root.code == "preview_decode_cancellation_gate_failed"
                && root.evidence.contains("UnknownCause")
        }));
    }

    #[test]
    fn preview_decode_performance_report_flags_playback_without_locality() {
        let diagnostics = AppUiPreviewDiagnostics {
            decode_successes: 2,
            decode_in_process_cpu_frames: 2,
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
                    in_process_cpu_frames: 2,
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
            decode_in_process_cpu_frames: 2,
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
            decode_in_process_cpu_frames: 1,
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
                    in_process_cpu_frames: 1,
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
        service
            .record_input_color_resolution(InputColorResolutionSource::MissingPolicyAssumeRec709);
        service.record_input_color_resolution(InputColorResolutionSource::MissingPolicyRejectMedia);
        service.record_input_color_resolution(InputColorResolutionSource::DetectedMetadata);

        let diagnostics = service.diagnostics();
        assert_eq!(diagnostics.input_color_resolution_override, 1);
        assert_eq!(diagnostics.input_color_resolution_data_texture, 1);
        assert_eq!(diagnostics.input_color_resolution_detected_metadata, 2);
        assert_eq!(diagnostics.input_color_resolution_missing_assume_rec709, 2);
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
        assert_eq!(
            preview.blocked_color_domain_composites,
            export.blocked_color_domain_composites
        );
        assert_eq!(preview.domain_blockers, export.domain_blockers);
        assert_eq!(preview.fully_float_linear, export.fully_float_linear);
        assert_eq!(preview.gpu_path_ready, export.gpu_path_ready);
    }

    #[test]
    fn unresolved_effect_domain_is_a_distinct_fail_closed_preview_failure() {
        let diagnostics = AppUiPreviewDiagnostics {
            color_composite_plans: 1,
            color_composite_elements: 1,
            color_composite_blocked_domains: 1,
            color_composite_blocked_media_effect_domain: 1,
            ..AppUiPreviewDiagnostics::default()
        };

        let composite = diagnostics.composite_color_path_summary();
        assert_eq!(composite.path, TimelineCompositeColorPath::Blocked);
        assert_eq!(composite.legacy_rgba8_composites, 0);
        assert_eq!(composite.blocked_composites, 1);
        assert_eq!(composite.domain_blockers.media_effect, 1);

        let summary = diagnostics.color_health_summary().expect("preview color health");
        let report = summary.health_report("effect-domain-blocker");
        assert_eq!(report.verdict, AppUiPreviewColorHealthVerdict::Fail);
        assert!(report.checks.iter().any(|check| {
            check.code == color_report_vocab::check::EFFECT_DOMAIN_BLOCKERS
                && check.severity == AppUiPreviewColorHealthSeverity::Fail
                && check.observed == 1
        }));
        assert!(report
            .root_causes
            .iter()
            .any(|root| { root.code == color_report_vocab::root_cause::EFFECT_DOMAIN_UNRESOLVED }));
        assert!(report.actions.iter().any(|action| {
            action.code == color_report_vocab::action::RESOLVE_EFFECT_DOMAIN_TRANSITIONS
        }));
        assert!(!report.root_causes.iter().any(|root| {
            root.code == color_report_vocab::root_cause::LEGACY_RGBA8_COMPOSITE_PATH
        }));
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
    fn playback_quality_scale_multiplies_user_preview_resolution() {
        let sequence = Sequence::new("runtime scale");

        assert_eq!(
            preview_dimensions_for_sequence_at_runtime_scale(
                &sequence,
                mondrian_playback::PreviewResolutionScale::Full,
            ),
            (960, 540)
        );
        assert_eq!(
            preview_dimensions_for_sequence_at_runtime_scale(
                &sequence,
                mondrian_playback::PreviewResolutionScale::Half,
            ),
            (480, 270)
        );
        assert_eq!(
            preview_dimensions_for_sequence_at_runtime_scale(
                &sequence,
                mondrian_playback::PreviewResolutionScale::Quarter,
            ),
            (240, 135)
        );
    }

    #[test]
    fn nested_solid_color_sequence_returns_preview_frame() {
        let mut state = AppState::new();
        let mut child = Sequence::new("child");
        let child_id = child.id;
        let child_tb = child.time_base();
        child.video_tracks[0]
            .add_clip(
                Clip::new_solid_color(
                    AssetId::new(),
                    Color::from_rgba8(48, 120, 220, 255),
                    tt(0, child_tb),
                    tt(24, child_tb),
                )
                .expect("valid clip"),
            )
            .expect("child solid clip");

        let mut parent = Sequence::new("parent");
        let parent_tb = parent.time_base();
        parent.video_tracks[0]
            .add_clip(
                Clip::new_nested_sequence(
                    child_id,
                    tt(0, parent_tb),
                    tt(24, parent_tb),
                    Some("child".to_owned()),
                )
                .expect("valid clip"),
            )
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
            .add_clip(Clip::new(AssetId::new(), tt(0, tb), tt(24, tb)).expect("valid clip"))
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
            .add_clip(Clip::new(AssetId::new(), tt(0, tb), tt(24, tb)).expect("valid clip"))
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
            diagnostics.color_intermediate_transform_calls,
            diagnostics.color_output_transform_calls
        );
        let color_stage_passes = diagnostics
            .color_output_transform_calls
            .saturating_add(diagnostics.color_intermediate_transform_calls);
        assert_eq!(diagnostics.color_stage_total_stages, color_stage_passes);
        assert_eq!(
            diagnostics.color_stage_cpu_output_stages,
            color_stage_passes
        );
        assert_eq!(
            diagnostics.color_stage_pixels,
            color_stage_passes * 960_u64 * 540
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
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
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
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
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
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_graph,
            frame_seed: 12,
        }];
        let sequence_id = SequenceId::new();

        let mut sdr = test_color_context(ColorSpace::Rec709);
        sdr.display_management = mondrian_core::DisplayManagementPolicy {
            monitor_profile: mondrian_core::MonitorProfileReference::ColorSpace(ColorSpace::Rec709),
            viewer_mode: mondrian_core::ViewerDisplayMode::Sdr,
            tone_map_policy: mondrian_core::DisplayToneMapPolicy::Automatic,
        };
        let mut p3 = sdr.clone();
        p3.display_management = mondrian_core::DisplayManagementPolicy {
            monitor_profile: mondrian_core::MonitorProfileReference::ColorSpace(
                ColorSpace::DisplayP3,
            ),
            viewer_mode: mondrian_core::ViewerDisplayMode::Sdr,
            tone_map_policy: mondrian_core::DisplayToneMapPolicy::Automatic,
        };
        let first =
            viewer_preview_cache_key_for_resolved_plan(sequence_id, 320, 180, &resolved, &sdr);
        let second =
            viewer_preview_cache_key_for_resolved_plan(sequence_id, 320, 180, &resolved, &p3);

        assert_ne!(first, second);
    }

    #[test]
    fn resolved_media_preview_cache_key_includes_versioned_output_transform_intent() {
        let effect_graph = get_or_compile_scheduled_effect_graph(&EffectRenderPlan::default())
            .expect("default effect graph");
        let resolved = vec![ResolvedPreviewElement::Media {
            frame: test_media_frame_with_size(0, 2, 2, 100),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_graph,
            frame_seed: 12,
        }];
        let sequence_id = SequenceId::new();

        let current = test_color_context(ColorSpace::Rec709);
        let mut legacy = current.clone();
        legacy.engine = ColorEngine::MondrianStandard {
            package: mondrian_core::MondrianStandardPackageIdentity::V2,
        };
        legacy.output_transform = mondrian_core::OutputTransformIntent::mondrian_standard_package(
            mondrian_core::MondrianStandardPackageIdentity::V2,
        );
        let first =
            viewer_preview_cache_key_for_resolved_plan(sequence_id, 320, 180, &resolved, &current);
        let second =
            viewer_preview_cache_key_for_resolved_plan(sequence_id, 320, 180, &resolved, &legacy);

        assert_ne!(first, second);
    }

    #[test]
    fn resolved_media_preview_cache_key_includes_output_transform_intent() {
        let effect_graph = get_or_compile_scheduled_effect_graph(&EffectRenderPlan::default())
            .expect("default effect graph");
        let resolved = vec![ResolvedPreviewElement::Media {
            frame: test_media_frame_with_size(0, 2, 2, 100),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_graph,
            frame_seed: 12,
        }];
        let sequence_id = SequenceId::new();

        let mut standard = test_color_context(ColorSpace::Rec709);
        standard.tone_map = true;
        standard.output_transform = mondrian_core::OutputTransformIntent::mondrian_standard();
        let mut colorimetric = standard.clone();
        colorimetric.output_transform = mondrian_core::OutputTransformIntent::Colorimetric;
        let first =
            viewer_preview_cache_key_for_resolved_plan(sequence_id, 320, 180, &resolved, &standard);
        let second = viewer_preview_cache_key_for_resolved_plan(
            sequence_id,
            320,
            180,
            &resolved,
            &colorimetric,
        );

        assert_ne!(first, second);
    }

    #[test]
    fn resolved_media_preview_cache_key_includes_exact_standard_package() {
        let effect_graph = get_or_compile_scheduled_effect_graph(&EffectRenderPlan::default())
            .expect("default effect graph");
        let resolved = vec![ResolvedPreviewElement::Media {
            frame: test_media_frame_with_size(0, 2, 2, 100),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_graph,
            frame_seed: 12,
        }];
        let sequence_id = SequenceId::new();
        let current = test_color_context(ColorSpace::Rec709);
        let mut legacy = current.clone();
        let legacy_package = mondrian_core::MondrianStandardPackageIdentity::V2;
        legacy.engine = ColorEngine::MondrianStandard { package: legacy_package };
        legacy.output_transform =
            mondrian_core::OutputTransformIntent::mondrian_standard_package(legacy_package);

        let current_key =
            viewer_preview_cache_key_for_resolved_plan(sequence_id, 320, 180, &resolved, &current);
        let legacy_key =
            viewer_preview_cache_key_for_resolved_plan(sequence_id, 320, 180, &resolved, &legacy);

        assert_ne!(current_key, legacy_key);
    }

    #[test]
    fn preview_working_composite_boundary_uses_resolved_display_view() {
        let mut color_context = test_color_context(ColorSpace::Rec709);
        color_context.tone_map = true;
        color_context.output_transform = mondrian_core::OutputTransformIntent::mondrian_standard();
        let mut scratch = TimelineCompositeScratch::default();

        let _output = composite_resolved_preview_working(2, 2, &[], &color_context, &mut scratch)
            .expect("empty preview composite");

        let display_view = output_boundary_from_color_context(&color_context)
            .expect("encoded preview output")
            .display_view
            .expect("resolved display/view");
        assert_eq!(display_view.display, "Rec.1886 Rec.709 - Display");
        assert_eq!(display_view.view, "Mondrian Standard SDR v2");
    }

    #[test]
    fn preview_boundary_uses_colorimetric_intent_when_tone_map_is_disabled() {
        let mut color_context = test_color_context(ColorSpace::Rec709);
        color_context.tone_map = false;
        color_context.output_transform = mondrian_core::OutputTransformIntent::Colorimetric;

        let boundary =
            output_boundary_from_color_context(&color_context).expect("encoded preview output");

        assert_eq!(boundary.display_view, None);
        assert!(!boundary.tone_map);
    }

    #[test]
    fn preview_input_color_resolution_honors_override_metadata_and_missing_policy() {
        let mut color_context = test_color_context(ColorSpace::Rec709);
        color_context.working_color_space = WorkingColorSpace::LinearRec2020;
        color_context.missing_metadata_policy = MissingColorMetadataPolicy::AssumeRec709;

        assert_eq!(
            resolve_preview_input_color_space(
                Some(ColorSpace::SonySLog3SGamut3Cine),
                AssetMediaInterpretation::default(),
                Some(ColorSpace::Srgb),
                &color_context,
            )
            .resolved,
            ResolvedInputColor::Color(ColorSpace::SonySLog3SGamut3Cine)
        );
        assert_eq!(
            resolve_preview_input_color_space(
                Some(ColorSpace::SonySLog3SGamut3Cine),
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
                resolved: ResolvedInputColor::Color(ColorSpace::Srgb),
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
            .resolved,
            ResolvedInputColor::Color(ColorSpace::Rec709)
        );
        assert_eq!(
            resolve_preview_input_color_space(
                None,
                AssetMediaInterpretation::default(),
                None,
                &color_context,
            )
            .source,
            mondrian_timeline::sequence::InputColorResolutionSource::MissingPolicyAssumeRec709
        );

        let asset_override = resolve_preview_input_color_space(
            None,
            AssetMediaInterpretation {
                color: mondrian_core::timeline_data::MediaColorInterpretation::Override {
                    color_space: ColorSpace::AppleLogBt2020,
                },
                ..AssetMediaInterpretation::default()
            },
            Some(ColorSpace::Srgb),
            &color_context,
        );
        assert_eq!(
            asset_override.resolved,
            ResolvedInputColor::Color(ColorSpace::AppleLogBt2020)
        );
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
        assert_eq!(data.resolved, ResolvedInputColor::Data);
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
            .resolved,
            ResolvedInputColor::Rejected
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
        color_context.working_color_space = WorkingColorSpace::LinearRec2020;
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
            "source=MissingMetadata,method=MissingMetadata,warnings=missing_cicp".to_string();
        let issue_summary = VideoColorDiagnosticIssueSummary {
            source: mondrian_media::VideoColorSpaceSource::MissingMetadata,
            method: mondrian_media::VideoColorDetectionMethod::MissingMetadata,
            confidence: mondrian_media::VideoColorInterpretationConfidence::None,
            missing_cicp_tags: 1,
            unsupported_cicp_tags: 0,
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
                lower_priority_metadata_hints: 0,
                ignored_lower_priority_metadata_hints: 0,
                partial_cicp_tags: 0,
                missing_cicp_tags: 0,
                unsupported_cicp_tags: 0,
                decoder_unavailable: 0,
                hdr_side_data_count: 0,
                has_mastering_display_metadata: false,
                has_content_light_metadata: false,
                has_dynamic_hdr10_plus: false,
                has_dolby_vision_config: false,
                has_icc_profile: false,
                icc_cicp_mismatch: 0,
                icc_profile_unmapped: 0,
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
        assert_eq!(
            rejection.working_color_space,
            WorkingColorSpace::LinearRec2020
        );
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
                lower_priority_metadata_hints: 0,
                ignored_lower_priority_metadata_hints: 0,
                partial_cicp_tags: 0,
                missing_cicp_tags: 0,
                unsupported_cicp_tags: 0,
                decoder_unavailable: 0,
                hdr_side_data_count: 0,
                has_mastering_display_metadata: false,
                has_content_light_metadata: false,
                has_dynamic_hdr10_plus: false,
                has_dolby_vision_config: false,
                has_icc_profile: false,
                icc_cicp_mismatch: 0,
                icc_profile_unmapped: 0,
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
        sequence.settings.working_color_space = WorkingColorSpace::LinearRec2020;
        sequence.settings.color_management.missing_metadata_policy =
            MissingColorMetadataPolicy::AssumeRec709;
        let tb = sequence.time_base();
        let detected_id = AssetId::new();
        let override_id = AssetId::new();
        let missing_id = AssetId::new();
        let data_id = AssetId::new();

        sequence.video_tracks[0]
            .add_clip(Clip::new(detected_id, tt(0, tb), tt(10, tb)).expect("valid clip"))
            .expect("add detected clip");
        for (name, asset_id) in [
            ("override", override_id),
            ("missing", missing_id),
            ("data", data_id),
        ] {
            let mut track = Track::new_video(name);
            track
                .add_clip(Clip::new(asset_id, tt(0, tb), tt(10, tb)).expect("valid clip"))
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
                    color_space: ColorSpace::SonySLog3SGamut3Cine,
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
                mondrian_timeline::sequence::InputColorResolutionSource::MissingPolicyAssumeRec709
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
        parent.settings.working_color_space = WorkingColorSpace::LinearRec2020;
        parent.settings.color_management.missing_metadata_policy =
            MissingColorMetadataPolicy::AssumeRec709;
        let mut nested = Sequence::new("nested-color-resolution-parity");
        nested.settings.working_color_space = WorkingColorSpace::LinearRec2020;
        nested.settings.color_management.missing_metadata_policy =
            MissingColorMetadataPolicy::AssumeRec709;

        let parent_tb = parent.time_base();
        let nested_tb = nested.time_base();
        let parent_override_id = AssetId::new();
        let nested_detected_id = AssetId::new();
        let nested_data_id = AssetId::new();
        let nested_missing_id = AssetId::new();

        parent.video_tracks[0]
            .add_clip(
                Clip::new(parent_override_id, tt(0, parent_tb), tt(10, parent_tb))
                    .expect("valid clip"),
            )
            .expect("add parent media clip");
        let mut nested_track = Track::new_video("nested");
        nested_track
            .add_clip(
                Clip::new_nested_sequence(
                    nested.id,
                    tt(0, parent_tb),
                    tt(10, parent_tb),
                    Some("Nested".to_owned()),
                )
                .expect("valid clip"),
            )
            .expect("add nested sequence clip");
        parent.video_tracks.push(nested_track);

        nested.video_tracks[0]
            .add_clip(
                Clip::new(nested_detected_id, tt(0, nested_tb), tt(10, nested_tb))
                    .expect("valid clip"),
            )
            .expect("add nested detected clip");
        for (name, asset_id) in [("data", nested_data_id), ("missing", nested_missing_id)] {
            let mut track = Track::new_video(name);
            track
                .add_clip(
                    Clip::new(asset_id, tt(0, nested_tb), tt(10, nested_tb)).expect("valid clip"),
                )
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
                    color_space: ColorSpace::SonySLog3SGamut3Cine,
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
                mondrian_timeline::sequence::InputColorResolutionSource::MissingPolicyAssumeRec709
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
            .add_clip(
                Clip::new(direct_id, tt(0, parent_tb), tt(10, parent_tb)).expect("valid clip"),
            )
            .expect("add direct media clip");
        let mut nested_track = Track::new_video("nested");
        nested_track
            .add_clip(
                Clip::new_nested_sequence(
                    nested_id,
                    tt(0, parent_tb),
                    tt(10, parent_tb),
                    Some("Nested".to_owned()),
                )
                .expect("valid clip"),
            )
            .expect("add nested sequence clip");
        parent.video_tracks.push(nested_track);

        nested.video_tracks[0]
            .add_clip(
                Clip::new(nested_asset_id, tt(0, nested_tb), tt(10, nested_tb))
                    .expect("valid clip"),
            )
            .expect("add nested media clip");

        let mut asset_color_diagnostics = HashMap::new();
        asset_color_diagnostics.insert(
            direct_id,
            mondrian_media::VideoColorDiagnostic {
                detected_color_space: None,
                color_range: DecodedVideoRange::Unknown,
                interpretation: mondrian_media::DetectedColorInterpretation {
                    color_space: None,
                    confidence: mondrian_media::VideoColorInterpretationConfidence::None,
                    source: mondrian_media::VideoColorSpaceSource::MissingMetadata,
                    method: mondrian_media::VideoColorDetectionMethod::MissingMetadata,
                    evidence: Vec::new(),
                    warnings: vec![
                        mondrian_media::VideoColorInterpretationWarning::MissingCicpTags,
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
                color_range: DecodedVideoRange::Unknown,
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
                color_range: DecodedVideoRange::Limited,
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
            tt(0, tb),
            tt(10, tb),
        )
        .expect("valid clip");
        solid.transform.set_scale(glam::Vec2::new(0.75, 0.75));
        let mut blur: mondrian_effects::EffectNode = mondrian_effects::EffectNodeExt::with_defaults(
            mondrian_effects::EffectType::GaussianBlur,
        );
        blur.set_static_value_by_suffix(
            "radius",
            mondrian_core::automation::PropertyValue::Float(1.0),
        )
        .expect("set test blur radius");
        solid.add_effect_node(blur);
        solid.masks.push(mondrian_core::mask_data::MaskComponent::new(
            "float-path-mask".to_owned(),
            mondrian_core::mask_data::MaskKeyframe {
                shape: mondrian_core::mask_data::MaskShape::Rectangle {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                    corner_radius: 0.0,
                },
                opacity: 0.5,
                ..Default::default()
            },
        ));
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
        assert_eq!(preview_summary.legacy_breakdown.solid_effect, 0);
        assert_eq!(export_summary.legacy_breakdown.solid_effect, 0);
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
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
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
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_graph,
            frame_seed: 0,
        })];
        let mut export_scratch = TimelineCompositeScratch::default();
        let expected_frame = mondrian_renderer::composite_timeline_elements_color_frame(
            1,
            1,
            &export_elements,
            TimelineCompositeOptions::default(),
            TimelineEffectColorRuntime::new(
                &color_context.engine,
                color_context.working_color_space,
            ),
            &mut export_scratch,
        );
        assert_eq!(
            expected_frame.descriptor().color_space,
            color_context.working_color_space.into()
        );
        let export_boundary = RenderOutputColorBoundary::from_intent(
            mondrian_renderer::RenderOutputColorBoundaryTarget::Export,
            color_context.output_color_space.color().expect("encoded export output"),
            &color_context.output_transform,
            color_context.tone_map,
            color_context.engine.clone(),
        )
        .expect("resolved export intent");
        let export =
            mondrian_renderer::execute_cpu_output_boundary(&expected_frame, &export_boundary)
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
    fn preview_camera_log_input_matches_export_frame_hash() {
        const SLOG3_TO_STANDARD_V3_SDR_V2_GOLDEN_HASH: u64 = 2_504_953_508_210_442_961;

        let effect_graph = get_or_compile_scheduled_effect_graph(&EffectRenderPlan::default())
            .expect("default effect graph");
        let source = CpuEncodedColorFrame::source_rgba8(
            2,
            2,
            ColorSpace::SonySLog3SGamut3Cine,
            vec![
                96, 128, 160, 255, 192, 112, 64, 255, 24, 208, 144, 255, 224, 224, 224, 128,
            ],
        );
        let input_transform = RenderInputTransform::to_working(
            WorkingColorSpace::LinearRec2020,
            false,
            ColorEngine::mondrian_standard(),
        );
        let media = MediaPreviewFrame {
            width: 2,
            height: 2,
            logical_width: 2,
            logical_height: 2,
            frame: None,
            gpu_source: Some(MediaPreviewGpuSourceFrame::new(
                source.clone(),
                input_transform.clone(),
            )),
            native_source: None,
            signature: 3_003,
            presentation_quality: mondrian_playback::FramePresentationQuality::Ready,
            decode_execution: AppUiPreviewDecodeExecutionSummary::from_path(
                PreviewDecodeExecutionPath::SoftwareCpu,
            ),
        };
        let mut color_context = test_color_context(ColorSpace::Srgb);
        color_context.working_color_space = WorkingColorSpace::LinearRec2020;
        color_context.tone_map = true;
        color_context.output_transform = mondrian_core::OutputTransformIntent::mondrian_standard();
        let resolved = [ResolvedPreviewElement::Media {
            frame: media,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_graph: Arc::clone(&effect_graph),
            frame_seed: 3_003,
        }];
        let preview_service = AppUiPreviewService::new();
        let mut preview_scratch = TimelineCompositeScratch::default();
        let preview = composite_resolved_preview(
            &preview_service,
            2,
            2,
            &resolved,
            &color_context,
            &mut preview_scratch,
        )
        .expect("preview camera-log composite");

        let export_input = execute_cpu_input_stage(&source, &input_transform)
            .expect("export camera-log input transform");
        let export_elements = [TimelineCompositeElement::Media(TimelineMediaLayer {
            frame: &export_input.result.frame,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_graph,
            frame_seed: 3_003,
        })];
        let mut export_scratch = TimelineCompositeScratch::default();
        let export_working = mondrian_renderer::composite_timeline_elements_color_frame(
            2,
            2,
            &export_elements,
            TimelineCompositeOptions::default(),
            TimelineEffectColorRuntime::new(
                &color_context.engine,
                color_context.working_color_space,
            ),
            &mut export_scratch,
        );
        let export_boundary = RenderOutputColorBoundary::from_intent(
            mondrian_renderer::RenderOutputColorBoundaryTarget::Export,
            ColorSpace::Srgb,
            &color_context.output_transform,
            color_context.tone_map,
            color_context.engine.clone(),
        )
        .expect("resolved export Standard SDR intent");
        let export =
            mondrian_renderer::execute_cpu_output_boundary(&export_working, &export_boundary)
                .expect("export camera-log output transform")
                .result
                .frame
                .into_rgba();

        assert_eq!(preview.rgba, export);
        assert_eq!(
            preview_service.diagnostics().color_stage_cpu_input_stages,
            1
        );
        assert_eq!(preview.color_stage_diagnostics.cpu_output_stages, 1);
        assert_eq!(preview.composite_diagnostics.float_linear_composites, 1);
        assert_eq!(preview.composite_diagnostics.legacy_rgba8_composites, 0);
        let hash = stable_rgba_hash(&preview.rgba);
        assert_eq!(
            hash, SLOG3_TO_STANDARD_V3_SDR_V2_GOLDEN_HASH,
            "actual hash={hash}"
        );
    }

    #[test]
    fn preview_multilayer_color_output_matches_export_frame_hash() {
        const REC2020_TO_STANDARD_V3_SDR_V2_SRGB_MULTILAYER_GOLDEN_HASH: u64 =
            16_678_535_327_707_552_965;

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
                WorkingColorSpace::LinearRec2020,
                false,
                ColorEngine::mondrian_standard(),
            ),
        )
        .expect("media input transform")
        .result
        .frame;
        let media = MediaPreviewFrame {
            width: frame.descriptor().width,
            height: frame.descriptor().height,
            logical_width: frame.descriptor().width,
            logical_height: frame.descriptor().height,
            frame: Some(frame),
            gpu_source: None,
            native_source: None,
            signature: 2_020,
            presentation_quality: mondrian_playback::FramePresentationQuality::Ready,
            decode_execution: AppUiPreviewDecodeExecutionSummary::from_path(
                PreviewDecodeExecutionPath::SoftwareCpu,
            ),
        };
        let solid = TimelineSolidColorLayer {
            color: Color::from_rgba8(32, 180, 220, 255),
            opacity: 0.35,
            blend_mode: BlendMode::Screen,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_graph: Arc::clone(&effect_graph),
            frame_seed: 14,
        };
        let mut color_context = test_color_context(ColorSpace::Srgb);
        color_context.working_color_space = WorkingColorSpace::LinearRec2020;
        color_context.tone_map = true;
        color_context.output_transform = mondrian_core::OutputTransformIntent::mondrian_standard();

        let resolved = vec![
            ResolvedPreviewElement::Media {
                frame: media.clone(),
                opacity: 0.85,
                blend_mode: BlendMode::Multiply,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
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
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
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
                TimelineEffectColorRuntime::new(
                    &color_context.engine,
                    color_context.working_color_space,
                ),
                &mut export_scratch,
            );
        let export_boundary = RenderOutputColorBoundary::from_intent(
            mondrian_renderer::RenderOutputColorBoundaryTarget::Export,
            color_context.output_color_space.color().expect("encoded export output"),
            &color_context.output_transform,
            color_context.tone_map,
            color_context.engine.clone(),
        )
        .expect("resolved export intent");
        let export_output =
            mondrian_renderer::execute_cpu_output_boundary(&export_working.frame, &export_boundary)
                .expect("export multilayer color transform");
        let export = export_output.result.frame.clone().into_rgba();

        assert_eq!(preview.rgba, export);
        let preview_export_hash = stable_rgba_hash(&preview.rgba);
        assert_eq!(preview_export_hash, stable_rgba_hash(&export));
        assert_eq!(
            preview_export_hash,
            REC2020_TO_STANDARD_V3_SDR_V2_SRGB_MULTILAYER_GOLDEN_HASH
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
        assert_eq!(
            preview_health.rgba8_boundary_calls, 0,
            "float OCIO Program Output must quantize only after optional monitor adaptation"
        );

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
        let demand_identity = AppUiPreviewService::test_frame_demand_identity();
        service.seed_pending_playback_current_preview_work_for_test(demand_identity);

        let before = service.diagnostics();
        assert_eq!(before.scheduler.pending_requests, 1);
        assert_eq!(before.worker_queue.queued_jobs, 1);
        assert!(service.current_frame_pending.get());

        let outcome = service
            .expire_stalled_realtime_current_with_timeout(Duration::ZERO, Some(demand_identity));
        assert!(!outcome.visible_change);
        assert!(outcome.transport_change);
        assert_eq!(outcome.frame_deliveries.len(), 1);

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
    fn expired_work_cannot_publish_late_after_its_demand_completed() {
        let service = AppUiPreviewService::new_without_workers_for_test();
        let completed_identity = AppUiPreviewService::test_frame_demand_identity();
        service.seed_pending_playback_current_preview_work_for_test(completed_identity);
        let mut engine = mondrian_playback::PlaybackEngine::new(
            Rational::new(1, 25),
            mondrian_playback::PlaybackPolicy::default(),
        )
        .expect("playback engine");
        engine
            .play_timeline(
                mondrian_playback::PlaybackTimelineBinding::new(None, 0, Rational::new(1, 25), 10)
                    .expect("timeline binding"),
                mondrian_core::FramePosition::new(0, Rational::new(1, 25)),
                mondrian_playback::MonotonicTimestamp::ZERO,
            )
            .expect("priming demand");
        engine
            .complete_priming(
                mondrian_playback::ClockMaster::Synthetic,
                mondrian_playback::MonotonicTimestamp::ZERO,
            )
            .expect("replacement demand");
        let replacement_identity =
            engine.pending_frame_demand().expect("replacement pending demand").identity();
        assert_ne!(replacement_identity, completed_identity);

        let outcome = service.expire_stalled_realtime_current_with_timeout(
            Duration::ZERO,
            Some(replacement_identity),
        );

        assert!(
            outcome.frame_deliveries.is_empty(),
            "scheduler expiration must not revive an already-completed playback demand"
        );
        assert_eq!(service.diagnostics().scheduler.pending_requests, 0);
    }

    #[test]
    fn stalled_scrub_releases_capacity_without_reporting_playback_delivery() {
        let service = AppUiPreviewService::new_without_workers_for_test();
        service.seed_pending_preview_work_with_access_mode_for_test(
            PreviewDecodeAccessMode::ScrubCursor,
            None,
        );

        let outcome = service.expire_stalled_realtime_current_with_timeout(Duration::ZERO, None);

        assert!(!outcome.visible_change);
        assert!(outcome.transport_change);
        assert!(outcome.frame_deliveries.is_empty());
        let diagnostics = service.diagnostics();
        assert_eq!(diagnostics.scheduler.pending_requests, 0);
        assert_eq!(diagnostics.worker_queue.queued_jobs, 0);
        assert_eq!(diagnostics.playback_current_stalled_expirations, 0);
        assert_eq!(diagnostics.playback_schedule.current_drop_late_decisions, 0);
    }

    #[test]
    fn repeated_late_playback_current_frames_enter_pressure_recovery() {
        let service = AppUiPreviewService::new_without_workers_for_test();

        let demand_identity = AppUiPreviewService::test_frame_demand_identity();
        service.seed_pending_playback_current_preview_work_for_test(demand_identity);
        assert!(
            service
                .expire_stalled_realtime_current_with_timeout(
                    Duration::ZERO,
                    Some(demand_identity),
                )
                .transport_change
        );
        service.seed_pending_playback_current_preview_work_for_test(demand_identity);
        assert!(
            service
                .expire_stalled_realtime_current_with_timeout(
                    Duration::ZERO,
                    Some(demand_identity),
                )
                .transport_change
        );

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
    fn playback_pressure_skips_new_current_decode_when_realtime_work_is_pending() {
        let service = AppUiPreviewService::new_without_workers_for_test();
        let key = test_media_key(200);
        assert_eq!(
            service.jobs.enqueue(MediaPreviewJob {
                key,
                source_secs: 1.0,
                generation: service.current_generation.get(),
                priority: MediaPreviewRequestPriority::Current,
                access_mode: PreviewDecodeAccessMode::PlaybackCursor,
                adaptive_hints: PreviewDecodeAdaptiveHints::default(),
                hardware_decode_request: PreviewHardwareDecodeRequest::Auto,
                hardware_decode_device_selector: None,
                enqueued_at: Instant::now(),
                deadline_at: None,
                demand_identity: None,
                execution_id: None,
            }),
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        );
        service.record_playback_current_late_drop(
            MEDIA_PREVIEW_PLAYBACK_PRESSURE_LATE_STREAK_THRESHOLD,
        );

        assert!(!service.request_media_preview(
            test_media_key(201),
            1.0,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::PlaybackCursor,
            Some(Instant::now() + Duration::from_millis(33)),
            None,
            PreviewDecodeAdaptiveHints::default(),
        ));

        let diagnostics = service.diagnostics();
        assert_eq!(diagnostics.worker_queue.queued_jobs, 1);
        assert_eq!(
            diagnostics.playback_schedule.current_sustained_pressure_skips,
            1
        );
        assert_eq!(diagnostics.playback_schedule.current_decode_decisions, 0);
        assert_eq!(
            diagnostics.playback_schedule.current_proxy_or_hardware_recommended_decisions,
            MEDIA_PREVIEW_PLAYBACK_PRESSURE_LATE_STREAK_THRESHOLD + 1
        );
    }

    #[test]
    fn playback_pressure_allows_recovery_decode_when_no_realtime_work_is_pending() {
        let service = AppUiPreviewService::new_without_workers_for_test();
        service.record_playback_current_late_drop(
            MEDIA_PREVIEW_PLAYBACK_PRESSURE_LATE_STREAK_THRESHOLD,
        );

        assert!(service.request_media_preview(
            test_media_key(202),
            1.0,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::PlaybackCursor,
            Some(Instant::now() + Duration::from_millis(33)),
            None,
            PreviewDecodeAdaptiveHints::default(),
        ));

        let diagnostics = service.diagnostics();
        assert_eq!(
            diagnostics.playback_schedule.current_sustained_pressure_skips,
            0
        );
        assert_eq!(diagnostics.playback_schedule.current_decode_decisions, 1);
        assert_eq!(diagnostics.worker_queue.queued_playback_cursor_jobs, 1);
    }

    #[test]
    fn realtime_current_preempts_queued_still_work_before_queue_is_full() {
        let service = AppUiPreviewService::new_without_workers_for_test();
        let still_key = test_media_key(203);
        let scrub_key = test_media_key(204);

        assert!(service.request_media_preview(
            still_key.clone(),
            1.0,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::RandomAccessStillFrame,
            None,
            None,
            PreviewDecodeAdaptiveHints::default(),
        ));
        assert!(service.request_media_preview(
            scrub_key.clone(),
            1.0,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::ScrubCursor,
            Some(Instant::now() + Duration::from_millis(33)),
            None,
            PreviewDecodeAdaptiveHints::default(),
        ));

        let diagnostics = service.diagnostics();
        assert_eq!(diagnostics.scheduler.pending_requests, 1);
        assert_eq!(diagnostics.scheduler.evicted_still_requests, 1);
        assert_eq!(diagnostics.queue_canceled_jobs, 1);
        assert_eq!(diagnostics.worker_queue.queued_random_access_still_jobs, 0);
        assert_eq!(diagnostics.worker_queue.queued_scrub_cursor_jobs, 1);
        assert_eq!(diagnostics.worker_queue.queued_jobs, 1);
        assert!(!service.scheduler.has_pending_key(&still_key));
        assert!(service.scheduler.has_pending_key(&scrub_key));
    }

    #[test]
    fn realtime_current_keeps_in_flight_still_pending_for_structured_preemption() {
        let service = AppUiPreviewService::new_without_workers_for_test();
        let still_key = test_media_key(205);
        let scrub_key = test_media_key(206);
        let generation = service.current_generation.get();

        assert_eq!(
            service.scheduler.request(
                still_key.clone(),
                generation,
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::RandomAccessStillFrame,
            ),
            MediaPreviewRequestStatus::Scheduled { evicted_prefetch: None, evicted_still: None }
        );
        let still_execution = service
            .scheduler
            .begin_test_execution(MediaPreviewWorkerLane::Still)
            .expect("still work should be in flight before realtime admission");
        assert!(service.request_media_preview(
            scrub_key.clone(),
            1.0,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::ScrubCursor,
            Some(Instant::now() + Duration::from_millis(33)),
            None,
            PreviewDecodeAdaptiveHints::default(),
        ));

        let diagnostics = service.diagnostics();
        assert_eq!(diagnostics.scheduler.pending_requests, 2);
        assert_eq!(diagnostics.scheduler.evicted_still_requests, 0);
        assert_eq!(diagnostics.queue_canceled_jobs, 0);
        assert_eq!(diagnostics.worker_queue.queued_scrub_cursor_jobs, 1);
        assert_eq!(diagnostics.worker_queue.queued_random_access_still_jobs, 0);
        assert!(service.scheduler.has_pending_key(&still_key));
        assert!(service.scheduler.has_pending_key(&scrub_key));
        assert_eq!(
            media_preview_cancel_reason(
                service.scheduler.execution_cancellation(still_execution),
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::RandomAccessStillFrame,
                Duration::ZERO,
                false,
            ),
            Some(MediaPreviewCancelReason::StillPreemptedByRealtimeCurrent)
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
        assert_eq!(diagnostics.prefetch_skipped_current_work, 0);
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
                hardware_decode_request: PreviewHardwareDecodeRequest::Auto,
                hardware_decode_device_selector: None,
                enqueued_at: Instant::now(),
                deadline_at: None,
                demand_identity: None,
                execution_id: None,
            }),
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        );

        service.schedule_media_prefetches(&state, sequence, state.current_frame(), width, height);

        let diagnostics = service.diagnostics();
        assert_eq!(diagnostics.prefetch_skipped_current_pending, 0);
        assert_eq!(diagnostics.prefetch_skipped_current_work, 1);
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
        let generation = service.scheduler.begin_generation();
        let _current = begin_test_media_execution(
            &service,
            test_media_key(150),
            generation,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::ScrubCursor,
            MediaPreviewWorkerLane::Scrub,
        );

        service.schedule_media_prefetches(&state, sequence, state.current_frame(), width, height);

        let diagnostics = service.diagnostics();
        assert_eq!(diagnostics.prefetch_skipped_current_pending, 0);
        assert_eq!(diagnostics.prefetch_skipped_current_work, 1);
        assert_eq!(diagnostics.prefetch_skipped_prefetch_backlog, 0);
        assert_eq!(diagnostics.enqueued_jobs, 0);
        assert_eq!(diagnostics.worker_queue.queued_prefetch_jobs, 0);
        assert_eq!(diagnostics.worker_queue.in_flight_current_jobs, 1);
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
                    hardware_decode_request: PreviewHardwareDecodeRequest::Auto,
                    hardware_decode_device_selector: None,
                    enqueued_at: Instant::now(),
                    deadline_at: None,
                    demand_identity: None,
                    execution_id: None,
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
        assert_eq!(diagnostics.prefetch_skipped_current_work, 0);
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
        let generation = service.scheduler.begin_generation();
        let _executions = (0..prefetch_window)
            .map(|offset| {
                begin_test_media_execution(
                    &service,
                    test_media_key(250 + offset as i64),
                    generation,
                    MediaPreviewRequestPriority::Prefetch,
                    PreviewDecodeAccessMode::PlaybackCursor,
                    MediaPreviewWorkerLane::Playback,
                )
            })
            .collect::<Vec<_>>();

        service.schedule_media_prefetches(&state, sequence, state.current_frame(), width, height);

        let diagnostics = service.diagnostics();
        assert_eq!(diagnostics.prefetch_skipped_current_pending, 0);
        assert_eq!(diagnostics.prefetch_skipped_current_work, 0);
        assert_eq!(diagnostics.prefetch_skipped_prefetch_backlog, 1);
        assert_eq!(diagnostics.enqueued_jobs, 0);
        assert_eq!(diagnostics.worker_queue.queued_prefetch_jobs, 0);
        assert_eq!(
            diagnostics.worker_queue.in_flight_prefetch_jobs,
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
                hardware_decode_request: PreviewHardwareDecodeRequest::Auto,
                hardware_decode_device_selector: None,
                enqueued_at: Instant::now(),
                deadline_at: None,
                demand_identity: None,
                execution_id: None,
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
            diagnostics.enqueued_jobs,
            prefetch_window.saturating_sub(1) as u64
        );
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
        let generation = service.scheduler.begin_generation();
        service.current_generation.set(generation);
        let _prefetch = begin_test_media_execution(
            &service,
            test_media_key(350),
            generation,
            MediaPreviewRequestPriority::Prefetch,
            PreviewDecodeAccessMode::PlaybackCursor,
            MediaPreviewWorkerLane::Playback,
        );

        service.schedule_media_prefetches(&state, sequence, state.current_frame(), width, height);

        let diagnostics = service.diagnostics();
        assert_eq!(diagnostics.prefetch_skipped_prefetch_backlog, 0);
        assert_eq!(
            diagnostics.worker_queue.queued_prefetch_jobs,
            prefetch_window.saturating_sub(1)
        );
        assert_eq!(diagnostics.worker_queue.in_flight_prefetch_jobs, 1);
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
                hardware_decode_request: PreviewHardwareDecodeRequest::Auto,
                hardware_decode_device_selector: None,
                enqueued_at: Instant::now(),
                deadline_at: None,
                demand_identity: None,
                execution_id: None,
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
            diagnostics.enqueued_jobs,
            prefetch_window.saturating_sub(1) as u64,
            "prefetch must fill only the remaining job slots even when a future frame has multiple active tracks"
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
        };

        let result = decode_media_preview(
            MediaPreviewJob {
                key: key.clone(),
                source_secs: 0.5,
                generation: 7,
                priority: MediaPreviewRequestPriority::Current,
                access_mode: PreviewDecodeAccessMode::ScrubCursor,
                adaptive_hints: PreviewDecodeAdaptiveHints::default(),
                hardware_decode_request: PreviewHardwareDecodeRequest::Auto,
                hardware_decode_device_selector: None,
                enqueued_at: Instant::now(),
                deadline_at: None,
                demand_identity: None,
                execution_id: None,
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
        };

        let result = decode_media_preview(
            MediaPreviewJob {
                key: key.clone(),
                source_secs: 0.5,
                generation: 7,
                priority: MediaPreviewRequestPriority::Prefetch,
                access_mode: PreviewDecodeAccessMode::PlaybackCursor,
                adaptive_hints: PreviewDecodeAdaptiveHints::default(),
                hardware_decode_request: PreviewHardwareDecodeRequest::Auto,
                hardware_decode_device_selector: None,
                enqueued_at: Instant::now(),
                deadline_at: None,
                demand_identity: None,
                execution_id: None,
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
        let (result_tx, result_rx) = mpsc::channel();
        let scheduler = MediaPreviewScheduler::default();
        let (job_tx, job_rx) = scheduler.job_queue();
        let shutdown = Arc::new(PreviewShutdownSignal::default());
        let key = test_media_key(42);
        let generation = scheduler.begin_generation();
        assert_eq!(
            job_tx.enqueue(MediaPreviewJob {
                key: key.clone(),
                source_secs: 42.0,
                generation,
                priority: MediaPreviewRequestPriority::Current,
                access_mode: PreviewDecodeAccessMode::PlaybackCursor,
                adaptive_hints: PreviewDecodeAdaptiveHints::default(),
                hardware_decode_request: PreviewHardwareDecodeRequest::Auto,
                hardware_decode_device_selector: None,
                enqueued_at: Instant::now(),
                deadline_at: Some(Instant::now() - Duration::from_millis(1)),
                demand_identity: None,
                execution_id: None,
            }),
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
            .expect("expired playback current should produce a canceled result");

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
        worker.join().expect("preview worker should stop after queue close");
    }

    #[test]
    fn queued_expiry_completes_matching_demand_without_cancellation_latency_evidence() {
        let service = AppUiPreviewService::new_without_workers_for_test();
        let result_tx = install_preview_result_channel_for_test(&service);
        let key = test_media_key(76);
        let generation = service.scheduler.begin_generation();
        let demand_identity = AppUiPreviewService::test_frame_demand_identity();
        assert_eq!(
            service.scheduler.request_with_demand_identity(
                key.clone(),
                generation,
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::PlaybackCursor,
                Some(demand_identity),
            ),
            MediaPreviewRequestStatus::Scheduled { evicted_prefetch: None, evicted_still: None }
        );
        let mut result = media_preview_canceled_result(
            MediaPreviewJob {
                key,
                source_secs: 76.0,
                generation,
                priority: MediaPreviewRequestPriority::Current,
                access_mode: PreviewDecodeAccessMode::PlaybackCursor,
                adaptive_hints: PreviewDecodeAdaptiveHints::default(),
                hardware_decode_request: PreviewHardwareDecodeRequest::Auto,
                hardware_decode_device_selector: None,
                enqueued_at: Instant::now(),
                deadline_at: Some(Instant::now() - Duration::from_millis(1)),
                demand_identity: Some(demand_identity),
                execution_id: None,
            },
            12_000,
            MediaPreviewCancelReason::PlaybackDeadline,
            0,
            Some(0),
            None,
        );
        result.cancellation_phase = Some(MediaPreviewCancellationPhase::Queued);
        result_tx.send(result).expect("send queued expiry result");

        let outcome = service.poll_finished_outcome_with_budget(
            8,
            Duration::from_millis(5),
            Some(demand_identity),
        );

        assert_eq!(
            outcome.frame_deliveries,
            vec![mondrian_playback::FrameDelivery::for_demand(
                demand_identity,
                mondrian_playback::FrameDeliveryKind::Late,
            )]
        );
        let diagnostics = service.diagnostics();
        assert_eq!(diagnostics.scheduler.pending_requests, 0);
        assert_eq!(diagnostics.decode_canceled_jobs, 0);
        assert_eq!(diagnostics.decode_cancellation.all.cancellations, 0);
        assert_eq!(diagnostics.playback_schedule.current_drop_late_decisions, 1);
        service.shutdown();
    }

    #[test]
    fn preview_service_poll_releases_expired_playback_deadline_without_preview_refresh() {
        let service = AppUiPreviewService::new_without_workers_for_test();
        let result_tx = install_preview_result_channel_for_test(&service);
        let key = test_media_key(77);
        let generation = service.scheduler.begin_generation();
        let demand_identity = AppUiPreviewService::test_frame_demand_identity();
        assert_eq!(
            service.scheduler.request_with_demand_identity(
                key.clone(),
                generation,
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::PlaybackCursor,
                Some(demand_identity),
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
                hardware_decode_request: PreviewHardwareDecodeRequest::Auto,
                hardware_decode_device_selector: None,
                enqueued_at: Instant::now(),
                deadline_at: Some(Instant::now() - Duration::from_millis(1)),
                demand_identity: Some(demand_identity),
                execution_id: None,
            },
            12_000,
            MediaPreviewCancelReason::PlaybackDeadline,
            0,
            Some(0),
            None,
        );
        result_tx.send(result).expect("send canceled result");

        let outcome = service.poll_finished_outcome_with_budget(
            8,
            Duration::from_millis(5),
            Some(demand_identity),
        );
        assert!(
            !outcome.visible_change,
            "deadline cancellation is scheduler/diagnostic evidence, not a new visible frame"
        );
        assert!(
            outcome.frame_deliveries.is_empty(),
            "canceled decode work is not a terminal frame presentation"
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
    fn preview_service_poll_drops_successful_playback_completion_after_deadline() {
        let service = AppUiPreviewService::new_without_workers_for_test();
        let mut state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
        state.play();
        let demand_identity = state
            .pending_playback_frame_demand_identity()
            .expect("playback demand identity");
        let result_tx = install_preview_result_channel_for_test(&service);
        let key = test_media_key(78);
        let generation = service.scheduler.begin_generation();
        let deadline = Instant::now() - Duration::from_millis(1);
        assert_eq!(
            service.scheduler.request_with_binding(
                key.clone(),
                generation,
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::PlaybackCursor,
                Some(demand_identity),
                Some(deadline),
            ),
            MediaPreviewRequestStatus::Scheduled { evicted_prefetch: None, evicted_still: None }
        );
        let mut result = test_successful_media_preview_result(key.clone(), generation, 9);
        result.priority = MediaPreviewRequestPriority::Current;
        result.access_mode = PreviewDecodeAccessMode::PlaybackCursor;
        result.deadline_at = Some(deadline);
        result.demand_identity = Some(demand_identity);
        result_tx.send(result).expect("send late successful result");

        let outcome = service.poll_finished_outcome_with_budget(
            8,
            Duration::from_millis(5),
            Some(demand_identity),
        );

        assert!(
            !outcome.visible_change,
            "a successfully decoded but late playback frame must not refresh the viewer"
        );
        assert!(!outcome.needs_follow_up_poll);
        assert_eq!(
            outcome.frame_deliveries,
            vec![mondrian_playback::FrameDelivery::for_demand(
                demand_identity,
                mondrian_playback::FrameDeliveryKind::Late,
            )]
        );
        let diagnostics = service.diagnostics();
        assert_eq!(diagnostics.scheduler.pending_requests, 0);
        assert_eq!(diagnostics.decode_successes, 1);
        assert_eq!(diagnostics.decode_canceled_jobs, 0);
        assert_eq!(diagnostics.playback_schedule.current_drop_late_decisions, 1);
        assert_eq!(
            diagnostics.playback_schedule.current_proxy_or_hardware_recommended_decisions,
            1
        );
        assert!(
            service.frame_store.borrow_mut().media_frame(&key).is_none(),
            "late playback completion should not enter the media preview cache"
        );
        service.shutdown();
    }

    #[test]
    fn preview_service_stale_late_completion_cannot_publish_terminal_delivery() {
        let service = AppUiPreviewService::new_without_workers_for_test();
        let mut state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
        state.play();
        let demand_identity = state
            .pending_playback_frame_demand_identity()
            .expect("playback demand identity");
        let result_tx = install_preview_result_channel_for_test(&service);
        let key = test_media_key(782);
        let generation = service.scheduler.begin_generation();
        assert!(matches!(
            service.scheduler.request_with_binding(
                key.clone(),
                generation,
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::PlaybackCursor,
                Some(demand_identity),
                Some(Instant::now() - Duration::from_millis(1)),
            ),
            MediaPreviewRequestStatus::Scheduled { .. }
        ));
        let _newer_generation = service.scheduler.begin_generation();
        let mut result = test_successful_media_preview_result(key, generation, 9);
        result.priority = MediaPreviewRequestPriority::Current;
        result.access_mode = PreviewDecodeAccessMode::PlaybackCursor;
        result.deadline_at = Some(Instant::now() - Duration::from_millis(1));
        result.demand_identity = Some(demand_identity);
        result_tx.send(result).expect("send stale late result");

        let outcome = service.poll_finished_outcome_with_budget(
            8,
            Duration::from_millis(5),
            Some(demand_identity),
        );

        assert!(outcome.frame_deliveries.is_empty());
        assert_eq!(service.diagnostics().scheduler.completed_stale_results, 1);
        service.shutdown();
    }

    #[test]
    fn preview_service_current_late_completion_cannot_revive_replaced_playback_demand() {
        let service = AppUiPreviewService::new_without_workers_for_test();
        let mut state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
        state.play();
        let completed_identity = state
            .pending_playback_frame_demand_identity()
            .expect("completed playback demand identity");
        let result_tx = install_preview_result_channel_for_test(&service);
        let key = test_media_key(783);
        let generation = service.scheduler.begin_generation();
        assert!(matches!(
            service.scheduler.request_with_binding(
                key.clone(),
                generation,
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::PlaybackCursor,
                Some(completed_identity),
                Some(Instant::now() - Duration::from_millis(1)),
            ),
            MediaPreviewRequestStatus::Scheduled { .. }
        ));
        state.seek(1);
        let replacement_identity = state
            .pending_playback_frame_demand_identity()
            .expect("replacement playback demand identity");
        assert_ne!(replacement_identity, completed_identity);
        let mut result = test_successful_media_preview_result(key, generation, 9);
        result.priority = MediaPreviewRequestPriority::Current;
        result.access_mode = PreviewDecodeAccessMode::PlaybackCursor;
        result.deadline_at = Some(Instant::now() - Duration::from_millis(1));
        result.demand_identity = Some(completed_identity);
        result_tx.send(result).expect("send replaced late result");

        let outcome = service.poll_finished_outcome_with_budget(
            8,
            Duration::from_millis(5),
            Some(replacement_identity),
        );

        assert!(outcome.frame_deliveries.is_empty());
        assert_eq!(
            service.diagnostics().playback_schedule.current_drop_late_decisions,
            0,
            "completed-demand cleanup must not create pressure on its replacement"
        );
        service.shutdown();
    }

    #[test]
    fn preview_service_deadline_uses_worker_completion_not_later_poll_time() {
        let service = AppUiPreviewService::new_without_workers_for_test();
        let mut state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
        state.play();
        let demand_identity = state
            .pending_playback_frame_demand_identity()
            .expect("playback demand identity");
        let result_tx = install_preview_result_channel_for_test(&service);
        let key = test_media_key(781);
        let generation = service.scheduler.begin_generation();
        let deadline = Instant::now() + Duration::from_millis(20);
        assert_eq!(
            service.scheduler.request_with_binding(
                key.clone(),
                generation,
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::PlaybackCursor,
                Some(demand_identity),
                Some(deadline),
            ),
            MediaPreviewRequestStatus::Scheduled { evicted_prefetch: None, evicted_still: None }
        );
        let mut result = test_successful_media_preview_result(key.clone(), generation, 9);
        result.priority = MediaPreviewRequestPriority::Current;
        result.access_mode = PreviewDecodeAccessMode::PlaybackCursor;
        result.demand_identity = Some(demand_identity);
        result.deadline_at = Some(deadline);
        let execution_id = service
            .scheduler
            .begin_test_execution(MediaPreviewWorkerLane::Playback)
            .expect("worker execution lease");
        assert!(service.scheduler.mark_execution_completed(execution_id));
        result.execution_id = Some(execution_id);
        result_tx.send(result).expect("send on-time worker result");
        while Instant::now() < deadline {
            thread::yield_now();
        }

        let outcome = service.poll_finished_outcome_with_budget(
            8,
            Duration::from_millis(5),
            Some(demand_identity),
        );

        assert!(outcome.visible_change);
        assert!(outcome.frame_deliveries.is_empty());
        assert!(service.frame_store.borrow_mut().media_frame(&key).is_some());
        assert_eq!(
            service.diagnostics().playback_schedule.current_drop_late_decisions,
            0
        );
        service.shutdown();
    }

    #[test]
    fn preview_service_stages_presentable_hardware_fallback_until_presentation() {
        let service = AppUiPreviewService::new_without_workers_for_test();
        let mut state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
        state.play();
        let demand_identity = state
            .pending_playback_frame_demand_identity()
            .expect("playback demand identity");
        let result_tx = install_preview_result_channel_for_test(&service);
        let key = test_media_key(79);
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
        let mut result = test_successful_media_preview_result(key.clone(), generation, 9);
        result.priority = MediaPreviewRequestPriority::Current;
        result.access_mode = PreviewDecodeAccessMode::PlaybackCursor;
        result.demand_identity = Some(demand_identity);
        let decode_diagnostics = test_preview_decode_diagnostics(
            PreviewDecodeAccessMode::PlaybackCursor,
            PreviewHardwareDecodeDecision::CpuRgbaHardwareUnavailable,
            PreviewHardwareDecodeBlocker::TextureResidencyNotConnected,
        );
        result.frame.as_mut().expect("decoded frame").presentation_quality =
            preview_decode_presentation_quality(&decode_diagnostics);
        result.decode_diagnostics = Some(decode_diagnostics);
        result_tx.send(result).expect("send degraded successful result");

        let outcome = service.poll_finished_outcome_with_budget(
            8,
            Duration::from_millis(5),
            Some(demand_identity),
        );

        assert!(
            outcome.visible_change,
            "correct CPU fallback remains presentable"
        );
        assert!(
            outcome.frame_deliveries.is_empty(),
            "decode readiness must not terminate the demand before presentation"
        );
        let cached_quality = service
            .frame_store
            .borrow_mut()
            .media_frame(&key)
            .expect("cached decoded frame")
            .presentation_quality();
        assert_eq!(
            cached_quality,
            mondrian_playback::FramePresentationQuality::Degraded,
            "cache admission must preserve executed hardware-fallback quality"
        );
        service.current_presentation_quality.set(cached_quality);
        let ticket = service.playback_presentation_ticket(&state).expect("presentation ticket");
        let delivery = ticket.complete_at(mondrian_playback::MonotonicTimestamp::ZERO);
        assert_eq!(
            delivery,
            mondrian_playback::FrameDelivery::for_demand(
                demand_identity,
                mondrian_playback::FrameDeliveryKind::Degraded,
            )
        );
        assert!(
            !state.observe_frame_delivery(delivery),
            "presentable fallback must still wait for bounded media lookahead"
        );
        assert!(state.observe_video_preroll(1, 1));
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
                        hardware_decode_request: PreviewHardwareDecodeRequest::Auto,
                        hardware_decode_device_selector: None,
                        enqueued_at: Instant::now(),
                        deadline_at: Some(Instant::now() - Duration::from_millis(1)),
                        demand_identity: None,
                        execution_id: None,
                    },
                    1_000,
                    MediaPreviewCancelReason::PlaybackDeadline,
                    0,
                    Some(0),
                    None,
                ))
                .expect("send canceled result");
        }

        let outcome = service.poll_finished_outcome_with_budget(1, Duration::from_millis(5), None);

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
    fn canceled_current_scrub_requests_follow_up_render_for_settled_frame() {
        let service = AppUiPreviewService::new_without_workers_for_test();
        let result_tx = install_preview_result_channel_for_test(&service);
        let generation = service.scheduler.begin_generation();
        let key = test_media_key(91);
        assert!(matches!(
            service.scheduler.request(
                key.clone(),
                generation,
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::ScrubCursor,
            ),
            MediaPreviewRequestStatus::Scheduled { .. }
        ));
        result_tx
            .send(media_preview_canceled_result(
                MediaPreviewJob {
                    key,
                    source_secs: 3.0,
                    generation,
                    priority: MediaPreviewRequestPriority::Current,
                    access_mode: PreviewDecodeAccessMode::ScrubCursor,
                    adaptive_hints: PreviewDecodeAdaptiveHints::default(),
                    hardware_decode_request: PreviewHardwareDecodeRequest::Auto,
                    hardware_decode_device_selector: None,
                    enqueued_at: Instant::now(),
                    deadline_at: None,
                    demand_identity: None,
                    execution_id: None,
                },
                1_000,
                MediaPreviewCancelReason::Obsolete,
                1_000,
                Some(1_000),
                None,
            ))
            .expect("send canceled scrub result");

        let outcome = service.poll_finished_outcome_with_budget(8, Duration::from_millis(5), None);

        assert!(
            outcome.visible_change,
            "settled non-playback work must get a render pass after cancellation"
        );
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
                AlphaInterpretation::Straight,
                0,
                0.0,
                width,
                height,
                &color_context,
                true,
                false,
            )
            .expect("media preview key");
        service.frame_store.borrow_mut().remember_failure(key);

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
    fn media_preview_cache_identity_changes_with_range_override() {
        let (state, asset_id, root) = state_with_invalid_video_asset();
        let service = AppUiPreviewService::new_without_workers_for_test();
        let sequence = state.sequence.as_ref().expect("sequence");
        let (width, height) = preview_dimensions_for_sequence(sequence);
        let color_context = sequence.settings.root_preview_color_context(
            &state.project_settings.color_management,
            ColorSpace::Rec709,
        );
        let key_for_state = || {
            service
                .media_preview_key_for_asset(
                    &state,
                    &asset_id,
                    None,
                    AlphaInterpretation::Straight,
                    0,
                    0.0,
                    width,
                    height,
                    &color_context,
                    false,
                    false,
                )
                .expect("media preview key")
                .0
        };

        let auto_key = key_for_state();
        assert_eq!(
            auto_key.input_video_range.baseline(),
            DecodedVideoRange::Limited
        );
        let library = state.asset_library.as_ref().expect("asset library");
        let mut interpretation = library
            .get_asset(asset_id)
            .expect("read asset")
            .expect("asset exists")
            .interpretation;
        interpretation.range = mondrian_core::timeline_data::MediaRangeInterpretation::Override {
            range: mondrian_core::timeline_data::MediaSignalRange::Full,
        };
        library
            .set_asset_interpretation(asset_id, interpretation)
            .expect("persist range override");

        let override_key = key_for_state();
        assert_eq!(
            override_key.input_video_range.baseline(),
            DecodedVideoRange::Full
        );
        assert_eq!(
            override_key.input_video_range,
            DecodedVideoRangeContract::OverrideFull
        );
        assert_ne!(auto_key, override_key);

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
                AlphaInterpretation::Straight,
                0,
                0.0,
                width,
                height,
                &color_context,
                true,
                false,
            )
            .expect("media preview key");
        service.frame_store.borrow_mut().insert_media_frame(
            key,
            test_media_frame_with_size(80, width, height, 123),
            false,
        );

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

    fn begin_test_media_execution(
        service: &AppUiPreviewService,
        key: MediaPreviewKey,
        generation: u64,
        priority: MediaPreviewRequestPriority,
        access_mode: PreviewDecodeAccessMode,
        lane: MediaPreviewWorkerLane,
    ) -> mondrian_playback::FrameExecutionId {
        let source_secs = key.source_micros as f64 / 1_000_000.0;
        assert_eq!(
            service.jobs.enqueue(MediaPreviewJob {
                key,
                source_secs,
                generation,
                priority,
                access_mode,
                adaptive_hints: PreviewDecodeAdaptiveHints::default(),
                hardware_decode_request: PreviewHardwareDecodeRequest::Auto,
                hardware_decode_device_selector: None,
                enqueued_at: Instant::now(),
                deadline_at: None,
                demand_identity: None,
                execution_id: None,
            }),
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        );
        service.scheduler.begin_test_execution(lane).expect("test execution lease")
    }

    fn test_cpu_frame_store(
        media_entry_capacity: usize,
        media_byte_budget: usize,
        failure_entry_capacity: usize,
    ) -> PreviewCpuFrameStore {
        PreviewCpuFrameStore::new(PreviewCpuFrameStoreConfig {
            media_entry_capacity,
            media_byte_budget,
            media_resource_unit_budget: 4,
            viewer_entry_capacity: 4,
            viewer_byte_budget: 1_024,
            failure_entry_capacity,
        })
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
            deadline_at: None,
            cancel_observed_elapsed_us: None,
            cancel_request_to_observed_us: None,
            canceled: false,
            cancellation_phase: None,
            cancel_reason: None,
            decode_diagnostics: None,
            color_diagnostics: None,
            color_stage_diagnostics: None,
            demand_identity: None,
            execution_id: None,
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
        let input_transform = RenderInputTransform::to_working(
            WorkingColorSpace::LinearRec709,
            false,
            ColorEngine::mondrian_standard(),
        );
        let frame =
            execute_cpu_input_stage(&source, &input_transform).expect("test media input transform");
        let frame = frame.result.frame;
        MediaPreviewFrame {
            width,
            height,
            logical_width: width,
            logical_height: height,
            frame: Some(frame),
            gpu_source: Some(MediaPreviewGpuSourceFrame::new(source, input_transform)),
            native_source: None,
            signature,
            presentation_quality: mondrian_playback::FramePresentationQuality::Ready,
            decode_execution: AppUiPreviewDecodeExecutionSummary::from_path(
                PreviewDecodeExecutionPath::SoftwareCpu,
            ),
        }
    }

    #[derive(Debug)]
    struct TestNativeDecodedFrameResource {
        kind: DecodedGpuFrameHandleKind,
        id: std::num::NonZeroU64,
    }

    impl mondrian_media::PreviewNativeDecodedFrameResource for TestNativeDecodedFrameResource {
        fn handle_kind(&self) -> DecodedGpuFrameHandleKind {
            self.kind
        }

        fn handle_id(&self) -> std::num::NonZeroU64 {
            self.id
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    fn test_native_source_frame(width: u32, height: u32) -> MediaPreviewNativeSourceFrame {
        let handle = PreviewNativeDecodedFrameHandle::new(TestNativeDecodedFrameResource {
            kind: DecodedGpuFrameHandleKind::D3D11Texture2D,
            id: std::num::NonZeroU64::new(7).expect("non-zero native frame handle"),
        });
        let native_frame = PreviewNativeDecodedFrame::new(
            width,
            height,
            handle,
            DecodedVideoSurfaceFormat::P010,
            DecodedVideoSampling {
                matrix: mondrian_media::DecodedVideoMatrix::Bt709,
                range: DecodedVideoRange::Limited,
                chroma_location: DecodedVideoChromaLocation::Left,
                bit_depth: 10,
            },
            test_preview_decode_diagnostics(
                PreviewDecodeAccessMode::PlaybackCursor,
                PreviewHardwareDecodeDecision::GpuResidentNative,
                PreviewHardwareDecodeBlocker::None,
            ),
        )
        .expect("test native decoded frame");
        MediaPreviewNativeSourceFrame::from_native_frame(
            native_frame,
            ColorSpace::Rec709,
            RenderInputTransform::to_working_gpu(
                WorkingColorSpace::LinearRec709,
                false,
                ColorEngine::mondrian_standard(),
            ),
        )
    }

    fn test_media_frame_rgba8(frame: &MediaPreviewFrame) -> Vec<u8> {
        let working = frame.working_frame().expect("test media working frame");
        mondrian_renderer::execute_cpu_output_boundary(
            &working.frame,
            &RenderOutputColorBoundary::display(
                ColorSpace::Rec709,
                false,
                ColorEngine::mondrian_standard(),
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
        let input_transform = RenderInputTransform::to_working(
            WorkingColorSpace::LinearRec709,
            false,
            ColorEngine::mondrian_standard(),
        );
        let frame = MediaPreviewFrame {
            width: 1,
            height: 1,
            logical_width: 1,
            logical_height: 1,
            frame: None,
            gpu_source: Some(MediaPreviewGpuSourceFrame::new(source, input_transform)),
            native_source: None,
            signature: 42,
            presentation_quality: mondrian_playback::FramePresentationQuality::Ready,
            decode_execution: AppUiPreviewDecodeExecutionSummary::from_path(
                PreviewDecodeExecutionPath::SoftwareCpu,
            ),
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

    #[test]
    fn media_preview_scene_linear_source_stays_gpu_eligible_and_lazily_caches_cpu_fallback() {
        let samples = Arc::new(vec![-0.25, 0.18, 2.0, 1.0, 4.0, 0.5, -1.0, 0.25]);
        let source =
            LinearFloatSource::new_shared(2, 1, ColorSpace::LinearRec709, Arc::clone(&samples));
        let input_transform = RenderInputTransform::to_working(
            WorkingColorSpace::LinearRec709,
            false,
            ColorEngine::mondrian_standard(),
        );
        let frame = MediaPreviewFrame {
            width: 2,
            height: 1,
            logical_width: 2,
            logical_height: 1,
            frame: None,
            gpu_source: Some(MediaPreviewGpuSourceFrame::new(source, input_transform)),
            native_source: None,
            signature: 43,
            presentation_quality: mondrian_playback::FramePresentationQuality::Ready,
            decode_execution: AppUiPreviewDecodeExecutionSummary::from_path(
                PreviewDecodeExecutionPath::SoftwareCpu,
            ),
        };

        let gpu_source = frame.gpu_source().expect("scene-linear GPU source");
        let CpuSourceColorFrame::LinearFloat(gpu_float) = gpu_source.source.as_ref() else {
            panic!("scene-linear preview must retain a float GPU source");
        };
        assert!(Arc::ptr_eq(&gpu_float.data_shared(), &samples));
        assert_eq!(frame.reserved_cpu_bytes(), 64);

        let first = frame.working_frame().expect("first float CPU fallback");
        let second = frame.working_frame().expect("cached float CPU fallback");
        assert!(first.color_diagnostics.is_some());
        assert!(second.color_diagnostics.is_none());
        assert_eq!(first.frame.rgba_f32().data[0], [-0.25, 0.18, 2.0, 1.0]);
        assert_eq!(first.frame.rgba_f32().data, second.frame.rgba_f32().data);
    }

    fn test_proxy_config(cache_dir: PathBuf) -> mondrian_media::ProxyConfig {
        mondrian_media::ProxyConfig {
            cache_dir,
            ..mondrian_media::ProxyConfig::default()
        }
    }

    fn test_proxy_color() -> mondrian_media::ProxyColorContract {
        mondrian_media::ProxyColorContract::try_new(
            ColorSpace::Rec709,
            8,
            DecodedVideoRange::Limited,
        )
        .expect("valid proxy color contract")
    }

    fn install_test_proxy_manifest(
        generator: &mondrian_media::ProxyGenerator,
        source: &Path,
        proxy: &Path,
    ) {
        let manifest = generator
            .expected_manifest(source, test_proxy_color())
            .expect("expected proxy manifest");
        let bytes = serde_json::to_vec_pretty(&manifest).expect("serialize proxy manifest");
        std::fs::write(mondrian_media::ProxyGenerator::manifest_path(proxy), bytes)
            .expect("write proxy manifest");
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
        let generator = mondrian_media::ProxyGenerator::new(proxy_config.clone());
        let proxy_path = generator.proxy_path(&source, test_proxy_color()).expect("proxy path");
        std::fs::create_dir_all(proxy_path.parent().expect("proxy parent")).expect("proxy root");
        std::fs::write(&proxy_path, b"proxy").expect("proxy");
        install_test_proxy_manifest(&generator, &source, &proxy_path);

        let resolved = resolve_preview_media_decode_path(
            true,
            false,
            &source,
            &proxy_config,
            Some(test_proxy_color()),
        )
        .expect("fresh proxy path");

        assert_eq!(resolved.path, proxy_path);
        assert_eq!(resolved.resolution, PreviewMediaDecodePathResolution::Proxy);
        assert_eq!(resolved.fingerprint.len, Some(5));

        let alpha_resolved = resolve_preview_media_decode_path(
            true,
            true,
            &source,
            &proxy_config,
            Some(test_proxy_color()),
        )
        .expect("alpha source path");
        assert_eq!(alpha_resolved.path, source);
        assert_eq!(
            alpha_resolved.resolution,
            PreviewMediaDecodePathResolution::Source
        );
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

        let resolved = resolve_preview_media_decode_path(
            true,
            false,
            &source,
            &proxy_config,
            Some(test_proxy_color()),
        )
        .expect("source path");

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
        let proxy_path = mondrian_media::ProxyGenerator::new(proxy_config.clone())
            .proxy_path(&source, test_proxy_color())
            .expect("proxy path");
        std::fs::create_dir_all(proxy_path.parent().expect("proxy parent")).expect("proxy root");
        std::fs::write(&proxy_path, b"proxy").expect("proxy");
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&source, b"newer source").expect("source");

        let resolved = resolve_preview_media_decode_path(
            true,
            false,
            &source,
            &proxy_config,
            Some(test_proxy_color()),
        )
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

        assert!(resolve_preview_media_decode_path(
            false,
            false,
            &source,
            &proxy_config,
            Some(test_proxy_color())
        )
        .is_none());
        assert!(resolve_preview_media_decode_path(
            true,
            false,
            &source,
            &proxy_config,
            Some(test_proxy_color())
        )
        .is_none());
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
    fn gpu_viewer_hardware_decode_admission_covers_every_access_mode() {
        let service = AppUiPreviewService::new_without_workers_for_test();

        assert_eq!(
            service
                .hardware_decode_request_for_access_mode(PreviewDecodeAccessMode::PlaybackCursor),
            PreviewHardwareDecodeRequest::Auto
        );
        service.set_playback_hardware_decode_admission(AppUiPlaybackHardwareDecodeAdmission {
            request: PreviewHardwareDecodeRequest::PreferGpuResident,
            hardware_decode_device_selector: Some(HwAccelDeviceSelector::D3D12VaAdapterIndex(1)),
            renderer_native_import_ready: true,
            platform_native_import_ready: true,
            native_import_admission_ready: true,
            admission_blocker: None,
            platform_discovery_available: true,
            platform_zero_copy_supported: true,
            platform_low_copy_fallback_supported: false,
            renderer_supported_handle_kinds: 1,
            renderer_supported_source_texture_formats: 1,
            renderer_supports_nv12: true,
            renderer_supports_p010: true,
        });
        assert_eq!(
            service
                .hardware_decode_request_for_access_mode(PreviewDecodeAccessMode::PlaybackCursor),
            PreviewHardwareDecodeRequest::PreferGpuResident
        );
        assert_eq!(
            service.hardware_decode_device_selector_for_access_mode(
                PreviewDecodeAccessMode::PlaybackCursor
            ),
            Some(HwAccelDeviceSelector::D3D12VaAdapterIndex(1))
        );
        assert_eq!(
            service.hardware_decode_request_for_access_mode(PreviewDecodeAccessMode::ScrubCursor),
            PreviewHardwareDecodeRequest::PreferGpuResident
        );
        assert_eq!(
            service.hardware_decode_device_selector_for_access_mode(
                PreviewDecodeAccessMode::ScrubCursor
            ),
            Some(HwAccelDeviceSelector::D3D12VaAdapterIndex(1))
        );
        assert_eq!(
            service.hardware_decode_request_for_access_mode(
                PreviewDecodeAccessMode::RandomAccessStillFrame
            ),
            PreviewHardwareDecodeRequest::PreferGpuResident
        );
        assert_eq!(
            service.hardware_decode_device_selector_for_access_mode(
                PreviewDecodeAccessMode::RandomAccessStillFrame
            ),
            Some(HwAccelDeviceSelector::D3D12VaAdapterIndex(1))
        );
    }

    #[test]
    fn gpu_viewer_hardware_decode_admission_is_scoped_to_native_surface_format() {
        let service = AppUiPreviewService::new_without_workers_for_test();
        service.set_playback_hardware_decode_admission(AppUiPlaybackHardwareDecodeAdmission {
            request: PreviewHardwareDecodeRequest::PreferGpuResident,
            hardware_decode_device_selector: Some(HwAccelDeviceSelector::D3D12VaAdapterIndex(1)),
            renderer_native_import_ready: true,
            platform_native_import_ready: true,
            native_import_admission_ready: true,
            admission_blocker: None,
            platform_discovery_available: true,
            platform_zero_copy_supported: true,
            platform_low_copy_fallback_supported: false,
            renderer_supported_handle_kinds: 1,
            renderer_supported_source_texture_formats: 1,
            renderer_supports_nv12: true,
            renderer_supports_p010: false,
        });
        let mut key = test_media_key(0);

        key.native_surface_hint = Some(MediaPreviewNativeSurfaceHint::Nv12);
        assert_eq!(
            service.hardware_decode_request_for_key(PreviewDecodeAccessMode::PlaybackCursor, &key),
            PreviewHardwareDecodeRequest::PreferGpuResident
        );

        key.native_surface_hint = Some(MediaPreviewNativeSurfaceHint::P010);
        assert_eq!(
            service.hardware_decode_request_for_key(PreviewDecodeAccessMode::PlaybackCursor, &key),
            PreviewHardwareDecodeRequest::PreferHardwareDecode
        );
    }

    #[test]
    fn native_decode_key_is_stable_across_viewer_quality_scales() {
        let service = AppUiPreviewService::new_without_workers_for_test();
        service.set_playback_hardware_decode_admission(AppUiPlaybackHardwareDecodeAdmission {
            request: PreviewHardwareDecodeRequest::PreferGpuResident,
            hardware_decode_device_selector: Some(HwAccelDeviceSelector::D3D12VaAdapterIndex(0)),
            renderer_native_import_ready: true,
            platform_native_import_ready: true,
            native_import_admission_ready: true,
            admission_blocker: None,
            platform_discovery_available: true,
            platform_zero_copy_supported: true,
            platform_low_copy_fallback_supported: false,
            renderer_supported_handle_kinds: 1,
            renderer_supported_source_texture_formats: 1,
            renderer_supports_nv12: true,
            renderer_supports_p010: true,
        });
        let mut full = test_media_key(12);
        full.source_width = 3840;
        full.source_height = 2160;
        full.target_width = 960;
        full.target_height = 540;
        full.native_surface_hint = Some(MediaPreviewNativeSurfaceHint::P010);
        let mut quarter = full.clone();
        quarter.target_width = 240;
        quarter.target_height = 135;

        let full = service.canonicalize_media_decode_geometry(full);
        let quarter = service.canonicalize_media_decode_geometry(quarter);

        assert_eq!(full, quarter);
        assert_eq!((full.target_width, full.target_height), (3840, 2160));
    }

    #[test]
    fn cpu_decode_key_retains_requested_decode_extent() {
        let service = AppUiPreviewService::new_without_workers_for_test();
        let mut key = test_media_key(12);
        key.source_width = 3840;
        key.source_height = 2160;
        key.target_width = 960;
        key.target_height = 540;
        key.native_surface_hint = Some(MediaPreviewNativeSurfaceHint::P010);

        let key = service.canonicalize_media_decode_geometry(key);

        assert_eq!((key.target_width, key.target_height), (960, 540));
    }

    #[test]
    fn scrub_adaptation_switches_for_hot_region_and_slow_latency() {
        let mut adaptation = PreviewScrubAdaptationState::default();
        let mut key = test_media_key(100);
        let observed_at = Instant::now();

        assert_eq!(
            adaptation
                .observe_request(key.asset_id, key.source_micros, observed_at)
                .scrub_class,
            PreviewScrubAdaptiveClass::Normal
        );
        key.source_micros += 100_000;
        assert_eq!(
            adaptation
                .observe_request(
                    key.asset_id,
                    key.source_micros,
                    observed_at + Duration::from_millis(1),
                )
                .scrub_class,
            PreviewScrubAdaptiveClass::Normal
        );
        key.source_micros += 100_000;
        assert_eq!(
            adaptation
                .observe_request(
                    key.asset_id,
                    key.source_micros,
                    observed_at + Duration::from_millis(2),
                )
                .scrub_class,
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
            requested_pts: Some(100),
            selected_pts: Some(100),
            temporal_approximation: false,
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
            hardware_decode_ffmpeg_device_type_available: false,
            hardware_decode_ffmpeg_codec_config_available: false,
            hardware_decode_ffmpeg_hw_pixel_format: None,
            hardware_decode_ffmpeg_device_context_attempted: false,
            hardware_decode_ffmpeg_device_context_created: false,
            hardware_decode_ffmpeg_device_context_error_code: None,
            hardware_decode_cpu_transfer_configured: false,
            hardware_decode_cpu_transfer_observed: false,
            hardware_decode_cpu_transfer_status:
                PreviewHardwareDecodeCpuTransferStatus::NotAttempted,
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
            hardware_decode_blocker: PreviewHardwareDecodeBlocker::TextureResidencyNotConnected,
            native_decode_fallback: None,
            decoded_surface_format: DecodedVideoSurfaceFormat::Unknown,
            decoded_video_sampling: DecodedVideoSampling::default(),
        };
        adaptation.observe_decode(slow_decode);
        adaptation.observe_decode(slow_decode);
        key.source_micros += 100_000;
        assert_eq!(
            adaptation
                .observe_request(
                    key.asset_id,
                    key.source_micros,
                    observed_at + Duration::from_millis(3),
                )
                .scrub_class,
            PreviewScrubAdaptiveClass::SlowLatency
        );

        let mut failed_adaptation = PreviewScrubAdaptationState::default();
        failed_adaptation.observe_failure(
            PreviewDecodeAccessMode::ScrubCursor,
            MediaPreviewFailureReason::ForwardDecodeBudgetExhausted,
        );
        assert_eq!(
            failed_adaptation
                .observe_request(
                    key.asset_id,
                    key.source_micros,
                    observed_at + Duration::from_millis(4),
                )
                .scrub_class,
            PreviewScrubAdaptiveClass::SlowLatency
        );
    }

    #[test]
    fn cpu_frame_store_evicts_least_recently_used_media_frame() {
        let mut store = PreviewCpuFrameStore::new(PreviewCpuFrameStoreConfig {
            media_entry_capacity: 2,
            media_byte_budget: 1_024,
            media_resource_unit_budget: 4,
            viewer_entry_capacity: 2,
            viewer_byte_budget: 1_024,
            failure_entry_capacity: 2,
        });
        let first = test_media_key(1);
        let second = test_media_key(2);
        let third = test_media_key(3);

        store.insert_media_frame(first.clone(), test_media_frame(1), false);
        store.insert_media_frame(second.clone(), test_media_frame(2), false);
        assert!(store.media_frame(&first).is_some());

        store.insert_media_frame(third.clone(), test_media_frame(3), false);

        assert_eq!(store.diagnostics().media_entries, 2);
        assert!(store.media_frame(&first).is_some());
        assert!(store.media_frame(&second).is_none());
        assert!(store.media_frame(&third).is_some());
    }

    #[test]
    fn cpu_frame_store_updates_existing_media_frame_without_growing() {
        let mut store = PreviewCpuFrameStore::new(PreviewCpuFrameStoreConfig {
            media_entry_capacity: 2,
            media_byte_budget: 1_024,
            media_resource_unit_budget: 4,
            viewer_entry_capacity: 2,
            viewer_byte_budget: 1_024,
            failure_entry_capacity: 2,
        });
        let key = test_media_key(1);

        store.insert_media_frame(key.clone(), test_media_frame(1), false);
        store.insert_media_frame(key.clone(), test_media_frame(9), false);

        let frame = store.media_frame(&key).expect("updated frame");
        assert_eq!(store.diagnostics().media_entries, 1);
        assert_eq!(test_media_frame_rgba8(&frame), vec![9, 0, 0, 255]);
    }

    #[test]
    fn cpu_frame_store_stays_within_budget_across_one_hundred_media_regions() {
        let frame_bytes = test_media_frame(0).reserved_cpu_bytes();
        let byte_budget = frame_bytes.saturating_mul(3);
        let mut store = test_cpu_frame_store(100, byte_budget, 8);

        for region in 0..100i64 {
            assert!(store.insert_media_frame(
                test_media_key(region),
                test_media_frame(region as u8),
                false,
            ));
            let diagnostics = store.diagnostics();
            assert!(diagnostics.media_reserved_bytes <= diagnostics.media_byte_budget);
        }

        let diagnostics = store.diagnostics();
        assert_eq!(diagnostics.media_entries, 3);
        assert_eq!(diagnostics.media_evictions, 97);
        assert_eq!(diagnostics.media_reserved_bytes, byte_budget);

        store.clear_all();
        let cleared = store.diagnostics();
        assert_eq!(cleared.media_entries, 0);
        assert_eq!(cleared.media_reserved_bytes, 0);
    }

    #[test]
    fn cpu_frame_store_pins_only_an_oversize_current_media_frame() {
        let current_key = test_media_key(200);
        let current_frame = test_media_frame(7);
        let frame_bytes = current_frame.reserved_cpu_bytes();
        let mut store = test_cpu_frame_store(4, frame_bytes.saturating_sub(1), 4);

        assert!(!store.insert_media_frame(current_key.clone(), current_frame, true));
        assert!(store.media_frame(&current_key).is_some());
        let current = store.diagnostics();
        assert_eq!(current.media_entries, 0);
        assert_eq!(current.pinned_media_bytes, frame_bytes);
        assert_eq!(current.media_oversize_rejections, 1);

        let prefetch_key = test_media_key(201);
        assert!(!store.insert_media_frame(prefetch_key.clone(), test_media_frame(8), false));
        assert!(store.media_frame(&prefetch_key).is_none());
        assert!(store.media_frame(&current_key).is_some());

        store.clear_pinned_media_frame();
        assert!(store.media_frame(&current_key).is_none());
        assert_eq!(store.diagnostics().pinned_media_bytes, 0);
    }

    #[test]
    fn cpu_frame_store_clear_releases_frames_failures_and_reserved_bytes() {
        let mut store = PreviewCpuFrameStore::new(PreviewCpuFrameStoreConfig {
            media_entry_capacity: 2,
            media_byte_budget: 1_024,
            media_resource_unit_budget: 4,
            viewer_entry_capacity: 2,
            viewer_byte_budget: 1_024,
            failure_entry_capacity: 2,
        });
        let key = test_media_key(1);

        store.insert_media_frame(key.clone(), test_media_frame(1), false);
        store.remember_failure(key.clone());
        store.clear_all();

        let diagnostics = store.diagnostics();
        assert_eq!(diagnostics.media_entries, 0);
        assert_eq!(diagnostics.media_reserved_bytes, 0);
        assert_eq!(diagnostics.failure_entries, 0);
        assert!(store.media_frame(&key).is_none());
        assert!(!store.contains_failure(&key));
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
        let mut store = test_cpu_frame_store(2, 1_024, 2);

        store.insert_media_frame(old_key.clone(), test_media_frame(1), false);

        assert!(store.media_frame(&new_key).is_none());
        assert!(store.media_frame(&old_key).is_some());
    }

    #[test]
    fn media_preview_caches_isolate_ocio_config_generations() {
        let mut old_key = test_media_key(1);
        old_key.ocio_generation = 41;
        let mut new_key = old_key.clone();
        new_key.ocio_generation = 42;
        let mut store = test_cpu_frame_store(2, 1_024, 2);

        store.insert_media_frame(old_key.clone(), test_media_frame(1), false);
        store.remember_failure(old_key.clone());

        assert!(store.media_frame(&new_key).is_none());
        assert!(!store.contains_failure(&new_key));
        assert!(store.media_frame(&old_key).is_some());
        assert!(store.contains_failure(&old_key));
    }

    #[test]
    fn media_preview_failure_cache_evicts_least_recently_used_key() {
        let mut store = test_cpu_frame_store(2, 1_024, 2);
        let first = test_media_key(1);
        let second = test_media_key(2);
        let third = test_media_key(3);

        store.remember_failure(first.clone());
        store.remember_failure(second.clone());
        assert!(store.contains_failure(&first));

        store.remember_failure(third.clone());

        assert_eq!(store.diagnostics().failure_entries, 2);
        assert!(store.contains_failure(&first));
        assert!(!store.contains_failure(&second));
        assert!(store.contains_failure(&third));
    }

    #[test]
    fn media_preview_failure_cache_updates_existing_key_without_growing() {
        let mut store = test_cpu_frame_store(2, 1_024, 2);
        let key = test_media_key(1);

        store.remember_failure(key.clone());
        store.remember_failure(key.clone());

        assert_eq!(store.diagnostics().failure_entries, 1);
        assert!(store.contains_failure(&key));
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
                hardware_decode_request: PreviewHardwareDecodeRequest::Auto,
                hardware_decode_device_selector: None,
                enqueued_at: Instant::now(),
                deadline_at: None,
                demand_identity: None,
                execution_id: None,
            }),
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        );
        service.frame_store.borrow_mut().insert_media_frame(
            key.clone(),
            test_media_frame(1),
            false,
        );
        service.frame_store.borrow_mut().remember_failure(key.clone());

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
        let frame_store = service.frame_store.borrow().diagnostics();
        assert_eq!(frame_store.media_entries, 0);
        assert_eq!(frame_store.media_reserved_bytes, 0);
        assert_eq!(frame_store.failure_entries, 0);
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

        assert!(service.poll_finished_with_budget(2, Duration::from_secs(1), None));
        let diagnostics = service.diagnostics();
        assert_eq!(diagnostics.decode_successes, 2);
        assert_eq!(diagnostics.completion_poll_calls, 1);
        assert_eq!(diagnostics.completion_poll_results, 2);
        assert_eq!(diagnostics.completion_poll_max_results_per_poll, 2);
        assert_eq!(diagnostics.completion_poll_count_budget_exhaustions, 1);
        assert_eq!(diagnostics.completion_poll_time_budget_exhaustions, 0);
        assert_eq!(service.scheduler.pending_len(), 1);

        assert!(service.poll_finished_with_budget(2, Duration::from_secs(1), None));
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

        assert!(service.poll_finished_with_budget(8, Duration::ZERO, None));

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
    fn media_preview_forward_prefetch_window_uses_sequence_frame_rate() {
        assert_eq!(
            media_preview_forward_prefetch_window_frames(Rational::FPS_24),
            Some(6)
        );
        assert_eq!(
            media_preview_forward_prefetch_window_frames(Rational::FPS_30),
            Some(8)
        );
        assert_eq!(
            media_preview_forward_prefetch_window_frames(Rational::FPS_60),
            Some(15)
        );
        assert_eq!(
            media_preview_forward_prefetch_window_frames(Rational::new(240, 1)),
            Some(MEDIA_PREVIEW_FORWARD_PREFETCH_MAX_FRAMES)
        );
        assert_eq!(
            media_preview_forward_prefetch_window_frames(Rational::FPS_10),
            Some(3)
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
                None,
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
                Some(
                    mondrian_playback::FrameExecutionCancellation::BrokerClosed {
                        age: Duration::ZERO,
                    }
                ),
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::ScrubCursor,
                Duration::ZERO,
                false,
            ),
            Some(MediaPreviewCancelReason::Shutdown),
        );
    }

    #[test]
    fn preview_shutdown_signal_is_idempotent_without_timing_authority() {
        let shutdown = PreviewShutdownSignal::default();
        assert!(!shutdown.is_requested());

        assert!(!shutdown.request());
        assert!(shutdown.is_requested());
        assert!(shutdown.request());
    }

    #[test]
    fn media_preview_shutdown_observation_uses_broker_close_timestamp() {
        let scheduler = MediaPreviewScheduler::with_max_pending(1);
        let (sender, receiver) = scheduler.job_queue();
        let generation = scheduler.begin_generation();
        assert!(matches!(
            scheduler.request(
                test_media_key(500),
                generation,
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::PlaybackCursor,
            ),
            MediaPreviewRequestStatus::Scheduled { .. }
        ));
        let execution_id = receiver
            .recv_for_worker(MediaPreviewWorkerLane::Playback)
            .and_then(|job| job.execution_id)
            .expect("worker execution lease");

        sender.close();
        let scheduler_cancellation = scheduler.execution_cancellation(execution_id);
        assert!(matches!(
            scheduler_cancellation,
            Some(mondrian_playback::FrameExecutionCancellation::BrokerClosed { .. })
        ));
        let observed_at = Instant::now();
        let latency = media_preview_cancel_request_to_observed_us(
            MediaPreviewCancelReason::Shutdown,
            scheduler_cancellation,
            observed_at,
            observed_at,
        );

        assert_eq!(
            latency,
            scheduler_cancellation
                .and_then(mondrian_playback::FrameExecutionCancellation::request_age)
                .map(app_duration_us)
        );
    }

    #[test]
    fn media_preview_cancel_observation_uses_scheduler_invalidation_timestamp() {
        let scheduler = MediaPreviewScheduler::with_max_pending(2);
        let (_sender, receiver) = scheduler.job_queue();
        let generation = scheduler.begin_generation();
        assert!(matches!(
            scheduler.request(
                test_media_key(501),
                generation,
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::ScrubCursor,
            ),
            MediaPreviewRequestStatus::Scheduled { .. }
        ));
        let execution_id = receiver
            .recv_for_worker(MediaPreviewWorkerLane::Scrub)
            .and_then(|job| job.execution_id)
            .expect("worker execution lease");

        scheduler.begin_generation();
        let scheduler_cancellation = scheduler.execution_cancellation(execution_id);
        let observed_at = Instant::now();
        let latency = media_preview_cancel_request_to_observed_us(
            MediaPreviewCancelReason::Obsolete,
            scheduler_cancellation,
            observed_at,
            observed_at,
        );

        assert!(latency.is_some());
    }

    #[test]
    fn media_preview_cancel_observation_uses_competing_request_timestamp() {
        let scheduler = MediaPreviewScheduler::with_max_pending(2);
        let (_sender, receiver) = scheduler.job_queue();
        let generation = scheduler.begin_generation();
        assert!(matches!(
            scheduler.request(
                test_media_key(502),
                generation,
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::RandomAccessStillFrame,
            ),
            MediaPreviewRequestStatus::Scheduled { .. }
        ));
        let execution_id = receiver
            .recv_for_worker(MediaPreviewWorkerLane::Still)
            .and_then(|job| job.execution_id)
            .expect("worker execution lease");
        assert!(matches!(
            scheduler.request(
                test_media_key(503),
                generation,
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::PlaybackCursor,
            ),
            MediaPreviewRequestStatus::Scheduled { .. }
        ));
        let scheduler_cancellation = scheduler.execution_cancellation(execution_id);
        let observed_at = Instant::now();
        let latency = media_preview_cancel_request_to_observed_us(
            MediaPreviewCancelReason::StillPreemptedByRealtimeCurrent,
            scheduler_cancellation,
            observed_at,
            observed_at,
        );

        assert!(latency.is_some());
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
                None,
                MediaPreviewRequestPriority::Prefetch,
                PreviewDecodeAccessMode::PlaybackCursor,
                Duration::from_micros(MEDIA_PREVIEW_PREFETCH_DECODE_BUDGET_US - 1),
                false,
            ),
            None,
        );
        assert_eq!(
            media_preview_cancel_reason(
                None,
                MediaPreviewRequestPriority::Prefetch,
                PreviewDecodeAccessMode::PlaybackCursor,
                Duration::from_micros(MEDIA_PREVIEW_PREFETCH_DECODE_BUDGET_US),
                false,
            ),
            Some(MediaPreviewCancelReason::PrefetchDeadline),
        );
        assert_eq!(
            media_preview_cancel_reason(
                None,
                MediaPreviewRequestPriority::Prefetch,
                PreviewDecodeAccessMode::ScrubCursor,
                Duration::from_micros(MEDIA_PREVIEW_PREFETCH_DECODE_BUDGET_US * 4),
                false,
            ),
            None,
        );
    }

    #[test]
    fn startup_preroll_prefetch_uses_session_deadline_instead_of_steady_state_budget() {
        let future_deadline = Instant::now() + Duration::from_millis(500);
        assert_eq!(
            media_preview_cancel_reason_at_checkpoint(
                None,
                MediaPreviewRequestPriority::Prefetch,
                PreviewDecodeAccessMode::PlaybackCursor,
                Duration::from_micros(MEDIA_PREVIEW_PREFETCH_DECODE_BUDGET_US * 4),
                Some(future_deadline),
            ),
            None,
        );
        assert_eq!(
            media_preview_cancel_reason_at_checkpoint(
                Some(
                    mondrian_playback::FrameExecutionCancellation::DeadlineExpired {
                        age: Duration::from_millis(1),
                    }
                ),
                MediaPreviewRequestPriority::Prefetch,
                PreviewDecodeAccessMode::PlaybackCursor,
                Duration::ZERO,
                Some(future_deadline),
            ),
            Some(MediaPreviewCancelReason::PrefetchDeadline),
        );
    }

    #[test]
    fn media_preview_decode_cancellation_preempts_prefetch_for_current_work() {
        assert_eq!(
            media_preview_cancel_reason(
                Some(
                    mondrian_playback::FrameExecutionCancellation::PrefetchPreemptedByCurrent {
                        request_age: Duration::ZERO,
                    },
                ),
                MediaPreviewRequestPriority::Prefetch,
                PreviewDecodeAccessMode::PlaybackCursor,
                Duration::ZERO,
                false,
            ),
            Some(MediaPreviewCancelReason::PrefetchPreemptedByCurrent),
        );
        assert_eq!(
            media_preview_cancel_reason(
                None,
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
                Some(
                    mondrian_playback::FrameExecutionCancellation::StillPreemptedByRealtimeCurrent {
                        request_age: Duration::ZERO,
                    },
                ),
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::RandomAccessStillFrame,
                Duration::ZERO,
                false,
            ),
            Some(MediaPreviewCancelReason::StillPreemptedByRealtimeCurrent),
        );
        assert_eq!(
            media_preview_cancel_reason(
                None,
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
                Some(mondrian_playback::FrameExecutionCancellation::Superseded {
                    age: Some(Duration::ZERO),
                }),
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::ScrubCursor,
                Duration::ZERO,
                false,
            ),
            Some(MediaPreviewCancelReason::Obsolete),
        );
        assert_eq!(
            media_preview_cancel_reason(
                Some(mondrian_playback::FrameExecutionCancellation::Superseded {
                    age: Some(Duration::ZERO),
                }),
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
                Some(
                    mondrian_playback::FrameExecutionCancellation::DeadlineExpired {
                        age: Duration::ZERO,
                    },
                ),
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::PlaybackCursor,
                Duration::ZERO,
                true,
            ),
            Some(MediaPreviewCancelReason::PlaybackDeadline),
        );
        assert_eq!(
            media_preview_cancel_reason(
                None,
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::PlaybackCursor,
                Duration::ZERO,
                true,
            ),
            None,
        );
        assert_eq!(
            media_preview_cancel_reason(
                Some(
                    mondrian_playback::FrameExecutionCancellation::DeadlineExpired {
                        age: Duration::ZERO,
                    },
                ),
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::ScrubCursor,
                Duration::ZERO,
                true,
            ),
            Some(MediaPreviewCancelReason::Unknown),
        );
    }
}
