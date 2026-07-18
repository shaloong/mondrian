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
    PreviewDecodeOutcome, PreviewDecodePath, PreviewDecodeRequest, PreviewDecodeSeekStrategy,
    PreviewDecodeStageDurations, PreviewDecodeThreadingKind, PreviewFileFingerprint,
    PreviewHardwareDecodeBlocker, PreviewHardwareDecodeCpuTransferStatus,
    PreviewHardwareDecodeDecision, PreviewHardwareDecodeRequest, PreviewNativeDecodedFrame,
    PreviewScrubAdaptiveClass, PreviewSeekIndexSource, PreviewSourceColorContract,
    VideoColorDiagnostic, VideoColorDiagnosticIssueSummary,
};
#[cfg(test)]
use mondrian_media::{
    DecodedVideoChromaLocation, PreviewDecodeExecutionPath, PreviewNativeDecodedFrameHandle,
};
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
use crate::app::preview_execution::{PreviewCandidateDecision, PreviewExecutionCoordinator};
use crate::app::preview_execution::{
    PreviewDecodeExecutionSummary as AppUiPreviewDecodeExecutionSummary,
    PreviewGpuFrame as AppUiGpuPreviewFrame, PreviewGpuFrameState as AppUiGpuPreviewFrameState,
    PreviewGpuWorkingInput as AppUiGpuPreviewWorkingInput,
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
    execution: RefCell<
        PreviewExecutionCoordinator<
            ViewerPreviewGenerationKey,
            ViewerPreviewCacheKey,
            ViewerExternalTextureFrame,
        >,
    >,
    playback_pressure: Cell<PlaybackPressureState>,
    scheduler: MediaPreviewScheduler,
    scratch: RefCell<TimelineCompositeScratch>,
    last_color_rejection: RefCell<Option<AppUiPreviewColorRejection>>,
    display_snapshot: RefCell<Option<DisplayOutputSnapshot>>,
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
            execution: RefCell::new(PreviewExecutionCoordinator::default()),
            playback_pressure: Cell::new(PlaybackPressureState::default()),
            scheduler,
            scratch: RefCell::new(TimelineCompositeScratch::default()),
            last_color_rejection: RefCell::new(None),
            display_snapshot: RefCell::new(None),
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
        self.execution.borrow_mut().invalidate(|| generation);
        self.execution.borrow_mut().set_pending(true);
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

    /// Build a CPU working-space preview frame suitable for the app-window GPU output boundary.
    ///
    /// The returned frame is not display encoded. The app window owns wgpu recording,
    /// output texture registration, and texture lifetime.
    pub(crate) fn gpu_preview_frame_for_state(
        &self,
        state: &AppState,
    ) -> AppUiGpuPreviewFrameState {
        bump(&self.metrics.gpu_preview_candidate_requests);
        self.execution.borrow_mut().set_pending(false);
        self.last_color_rejection.replace(None);
        let Some(sequence) = state.sequence.as_ref() else {
            self.invalidate_preview_generation();
            self.scheduler.prune_obsolete();
            self.execution.borrow_mut().clear_output();
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
                self.execution.borrow_mut().clear_output();
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
            self.execution.borrow_mut().clear_output();
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
                self.execution.borrow_mut().clear_output();
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
            None => {
                let decision = self.execution.borrow_mut().plan_candidate(None);
                self.schedule_media_prefetches(state, sequence, frame, width, height);
                self.scheduler.prune_obsolete();
                return match decision {
                    PreviewCandidateDecision::Loading => {
                        bump(&self.metrics.gpu_preview_candidate_loading);
                        AppUiGpuPreviewFrameState::Loading
                    }
                    PreviewCandidateDecision::Unavailable => {
                        self.execution.borrow_mut().clear_output();
                        bump(&self.metrics.gpu_preview_candidate_unavailable);
                        AppUiGpuPreviewFrameState::Unavailable
                    }
                    PreviewCandidateDecision::Current | PreviewCandidateDecision::Execute(_) => {
                        unreachable!("unresolved preview intent cannot select an output")
                    }
                };
            }
        };
        if let Some(cache_key) = resolved.cache_key.as_mut() {
            *cache_key = cache_key.with_monitor_adaptation(&monitor_adaptation);
        }
        self.execution
            .borrow_mut()
            .set_presentation_quality(resolved_preview_presentation_quality(&resolved.elements));
        let Some(cache_key) = resolved.cache_key.clone() else {
            self.scheduler.prune_obsolete();
            self.execution.borrow_mut().clear_output();
            bump(&self.metrics.gpu_preview_candidate_unavailable);
            return AppUiGpuPreviewFrameState::Unavailable;
        };
        let candidate_id = match self.execution.borrow_mut().plan_candidate(Some(&cache_key)) {
            PreviewCandidateDecision::Current => {
                self.schedule_media_prefetches(state, sequence, frame, width, height);
                self.scheduler.prune_obsolete();
                bump(&self.metrics.gpu_preview_candidate_current);
                return AppUiGpuPreviewFrameState::Current;
            }
            PreviewCandidateDecision::Execute(candidate_id) => candidate_id,
            PreviewCandidateDecision::Loading | PreviewCandidateDecision::Unavailable => {
                unreachable!("resolved preview key must be current or executable")
            }
        };
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
        AppUiGpuPreviewFrameState::Ready(Box::new(AppUiGpuPreviewFrame::new(
            cache_key,
            sequence.id,
            frame,
            width,
            height,
            resolved.color_context.working_color_space,
            working_input,
            program_output_boundary,
            monitor_adaptation,
            candidate_id,
            self.playback_presentation_ticket(state),
            decode_execution,
        )))
    }

    /// Build the exact ticket that a Presentation Adapter may complete only
    /// after it makes the current output usable.
    pub(crate) fn playback_presentation_ticket(
        &self,
        state: &AppState,
    ) -> Option<mondrian_playback::FramePresentationTicket> {
        state.playback_frame_presentation_ticket(self.execution.borrow().presentation_quality())
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
        self.execution.borrow_mut().register_output(frame.output_key.clone(), content);
        bump(&self.metrics.gpu_preview_external_frames_registered);
        true
    }

    /// Clear any external GPU viewer frame currently advertised by the service.
    pub(crate) fn clear_external_viewer_frame(&self) {
        self.execution.borrow_mut().clear_output();
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
        self.execution.borrow_mut().clear_output();
        self.invalidate_preview_generation();
        self.scheduler.prune_obsolete();
    }

    fn activate_preview_generation(&self, key: ViewerPreviewGenerationKey) -> u64 {
        self.execution
            .borrow_mut()
            .bind_generation(key, || self.scheduler.begin_generation())
            .generation()
    }

    fn invalidate_preview_generation(&self) {
        self.execution.borrow_mut().invalidate(|| self.scheduler.begin_generation());
    }

    fn external_viewer_frame_for_key(
        &self,
        key: &ViewerPreviewCacheKey,
    ) -> Option<ViewerExternalTextureFrame> {
        self.execution.borrow().output_for(key)
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

mod composite;
use composite::*;
mod diagnostics;
pub use diagnostics::*;
mod media_adapter;
use media_adapter::PreviewProxyGenerationRequestKey;
#[cfg(test)]
use media_adapter::{
    resolve_preview_media_decode_path, should_request_preview_proxy_generation, source_micros,
    PreviewMediaDecodePathResolution,
};
mod media_frame;
pub(crate) use media_frame::MediaPreviewFrame;
use media_frame::{
    project_preview_media_transform, MediaPreviewGpuSourceFrame, MediaPreviewNativeSourceFrame,
};
mod presentation;
mod result_pump;
mod service_lifecycle;
mod timeline_evaluation;
mod viewer_plan;

use crate::app::preview_execution::PreviewOutputKey as ViewerPreviewCacheKey;
use viewer_plan::{
    gpu_composite_layers_for_resolved, preview_elements_require_deferred_composite,
    resolved_preview_decode_execution, resolved_preview_presentation_quality,
    uncached_viewer_raster_frame_key, viewer_preview_cache_key_for_resolved_plan,
    viewer_raster_frame_key, ResolvedPreviewElement,
};
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
pub(crate) struct ScopedViewerFrame {
    pub(crate) sequence_id: SequenceId,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) frame: ViewerFrameImage,
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
        if self.execution.borrow().is_pending() {
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
        let generation = self.execution.borrow().generation();
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
#[path = "preview/tests.rs"]
mod tests;
