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
use std::path::PathBuf;
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use mondrian_assets::AssetKind;
use mondrian_core::display_contract::{DisplayOutputSnapshot, MonitorProfileStatus};
use mondrian_core::timeline_data::{AlphaInterpretation, AssetMediaInterpretation};
use mondrian_core::types::{AssetId, ColorSpace, Rational, SequenceId};
#[cfg(test)]
use mondrian_core::types::{BlendMode, ColorEngine};
use mondrian_core::{Resolution, WorkingColorSpace};
use mondrian_media::{
    preview_decode_cpu_budget, DecodedFrameResidency, DecodedVideoSurfaceFormat, HwAccelBackend,
    HwAccelDeviceSelector, PreviewDecodeAccessMode, PreviewDecodeAdaptiveHints,
    PreviewDecodeCpuBudget, PreviewDecodeDiagnostics, PreviewDecodePath, PreviewDecodeSeekStrategy,
    PreviewDecodeStageDurations, PreviewDecodeThreadingKind, PreviewHardwareDecodeBlocker,
    PreviewHardwareDecodeCpuTransferStatus, PreviewHardwareDecodeDecision,
    PreviewHardwareDecodeRequest, PreviewScrubAdaptiveClass, PreviewSeekIndexSource,
    VideoColorDiagnosticIssueSummary,
};
#[cfg(test)]
use mondrian_media::{
    DecodedGpuFrameHandleKind, DecodedVideoChromaLocation, DecodedVideoRange,
    DecodedVideoRangeContract, DecodedVideoSampling, PreviewDecodeExecutionPath,
    PreviewFileFingerprint, PreviewNativeDecodedFrame, PreviewNativeDecodedFrameHandle,
    VideoColorDiagnostic,
};
use mondrian_renderer::{
    color_report_vocab, evaluate_timeline_render_plan, execute_cpu_working_transform,
    CpuColorFrame, GpuCompositingDiagnostics, RenderColorStageDiagnostics,
    RenderColorStageGpuBlockerBreakdown, RenderColorTransformDiagnostics,
    RenderColorTransformDirection, RenderMonitorAdaptation, TimelineAdjustmentLayer,
    TimelineCompositeColorPathSummary, TimelineCompositeDiagnostics,
    TimelineCompositeDomainBlockerBreakdown, TimelineCompositeLegacyBreakdown,
    TimelineCompositeScratch, TimelineEvaluationRequest, TimelineRenderPlanElement,
    TimelineSolidColorLayer,
};
#[cfg(test)]
use mondrian_renderer::{
    execute_cpu_input_stage, CpuEncodedColorFrame, CpuSourceColorFrame,
    GpuCompositingBlockerReason, GpuNativeDecodedFrameImportSource,
    GpuNativeDecodedFrameTextureFormat, LinearFloatSource, RenderInputTransform,
    RenderOutputColorBoundary, TimelineCompositeColorPath, TimelineCompositeElement,
    TimelineCompositeOptions, TimelineEffectColorRuntime, TimelineMediaLayer,
    ViewerGpuExecutionLayer,
};
#[cfg(test)]
use mondrian_timeline::sequence::ResolvedInputColor;
use mondrian_timeline::sequence::{
    ColorContext, InputColorResolution, InputColorResolutionSource,
    InputColorResolutionSourceCounts, MissingColorMetadataPolicy, Sequence,
    MAX_NESTED_SEQUENCE_RENDER_DEPTH,
};
use mondrian_ui_widgets::{
    ViewerExternalTextureFrame, ViewerExternalTexturePresentation, ViewerFrameContent,
    ViewerFrameImage,
};

use crate::app::native_video_import::PlaybackHardwareDecodeAdmission;
#[cfg(test)]
use crate::app::native_video_import::PreviewHardwareDecodeAdmissionBlocker;
use crate::app::playback_preview::{PlaybackPreviewAdapter, PreviewVideoPreroll, PreviewWorkPoll};
use crate::app::preview_access_mode::{
    media_preview_access_mode_for_intent, media_preview_frame_work_class,
    media_preview_viewer_access_intent, media_preview_worker_count, media_preview_worker_lane,
    MediaPreviewCancelReason, MediaPreviewJob, MediaPreviewJobQueueDiagnostics,
    MediaPreviewJobQueueSender, MediaPreviewKey, MediaPreviewRequestPriority,
    MediaPreviewRequestStatus, MediaPreviewScheduler, MediaPreviewSchedulerDiagnostics,
};
#[cfg(test)]
use crate::app::preview_access_mode::{
    media_preview_cancel_reason, media_preview_cancel_reason_at_checkpoint,
    media_preview_cancel_request_to_observed_us, MediaPreviewJobEnqueueStatus,
    MediaPreviewNativeSurfaceHint, MediaPreviewWorkerLane, MEDIA_PREVIEW_PREFETCH_DECODE_BUDGET_US,
};
use crate::app::preview_cpu_execution::{
    composite_resolved_preview, composite_resolved_preview_working,
    output_boundary_from_color_context, PreviewCompositeOutput, PreviewCpuExecutionDurations,
};
#[cfg(test)]
use crate::app::preview_execution::PreviewDecodeExecutionSummary as AppUiPreviewDecodeExecutionSummary;
use crate::app::preview_execution::{PreviewCandidateDecision, PreviewExecutionCoordinator};
use crate::app::preview_execution::{
    PreviewGpuFrame as AppUiGpuPreviewFrame, PreviewGpuFrameState as AppUiGpuPreviewFrameState,
    PreviewGpuWorkingInput as AppUiGpuPreviewWorkingInput,
};
use crate::app::preview_frame_store::PreviewFrameStoreAdapter;
#[cfg(test)]
use crate::app::preview_frame_store::PreviewFrameStoreAdapterConfig;
use crate::app::preview_frame_store::ScopedPreviewRasterFrame;
use crate::app::preview_gpu_output_blocker::PreviewGpuOutputBlocker;
use crate::app::preview_hardware_admission::PreviewHardwareDecodeAdmissionState;
use crate::app::preview_media_frame::{project_preview_media_transform, MediaPreviewFrame};
#[cfg(test)]
use crate::app::preview_media_frame::{MediaPreviewGpuSourceFrame, MediaPreviewNativeSourceFrame};
use crate::app::preview_media_source::{
    resolve_preview_input_color_space, PreviewProxyGenerationRequestKey,
};
#[cfg(test)]
use crate::app::preview_media_task::{
    media_preview_canceled_result, MediaPreviewCancellationPhase,
};
use crate::app::preview_media_task::{
    media_preview_worker, MediaPreviewResult, PreviewShutdownSignal,
};
use crate::app::preview_quality::normalize_preview_resolution_scale;
use crate::app::preview_raster_frame::{
    preview_raster_presentation_contract, preview_raster_resource_key,
    uncached_preview_raster_resource_key, PreviewRasterColorSpace, PreviewRasterFrame,
};
use crate::app::preview_scheduler_policy::{
    media_preview_forward_prefetch_window_frames, playback_frame_delivery_kind,
    playback_hardware_recovery_signals, MediaPreviewFailureReason, PlaybackDecodeExecution,
    PlaybackPressureState, PlaybackPressureTransition, PreviewScrubAdaptationState,
    MEDIA_PREVIEW_FORWARD_PREFETCH_HORIZON_US, MEDIA_PREVIEW_FORWARD_PREFETCH_MAX_FRAMES,
    MEDIA_PREVIEW_FORWARD_PREFETCH_MIN_FRAMES,
};
#[cfg(test)]
use crate::app::preview_scheduler_policy::{
    preview_decode_presentation_quality, MEDIA_PREVIEW_PLAYBACK_PRESSURE_LATE_STREAK_THRESHOLD,
    PREVIEW_SCRUB_SLOW_LATENCY_US,
};
use crate::app::preview_viewer_plan::{
    gpu_composite_layers_for_resolved, preview_elements_require_deferred_composite,
    resolved_preview_decode_execution, resolved_preview_presentation_quality,
    viewer_preview_cache_key_for_resolved_plan, ResolvedPreviewElement,
};
use crate::app::proxy_generation::{request_proxy_generation, resolve_asset_proxy_color_contract};
use crate::app::AppState;
use crate::app_ui::panels::{
    ViewerColorPipelineStatus, ViewerPreviewColorRejectionModel, ViewerPreviewSource,
    ViewerPreviewState,
};

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
    frame_store: RefCell<PreviewFrameStoreAdapter>,
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
    hardware_decode_admission: Cell<PreviewHardwareDecodeAdmissionState>,
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
            frame_store: RefCell::new(PreviewFrameStoreAdapter::default()),
            requested_proxy_generations: RefCell::new(HashSet::new()),
            scrub_adaptation: RefCell::new(PreviewScrubAdaptationState::default()),
            execution: RefCell::new(PreviewExecutionCoordinator::default()),
            playback_pressure: Cell::new(PlaybackPressureState::default()),
            scheduler,
            scratch: RefCell::new(TimelineCompositeScratch::default()),
            last_color_rejection: RefCell::new(None),
            display_snapshot: RefCell::new(None),
            hardware_decode_admission: Cell::new(PreviewHardwareDecodeAdmissionState::default()),
            decode_cpu_budget,
            decode_worker_count,
            metrics: AppUiPreviewMetrics::default(),
        }
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

    fn stale_frame_for_sequence(
        &self,
        sequence: &Sequence,
        width: u32,
        height: u32,
    ) -> Option<PreviewRasterFrame> {
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
mod evidence;
mod hardware_admission;
mod media_adapter;
mod presentation;
mod request_scheduler;
mod result_pump;
mod service_lifecycle;
mod timeline_evaluation;

use crate::app::preview_execution::PreviewOutputKey as ViewerPreviewCacheKey;
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

impl AppUiPreviewService {
    fn record_color_rejection(&self, rejection: AppUiPreviewColorRejection) {
        self.last_color_rejection.replace(Some(rejection));
    }
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
        RefCell<crate::app::preview_gpu_output_blocker::PreviewGpuOutputBlockerBreakdown>,
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
) -> Result<ColorSpace, crate::app::preview_gpu_output_blocker::PreviewGpuOutputBlocker> {
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
) -> Result<ColorSpace, crate::app::preview_gpu_output_blocker::PreviewGpuOutputBlocker> {
    let Some(snapshot) = display_snapshot else {
        return Err(
            crate::app::preview_gpu_output_blocker::PreviewGpuOutputBlocker::UnsupportedFeature {
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
                crate::app::preview_gpu_output_blocker::PreviewGpuOutputBlocker::UnsupportedFeature {
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
            crate::app::preview_gpu_output_blocker::PreviewGpuOutputBlocker::UnsupportedFeature {
                feature: "icc_monitor_calibration_processor".to_owned(),
                reason: "OS ICC profile was marked managed without a renderer calibration processor; preview refuses the uncalibrated output".to_owned(),
            },
        ),
        ref status => Err(
            crate::app::preview_gpu_output_blocker::PreviewGpuOutputBlocker::UnsupportedFeature {
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

#[cfg(test)]
#[path = "preview/tests.rs"]
mod tests;
