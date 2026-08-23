//! UI-independent production Preview composition root.
//!
//! The Runtime composes Timeline execution, media-source/task, Viewer-plan,
//! CPU/GPU execution, asset-library/proxy side effects, diagnostics, and final
//! application presentation state. Window and Headless Adapters only register
//! their usable GPU output and project the resulting state.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::hash::Hash;
#[cfg(test)]
use std::path::PathBuf;
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::app::native_video_import::PlaybackHardwareDecodeAdmission;
#[cfg(test)]
use crate::app::native_video_import::PreviewHardwareDecodeAdmissionBlocker;
use crate::app::playback_preview::{
    PlaybackPreviewAdapter, PreviewTransportIntent, PreviewVideoPreroll, PreviewWorkPoll,
};
use crate::app::preview_access_mode::{
    media_preview_access_mode_for_intent, media_preview_frame_work_class,
    media_preview_viewer_access_intent, media_preview_worker_count, media_preview_worker_lane,
    MediaPreviewCancelReason, MediaPreviewExistingWorkBinding, MediaPreviewJob,
    MediaPreviewJobQueueDiagnostics, MediaPreviewJobQueueSender, MediaPreviewKey,
    MediaPreviewRequestPriority, MediaPreviewRequestStatus, MediaPreviewScheduler,
    MediaPreviewSchedulerDiagnostics, MediaPreviewWorkerLane,
};
#[cfg(test)]
use crate::app::preview_access_mode::{
    media_preview_cancel_reason_at_logical_observation,
    media_preview_cancel_reason_for_test_observation,
    media_preview_cancel_request_to_logical_observation_us, MediaPreviewJobEnqueueStatus,
    MEDIA_PREVIEW_PREFETCH_DECODE_BUDGET_US,
};
#[cfg(test)]
use crate::app::preview_cpu_execution::composite_resolved_preview_working;
use crate::app::preview_cpu_execution::{
    composite_resolved_preview, output_boundary_from_color_context, PreviewCompositeOutput,
    PreviewCpuExecutionDurations,
};
use crate::app::preview_cpu_fallback_task::{
    PreviewCpuFallbackRequest, PreviewCpuFallbackResult, PreviewCpuFallbackSubmission,
    PreviewCpuFallbackTask,
};
use crate::app::preview_decode_residency::{
    PreviewDecodeResidencyCoordinator, PreviewDecodeResidencyFamily,
};
use crate::app::preview_display_contract::preview_blockers_from_snapshot;
#[cfg(test)]
use crate::app::preview_execution::PreviewDecodeExecutionSummary;
#[cfg(any(test, feature = "validation"))]
use crate::app::preview_execution::PreviewPlaybackIntent;
use crate::app::preview_execution::{
    PreviewCandidateDecision, PreviewExecutionCoordinator, PreviewGenerationBinding,
    PreviewPreparedPromotion, PreviewSemanticIdentityBuilder,
};
use crate::app::preview_execution::{
    PreviewGpuFrame, PreviewGpuFrameState, PreviewGpuHeterogeneousCompletionError,
    PreviewGpuHeterogeneousExecution, PreviewGpuWorkingInput, PreviewOutputKey,
};
use crate::app::preview_frame_store::PreviewFrameStoreAdapter;
#[cfg(test)]
use crate::app::preview_frame_store::PreviewFrameStoreAdapterConfig;
use crate::app::preview_frame_store::ScopedPreviewRasterFrame;
use crate::app::preview_gpu_output_blocker::PreviewGpuOutputBlocker;
use crate::app::preview_hardware_admission::PreviewHardwareDecodeAdmissionState;
use crate::app::preview_media_frame::MediaPreviewFrame;
#[cfg(test)]
use crate::app::preview_media_frame::{
    project_preview_media_transform, MediaPreviewGpuSourceFrame, MediaPreviewNativeSourceFrame,
};
use crate::app::preview_media_source::resolve_preview_input_color_space;
#[cfg(test)]
use crate::app::preview_media_task::{
    media_preview_canceled_result, MediaPreviewCancellationPhase, MediaPreviewQueueDisposition,
};
use crate::app::preview_media_task::{
    media_preview_worker, LogicalCancellationObserved, MediaPreviewResult, PreviewShutdownSignal,
};
#[cfg(test)]
use crate::app::preview_quality::normalize_preview_resolution_scale;
use crate::app::preview_quality::preview_execution_resolution;
use crate::app::preview_raster_frame::{
    preview_raster_presentation_contract, preview_raster_resource_key, PreviewRasterFrame,
};
use crate::app::preview_scheduler_policy::{
    media_preview_forward_prefetch_window_frames, media_preview_residency_reservation,
    playback_frame_delivery_kind, playback_hardware_recovery_signals, MediaPreviewFailureReason,
    MediaPreviewResidencyReservation, PlaybackDecodeExecution, PlaybackPressureState,
    PlaybackPressureTransition, PreviewScrubAdaptationState,
    MEDIA_PREVIEW_FORWARD_PREFETCH_HORIZON_US, MEDIA_PREVIEW_FORWARD_PREFETCH_MAX_FRAMES,
    MEDIA_PREVIEW_FORWARD_PREFETCH_MIN_FRAMES,
};
#[cfg(test)]
use crate::app::preview_scheduler_policy::{
    preview_decode_presentation_quality, MEDIA_PREVIEW_PLAYBACK_PRESSURE_LATE_STREAK_THRESHOLD,
    PREVIEW_SCRUB_SLOW_LATENCY_US,
};
use crate::app::preview_title_task::PreviewTitleTask;
use crate::app::preview_unavailability::{
    PreviewOutputStage, PreviewUnavailability, PreviewUnavailabilityEvidence,
};
#[cfg(test)]
use crate::app::preview_viewer_plan::gpu_composite_layers_for_resolved;
#[cfg(test)]
use crate::app::preview_viewer_plan::viewer_preview_cache_key_for_resolved_plan;
use crate::app::preview_viewer_plan::{
    prepare_gpu_composite_layers_with_heterogeneous_effects, resolved_preview_decode_execution,
    resolved_preview_media_protections, resolved_preview_presentation_quality,
    PreparedPreviewViewerGpuLayers, PreviewViewerGpuLayerPreparationError, ResolvedPreviewElement,
};
use crate::app::preview_visual_dependencies::PreviewVisualDependencyObserver;
use crate::app::preview_visual_execution_task::{
    VisualExecutionAdmission, VisualExecutionFailed, VisualExecutionGpuFailure,
    VisualExecutionPrefixReady, VisualExecutionTask, VisualExecutionTaskConfig,
    VisualExecutionTaskFailure, VisualExecutionTaskKey, VisualExecutionTaskPayload,
    VisualExecutionTaskPoll, VisualExecutionTaskResult,
};
use crate::app::preview_work_notification::{
    preview_work_notification_channel, PreviewWorkNotifier, PreviewWorkWatch,
};
use crate::app::proxy_generation::{
    resolve_asset_proxy_color_contract, ProxyGenerationRequestOutcome,
};
use mondrian_assets::AssetKind;
use mondrian_core::display_contract::{
    DisplayOutputIdentity, DisplayOutputSnapshot, MonitorProfileStatus,
};
use mondrian_core::timeline_data::{AlphaInterpretation, AssetMediaInterpretation};
use mondrian_core::types::{AssetId, ColorSpace, SequenceId};
#[cfg(test)]
use mondrian_core::types::{BlendMode, ColorEngine, Rational};
use mondrian_core::{Resolution, WorkingColorSpace};
use mondrian_editor_state::AuthoringSessionId;
use mondrian_effects::EffectExecutionSessionConfig;
use mondrian_media::{
    preview_decode_cpu_budget, DecodedFrameResidency, DecodedVideoSurfaceFormat, HwAccelBackend,
    HwAccelDeviceSelector, PreviewDecodeAccessMode, PreviewDecodeAdaptiveHints,
    PreviewDecodeCpuBudget, PreviewDecodeDiagnostics, PreviewDecodeExecutionObserver,
    PreviewDecodePath, PreviewDecodeSeekStrategy, PreviewDecodeSessionDisposition,
    PreviewDecodeStageDurations, PreviewDecodeThreadingKind, PreviewHardwareDecodeBlocker,
    PreviewHardwareDecodeCpuTransferStatus, PreviewHardwareDecodeDecision,
    PreviewHardwareDecodeRequest, PreviewScrubAdaptiveClass, PreviewSeekIndexSource,
    VideoColorDiagnosticIssueSummary,
};
#[cfg(test)]
use mondrian_media::{
    DecodedGpuFrameHandleKind, DecodedVideoChromaLocation, DecodedVideoRange,
    DecodedVideoRangeContract, DecodedVideoSampling, MediaFileFingerprint,
    PreviewDecodeExecutionPath, PreviewNativeDecodedFrame, PreviewNativeDecodedFrameHandle,
    PreviewTemporalExtentSource, VideoColorDiagnostic,
};
use mondrian_renderer::{
    color_report_vocab, GpuCompositingDiagnostics, PreparedVisualProgramCacheDiagnostics,
    RenderColorStageDiagnostics, RenderColorStageGpuBlockerBreakdown,
    RenderColorTransformDiagnostics, RenderColorTransformDirection, RenderMonitorAdaptation,
    TimelineCompositeColorPathSummary, TimelineCompositeDiagnostics,
    TimelineCompositeDomainBlockerBreakdown, TimelineCompositeLegacyBreakdown,
    TimelineCompositeScratch, ViewerHeterogeneousGpuInput,
};
#[cfg(test)]
use mondrian_renderer::{
    evaluate_prepared_visual_program, execute_cpu_input_stage, CpuEncodedColorFrame,
    CpuSourceColorFrame, GpuCompositingBlockerReason, GpuNativeDecodedFrameImportSource,
    GpuNativeDecodedFrameTextureFormat, LinearFloatSource, PreparedVisualProgram,
    RenderInputTransform, RenderOutputColorBoundary, TimelineAdjustmentLayer,
    TimelineCompositeColorPath, TimelineCompositeElement, TimelineCompositeOptions,
    TimelineEffectColorRuntime, TimelineEvaluationRequest, TimelineMediaLayer,
    TimelineRenderPlanElement, TimelineSolidColorLayer, ViewerGpuExecutionLayer,
};
#[cfg(test)]
use mondrian_timeline::sequence::ResolvedInputColor;
use mondrian_timeline::sequence::{
    InputColorResolution, InputColorResolutionSource, InputColorResolutionSourceCounts,
    MediaInputColorContext, MissingColorMetadataPolicy, ProgramColorContext, Sequence,
};
const MEDIA_PREVIEW_PLAYBACK_BUFFERING_STALL_TIMEOUT_US: u64 = 250_000;
const MEDIA_PREVIEW_MAX_COMPLETED_RESULTS_PER_POLL: usize = 8;
// Keep decoded payload ownership bounded independently of foreground polling.
// At most one additional result may be held by each of the two decode workers
// while this queue is full; native decoder pool headroom accounts for both.
const MEDIA_PREVIEW_COMPLETED_RESULT_QUEUE_CAPACITY: usize =
    MEDIA_PREVIEW_MAX_COMPLETED_RESULTS_PER_POLL;
const MEDIA_PREVIEW_COMPLETED_RESULTS_POLL_BUDGET_US: u64 = 2_000;

#[cfg(test)]
/// Return the exact production stall threshold to deterministic integration tests.
pub(crate) const fn playback_buffering_stall_timeout_for_test() -> Duration {
    Duration::from_micros(MEDIA_PREVIEW_PLAYBACK_BUFFERING_STALL_TIMEOUT_US)
}

/// UI-independent content made usable by Preview production execution.
#[derive(Debug, Clone)]
pub(crate) enum PreviewPresentationContent<O> {
    /// GPU output published by the active presentation Adapter.
    Gpu(O),
    /// Validated final CPU raster owned by the App preview contract.
    Raster(PreviewRasterFrame),
}

/// Exact presentable value bound to the Frame Demand sampled during evaluation.
///
/// The ticket travels with the value so a later Window or Headless Adapter
/// cannot attach an older candidate to whichever demand happens to be current
/// when UI feedback is synchronized.
#[derive(Debug, Clone)]
pub(crate) struct PreviewPresentationCandidate<T> {
    value: T,
    presentation_ticket: Option<mondrian_playback::FramePresentationTicket>,
    already_visible: bool,
}

impl<T> PreviewPresentationCandidate<T> {
    pub(crate) fn new(
        value: T,
        presentation_ticket: Option<mondrian_playback::FramePresentationTicket>,
    ) -> Self {
        Self { value, presentation_ticket, already_visible: false }
    }

    /// Bind a candidate whose exact physical artifact was already visible
    /// before the current transport coordinate became active.
    pub(crate) fn already_visible(
        value: T,
        presentation_ticket: Option<mondrian_playback::FramePresentationTicket>,
    ) -> Self {
        Self { value, presentation_ticket, already_visible: true }
    }

    /// Exact terminal authority captured with this candidate.
    pub(crate) const fn presentation_ticket(
        &self,
    ) -> Option<mondrian_playback::FramePresentationTicket> {
        self.presentation_ticket
    }

    /// Whether publication only needs to synchronize semantic ownership.
    pub(crate) const fn was_already_visible(&self) -> bool {
        self.already_visible
    }

    /// Consume the candidate after presentation arbitration.
    pub(crate) fn into_value(self) -> T {
        self.value
    }
}

/// UI-independent terminal state of one Preview presentation request.
#[derive(Debug, Clone)]
pub(crate) enum PreviewPresentationState<O> {
    /// Exact current output is usable after its bound ticket is accepted.
    Ready(PreviewPresentationCandidate<PreviewPresentationContent<O>>),
    /// Exact current output is the semantic transparent canvas after its bound
    /// ticket is accepted.
    Transparent(PreviewPresentationCandidate<()>),
    /// Required production work remains pending.
    Loading,
    /// Same-scope prior output is explicitly reusable while work is pending.
    Stale(PreviewPresentationContent<O>),
    /// Current intent cannot produce a valid output.
    Unavailable(PreviewUnavailability),
}

/// Publication decision after the exact heterogeneous Viewer submission
/// reached real GPU completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreviewVisualGpuCompletionDisposition {
    /// The completion is current, on time, and may become visible.
    PublishCurrent,
    /// The artifact is cache-only, stale, or no longer belongs to this
    /// Preview lifecycle. Adapter resources must be released without
    /// publication.
    Release,
    /// Playback's exact current demand has a terminal outcome whose completion
    /// timestamp must be bound by the App consumption seam.
    TerminalCandidate(mondrian_playback::FrameDeliveryCandidate),
}

/// Production Preview composition root shared by Window and Headless Adapters.
///
/// `O` is the concrete usable GPU output published by the active presentation
/// Adapter. The runtime treats it as an opaque payload and owns only its exact
/// resolved-output lifetime.
pub struct PreviewProductionRuntime<O: Clone> {
    work_notifier: PreviewWorkNotifier,
    work_watch: PreviewWorkWatch,
    jobs: MediaPreviewJobQueueSender,
    results: RefCell<mpsc::Receiver<MediaPreviewResult>>,
    workers: RefCell<Vec<JoinHandle<()>>>,
    shutdown: Arc<PreviewShutdownSignal>,
    decode_residency: Arc<PreviewDecodeResidencyCoordinator>,
    observed_decode_residency_retry_revision: Cell<u64>,
    /// Exact access family whose current candidate was denied by the decoder
    /// residency retirement barrier.
    decode_residency_waiting: Cell<Option<PreviewDecodeAccessMode>>,
    decode_worker_resources: mondrian_media::PreviewDecodeWorkerResources,
    frame_store: RefCell<PreviewFrameStoreAdapter>,
    /// One current Viewer candidate is waiting for aggregate media capacity.
    ///
    /// The bit is consumed only after a worker result releases physical work
    /// ownership, and is cleared at every transport-generation boundary.
    media_aggregate_capacity_waiting: Cell<bool>,
    /// Exact current media bindings that reused already-owned Broker work.
    ///
    /// Entries remain until their queued/in-flight owner settles. The next
    /// work poll then publishes one retry edge for the whole Viewer candidate.
    media_existing_work_waiters: RefCell<HashMap<MediaPreviewKey, MediaPreviewExistingWorkBinding>>,
    /// Retry request retained until a consumer actually enters candidate evaluation.
    media_existing_work_retry_pending: Cell<bool>,
    media_existing_work_waiter_registrations: Cell<u64>,
    media_existing_work_retry_acknowledgements: Cell<u64>,
    scrub_adaptation: RefCell<PreviewScrubAdaptationState>,
    execution:
        RefCell<PreviewExecutionCoordinator<ViewerPreviewGenerationKey, ViewerPreviewCacheKey, O>>,
    transport_playing: Cell<bool>,
    transport_epoch: Cell<Option<mondrian_playback::PlaybackEpoch>>,
    playback_pressure: Cell<PlaybackPressureState>,
    applied_resource_trim: Cell<crate::app::execution_resource_coordination::ResourceTrimRequest>,
    heterogeneous_effect_decision: Cell<
        crate::app::execution_resource_coordination::PreviewHeterogeneousEffectExecutionDecision,
    >,
    scheduler: MediaPreviewScheduler,
    title_task: RefCell<PreviewTitleTask>,
    visual_execution: Option<VisualExecutionTask>,
    visual_execution_start_failure: Option<String>,
    visual_execution_health_failed: Cell<bool>,
    cpu_fallback_task: Option<PreviewCpuFallbackTask>,
    cpu_fallback_start_failure: Option<String>,
    viewer_cpu_fallback_active: Cell<bool>,
    cpu_fallback_in_flight:
        RefCell<Option<(u64, mondrian_playback::PlaybackEpoch, PreviewOutputKey)>>,
    cpu_fallback_failure: RefCell<Option<(PreviewOutputKey, String)>>,
    visual_ready: RefCell<HashMap<VisualExecutionTaskKey, VisualExecutionPrefixReady>>,
    visual_failures: RefCell<HashMap<VisualExecutionTaskKey, String>>,
    media_execution_failures: RefCell<HashMap<MediaPreviewKey, (u64, MediaPreviewFailureReason)>>,
    media_worker_start_failure: Option<String>,
    media_worker_health_failed: Cell<bool>,
    last_current_media_admission: Cell<Option<&'static str>>,
    last_gpu_loading_reason: Cell<Option<&'static str>>,
    visual_terminal_candidates: RefCell<Vec<mondrian_playback::FrameDeliveryCandidate>>,
    visual_program_authoring_session: Cell<Option<AuthoringSessionId>>,
    visual_programs: RefCell<mondrian_renderer::PreparedVisualProgramCache>,
    future_media_window: RefCell<request_scheduler::FutureMediaWindowCache>,
    visual_dependencies: PreviewVisualDependencyObserver,
    visual_dependency_health_failed: Cell<bool>,
    scratch: RefCell<TimelineCompositeScratch>,
    evaluation_working_set: RefCell<EvaluationWorkingSet>,
    evaluation_working_set_clock: Cell<u64>,
    last_color_rejection: RefCell<Option<PreviewColorRejection>>,
    unavailability_evidence: RefCell<PreviewUnavailabilityEvidence>,
    display_snapshot: RefCell<Option<DisplayOutputSnapshot>>,
    hardware_decode_admission: Cell<PreviewHardwareDecodeAdmissionState>,
    decode_cpu_budget: PreviewDecodeCpuBudget,
    decode_worker_count: usize,
    decode_execution_watch: PreviewDecodeWorkerExecutionWatch,
    metrics: PreviewMetrics,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PreviewWorkerIsolation {
    RequiredPackaged,
    #[cfg(test)]
    DirectTestAdapter,
}

impl PreviewWorkerIsolation {
    const fn requires_packaged_worker(self) -> bool {
        matches!(self, Self::RequiredPackaged)
    }
}

impl<O: Clone> PreviewProductionRuntime<O> {
    /// Enter bounded CPU Viewer execution after a concrete Window GPU failure.
    /// The active semantic generation is retained; only the execution Adapter
    /// changes. Native decode admission is bypassed by media requests while
    /// this mode is active so the fallback worker always receives CPU pixels.
    pub(crate) fn request_viewer_cpu_fallback(&self, reason: impl Into<String>) {
        let reason = reason.into();
        if !self.viewer_cpu_fallback_active.replace(true) {
            tracing::warn!(%reason, "Window Viewer enabled bounded CPU fallback execution");
        } else {
            tracing::debug!(%reason, "Window Viewer CPU fallback remains active");
        }
        self.cpu_fallback_failure.borrow_mut().take();
        self.invalidate_preview_generation();
        self.clear_decoder_resource_preview_residency();
        self.execution.borrow_mut().clear_output();
        self.work_notifier.retry_became_actionable();
    }

    /// Leave CPU fallback after the Window rebuilt a healthy GPU generation.
    pub(crate) fn clear_viewer_cpu_fallback(&self) {
        if self.viewer_cpu_fallback_active.replace(false) {
            self.cpu_fallback_in_flight.borrow_mut().take();
            self.cpu_fallback_failure.borrow_mut().take();
            self.invalidate_preview_generation();
            self.work_notifier.retry_became_actionable();
        }
    }

    fn schedule_cpu_fallback(
        &self,
        generation: u64,
        epoch: mondrian_playback::PlaybackEpoch,
        resolved: &ResolvedPlanView,
        width: u32,
        height: u32,
    ) -> PreviewGpuFrameState {
        if self.frame_store.borrow_mut().viewer_frame(&resolved.cpu_cache_key).is_some() {
            return PreviewGpuFrameState::Loading;
        }
        if self
            .cpu_fallback_failure
            .borrow()
            .as_ref()
            .is_some_and(|(key, _)| key == &resolved.cpu_cache_key)
        {
            let reason = self
                .cpu_fallback_failure
                .borrow()
                .as_ref()
                .map(|(_, reason)| reason.clone())
                .unwrap_or_else(|| "Viewer CPU fallback failed".to_owned());
            return self.unavailable_gpu_candidate(PreviewUnavailability::failed(
                PreviewOutputStage::GpuComposite,
                reason,
            ));
        }
        if self.cpu_fallback_in_flight.borrow().as_ref().is_some_and(
            |(active_generation, active_epoch, key)| {
                *active_generation == generation
                    && *active_epoch == epoch
                    && key == &resolved.cpu_cache_key
            },
        ) {
            return PreviewGpuFrameState::Loading;
        }
        let Some(task) = self.cpu_fallback_task.as_ref() else {
            return self.unavailable_gpu_candidate(PreviewUnavailability::failed(
                PreviewOutputStage::GpuComposite,
                self.cpu_fallback_start_failure
                    .as_deref()
                    .unwrap_or("Preview CPU fallback worker is unavailable"),
            ));
        };
        let request = PreviewCpuFallbackRequest {
            generation,
            epoch,
            output_key: resolved.cpu_cache_key.clone(),
            width,
            height,
            elements: Arc::clone(&resolved.elements),
            color_context: resolved.color_context.clone(),
        };
        match task.submit(request) {
            PreviewCpuFallbackSubmission::Scheduled => {
                self.cpu_fallback_in_flight.borrow_mut().replace((
                    generation,
                    epoch,
                    resolved.cpu_cache_key.clone(),
                ));
                self.execution.borrow_mut().set_pending(true);
                PreviewGpuFrameState::Loading
            }
            PreviewCpuFallbackSubmission::Busy => PreviewGpuFrameState::Loading,
            PreviewCpuFallbackSubmission::Disconnected => {
                self.unavailable_gpu_candidate(PreviewUnavailability::failed(
                    PreviewOutputStage::GpuComposite,
                    "Preview CPU fallback worker disconnected",
                ))
            }
        }
    }

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

    #[cfg(test)]
    /// Create a workerless Runtime with a deterministic media-scheduler clock.
    pub(crate) fn new_without_workers_with_scheduler_clock_for_test<C>(clock: C) -> Self
    where
        C: mondrian_playback::MonotonicRuntimeClock,
    {
        Self::with_worker_count_and_scheduler(
            preview_decode_cpu_budget(),
            0,
            MediaPreviewScheduler::with_clock_for_test(clock),
            PreviewWorkerIsolation::DirectTestAdapter,
        )
    }

    fn with_worker_count(decode_cpu_budget: PreviewDecodeCpuBudget, worker_count: usize) -> Self {
        let scheduler = MediaPreviewScheduler::default();
        Self::with_worker_count_and_scheduler(
            decode_cpu_budget,
            worker_count,
            scheduler,
            PreviewWorkerIsolation::RequiredPackaged,
        )
    }

    #[cfg(test)]
    fn with_direct_worker_count_for_test(
        decode_cpu_budget: PreviewDecodeCpuBudget,
        worker_count: usize,
    ) -> Self {
        Self::with_worker_count_and_scheduler(
            decode_cpu_budget,
            worker_count,
            MediaPreviewScheduler::default(),
            PreviewWorkerIsolation::DirectTestAdapter,
        )
    }

    fn with_worker_count_and_scheduler(
        decode_cpu_budget: PreviewDecodeCpuBudget,
        worker_count: usize,
        scheduler: MediaPreviewScheduler,
        worker_isolation: PreviewWorkerIsolation,
    ) -> Self {
        let (work_notifier, work_watch) = preview_work_notification_channel();
        let (job_tx, job_rx) = scheduler.job_queue();
        let (result_tx, result_rx) =
            mpsc::sync_channel::<MediaPreviewResult>(MEDIA_PREVIEW_COMPLETED_RESULT_QUEUE_CAPACITY);
        let shutdown = Arc::new(PreviewShutdownSignal::default());
        let decode_residency = Arc::new(PreviewDecodeResidencyCoordinator::new_with_notifier(
            work_notifier.clone(),
        ));
        let decode_worker_resources = mondrian_media::PreviewDecodeWorkerResources::default();
        let (visual_execution, visual_execution_start_failure) =
            match VisualExecutionTask::new_with_notifier(
                mondrian_playback::SystemMonotonicRuntimeClock::default(),
                VisualExecutionTaskConfig::default(),
                work_notifier.clone(),
            ) {
                Ok(task) => (Some(task), None),
                Err(error) => {
                    tracing::error!("failed to start Preview visual execution worker: {error}");
                    (None, Some(error.to_string()))
                }
            };
        let (cpu_fallback_task, cpu_fallback_start_failure) =
            match PreviewCpuFallbackTask::new(work_notifier.clone()) {
                Ok(task) => (Some(task), None),
                Err(error) => {
                    tracing::error!("failed to start Preview CPU fallback worker: {error}");
                    (None, Some(error.to_string()))
                }
            };
        let mut decode_worker_count = 0;
        let mut workers = Vec::new();
        let mut decode_execution_observers = Vec::new();
        let (demux_worker_executable, mut media_worker_start_failure) = if worker_count == 0
            || !worker_isolation.requires_packaged_worker()
        {
            (None, None)
        } else {
            match super::packaged_worker::discover_preview_demux_worker() {
                Ok(executable) => (Some(executable), None),
                Err(error) => {
                    tracing::error!(
                        %error,
                        "Preview media workers were not started because required demux isolation is unavailable"
                    );
                    scheduler.close();
                    (None, Some(error.to_string()))
                }
            }
        };
        for worker_index in 0..worker_count {
            if worker_isolation.requires_packaged_worker() && demux_worker_executable.is_none() {
                break;
            }
            let worker_jobs = job_rx.clone();
            let worker_results = result_tx.clone();
            let worker_work_notifier = work_notifier.clone();
            let worker_scheduler = scheduler.clone();
            let worker_shutdown = Arc::clone(&shutdown);
            let worker_decode_residency = Arc::clone(&decode_residency);
            let worker_lane = media_preview_worker_lane(worker_index, worker_count);
            let (worker_decode_context_bootstrap, execution_observer) =
                match demux_worker_executable.clone() {
                    Some(executable) => mondrian_media::PreviewDecodeSessionContext::observed_bootstrap_with_demux_worker(executable),
                    #[cfg(test)]
                    None if worker_isolation == PreviewWorkerIsolation::DirectTestAdapter => {
                        mondrian_media::PreviewDecodeSessionContext::observed_bootstrap()
                    }
                    None => unreachable!("required packaged worker was checked before spawn"),
                };
            let worker_decode_context_bootstrap = worker_decode_context_bootstrap
                .with_worker_resources(decode_worker_resources.clone());
            decode_residency.register_worker(worker_lane);
            match std::thread::Builder::new()
                .name(format!("mondrian-preview-worker-{worker_index}"))
                .spawn(move || {
                    media_preview_worker(
                        worker_lane,
                        worker_jobs,
                        worker_results,
                        worker_work_notifier,
                        worker_scheduler,
                        worker_shutdown,
                        worker_decode_residency,
                        worker_decode_context_bootstrap,
                    )
                }) {
                Ok(handle) => {
                    workers.push(handle);
                    decode_execution_observers.push((worker_lane, execution_observer));
                    decode_worker_count += 1;
                }
                Err(err) => {
                    decode_residency.unregister_worker(worker_lane);
                    if media_worker_start_failure.is_none() {
                        media_worker_start_failure = Some(format!(
                            "failed to start Preview media worker {worker_index}: {err}"
                        ));
                    }
                    tracing::warn!(
                        worker_index,
                        "failed to start production preview worker: {err}"
                    );
                }
            }
        }
        if worker_count > 0 && decode_worker_count == 0 {
            scheduler.close();
            media_worker_start_failure.get_or_insert_with(|| {
                "no configured Preview media worker could be started".to_owned()
            });
        }

        Self {
            work_notifier: work_notifier.clone(),
            work_watch,
            jobs: job_tx,
            results: RefCell::new(result_rx),
            workers: RefCell::new(workers),
            shutdown,
            decode_residency,
            observed_decode_residency_retry_revision: Cell::new(0),
            decode_residency_waiting: Cell::new(None),
            decode_worker_resources,
            frame_store: RefCell::new(PreviewFrameStoreAdapter::default()),
            media_aggregate_capacity_waiting: Cell::new(false),
            media_existing_work_waiters: RefCell::new(HashMap::new()),
            media_existing_work_retry_pending: Cell::new(false),
            media_existing_work_waiter_registrations: Cell::new(0),
            media_existing_work_retry_acknowledgements: Cell::new(0),
            scrub_adaptation: RefCell::new(PreviewScrubAdaptationState::default()),
            execution: RefCell::new(PreviewExecutionCoordinator::default()),
            transport_playing: Cell::new(false),
            transport_epoch: Cell::new(None),
            playback_pressure: Cell::new(PlaybackPressureState::default()),
            applied_resource_trim: Cell::new(
                crate::app::execution_resource_coordination::ResourceTrimRequest::None,
            ),
            heterogeneous_effect_decision: Cell::new(
                crate::app::execution_resource_coordination::PreviewHeterogeneousEffectExecutionDecision::conservative_baseline(),
            ),
            scheduler,
            title_task: RefCell::new(PreviewTitleTask::with_notifier(work_notifier.clone())),
            visual_execution,
            visual_execution_start_failure,
            visual_execution_health_failed: Cell::new(false),
            cpu_fallback_task,
            cpu_fallback_start_failure,
            viewer_cpu_fallback_active: Cell::new(false),
            cpu_fallback_in_flight: RefCell::new(None),
            cpu_fallback_failure: RefCell::new(None),
            visual_ready: RefCell::new(HashMap::new()),
            visual_failures: RefCell::new(HashMap::new()),
            media_execution_failures: RefCell::new(HashMap::new()),
            media_worker_health_failed: Cell::new(media_worker_start_failure.is_some()),
            media_worker_start_failure,
            last_current_media_admission: Cell::new(None),
            last_gpu_loading_reason: Cell::new(None),
            visual_terminal_candidates: RefCell::new(Vec::new()),
            visual_program_authoring_session: Cell::new(None),
            visual_programs: RefCell::new(mondrian_renderer::PreparedVisualProgramCache::default()),
            future_media_window: RefCell::new(request_scheduler::FutureMediaWindowCache::default()),
            visual_dependencies: PreviewVisualDependencyObserver::new_with_notifier(
                work_notifier.clone(),
            ),
            visual_dependency_health_failed: Cell::new(false),
            scratch: RefCell::new(TimelineCompositeScratch::default()),
            evaluation_working_set: RefCell::new(EvaluationWorkingSet::new()),
            evaluation_working_set_clock: Cell::new(0),
            last_color_rejection: RefCell::new(None),
            unavailability_evidence: RefCell::new(PreviewUnavailabilityEvidence::default()),
            display_snapshot: RefCell::new(None),
            hardware_decode_admission: Cell::new(PreviewHardwareDecodeAdmissionState::default()),
            decode_cpu_budget,
            decode_worker_count,
            decode_execution_watch: PreviewDecodeWorkerExecutionWatch::new(
                decode_execution_observers,
            ),
            metrics: PreviewMetrics::default(),
        }
    }

    /// Clone the payload-free completion watch shared by every Preview worker.
    ///
    /// Revisions only indicate that result transports should be polled. The
    /// Runtime's typed result pumps remain the sole completion authority.
    pub(crate) fn work_watch(&self) -> PreviewWorkWatch {
        self.work_watch.clone()
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
        let key = MediaPreviewKey::test_cpu(
            PathBuf::from("E:/media/pending-preview.mov"),
            MediaPreviewKey::test_fingerprint(1_920),
            mondrian_core::TimelineTime::ZERO,
            Resolution { width: 1920, height: 1080 },
            mondrian_media::PreviewSourceColorContract::automatic(
                ColorSpace::Srgb,
                DecodedVideoRange::Full,
            ),
        );
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
            residency_work: None,
        });
        self.execution.borrow_mut().invalidate(|| generation);
        self.execution.borrow_mut().set_pending(true);
    }

    /// Build a CPU working-space preview frame suitable for the app-window GPU output boundary.
    ///
    /// The returned frame is not display encoded. The app window owns wgpu recording,
    /// output texture registration, and texture lifetime.
    pub(crate) fn gpu_preview_frame(
        &self,
        request: PreviewFrameExecutionRequest<'_>,
    ) -> PreviewGpuFrameState {
        // Candidate evaluation is the acknowledgement boundary for retained
        // retry authority. Polling alone must never consume this request.
        if self.media_existing_work_retry_pending.replace(false) {
            bump(&self.media_existing_work_retry_acknowledgements);
        }
        self.last_gpu_loading_reason.set(None);
        let snapshot = request.snapshot();
        let proxy_demands = request.proxy_demands();
        let transport = snapshot.transport();
        bump(&self.metrics.gpu_preview_candidate_requests);
        self.synchronize_visual_program_authoring_session(snapshot);
        self.synchronize_transport_intent(transport.intent());
        self.pump_visual_execution_results(
            transport
                .is_playing()
                .then(|| transport.demand().map(PreviewFrameDemandSnapshot::identity))
                .flatten(),
        );
        self.execution.borrow_mut().set_pending(false);
        self.last_color_rejection.replace(None);
        let Some(authoring) = snapshot.authoring() else {
            self.invalidate_preview_generation();
            self.scheduler.prune_obsolete();
            return self.unavailable_gpu_candidate(PreviewUnavailability::no_content(
                PreviewOutputStage::Project,
                "no active Sequence",
            ));
        };
        let Some(sequence) = authoring.active_sequence() else {
            self.invalidate_preview_generation();
            self.scheduler.prune_obsolete();
            return self.unavailable_gpu_candidate(PreviewUnavailability::no_content(
                PreviewOutputStage::Project,
                "no active Sequence",
            ));
        };
        if let Err(reason) = self.apply_visual_dependency_refreshes() {
            return self.unavailable_gpu_candidate(reason);
        }
        let frame = transport.current_frame().max(0);
        let (width, height) = preview_dimensions_for_snapshot(snapshot, sequence);
        let display_snapshot = self.display_snapshot.borrow();
        let display_color_space = match preview_display_color_space(
            sequence,
            snapshot.viewer_display(),
            display_snapshot.as_ref(),
        ) {
            Ok(color_space) => color_space,
            Err(blocker) => {
                self.record_preview_gpu_output_blocker(&blocker);
                self.scheduler.prune_obsolete();
                return self.unavailable_gpu_candidate(PreviewUnavailability::blocked(
                    PreviewOutputStage::DisplayContract,
                    blocker.description(),
                ));
            }
        };
        let color_context =
            sequence.settings.root_program_color_context(authoring.color_environment());
        let Some(program_output_color_space) = color_context.output_color_space.color() else {
            self.record_preview_gpu_output_blocker(&PreviewGpuOutputBlocker::UnsupportedFeature {
                feature: "program_output_identity".to_owned(),
                reason: format!(
                    "Program Output {:?} is not an encoded color identity",
                    color_context.output_color_space
                ),
            });
            self.scheduler.prune_obsolete();
            return self.unavailable_gpu_candidate(PreviewUnavailability::blocked(
                PreviewOutputStage::ProgramOutput,
                format!(
                    "Program Output {:?} is not an encoded color identity",
                    color_context.output_color_space
                ),
            ));
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
                return self.unavailable_gpu_candidate(PreviewUnavailability::blocked(
                    PreviewOutputStage::MonitorAdaptation,
                    error.to_string(),
                ));
            }
        };
        let generation_binding =
            self.activate_preview_generation(ViewerPreviewGenerationKey::from_snapshot(
                snapshot,
                sequence,
                frame,
                width,
                height,
                display_color_space,
                display_snapshot.as_ref().map(DisplayOutputSnapshot::contract_identity),
            ));
        let generation = match generation_binding {
            PreviewGenerationBinding::Current(generation)
            | PreviewGenerationBinding::Rotated(generation) => generation,
        };
        let playback_intent = transport.playback_intent();
        if transport.is_successor_preparation()
            && self.execution.borrow().has_prepared_successor_for_intent(playback_intent)
        {
            self.scheduler.prune_obsolete();
            return PreviewGpuFrameState::Prepared;
        }
        let prepared_promotion = if transport.is_successor_preparation() {
            None
        } else {
            self.execution
                .borrow_mut()
                .promote_prepared_successor_for_intent(playback_intent)
        };
        if let Some(prepared) = prepared_promotion {
            self.scheduler.prune_obsolete();
            return match prepared {
                PreviewPreparedPromotion::Gpu { already_visible, .. } => {
                    bump(&self.metrics.gpu_preview_candidate_current);
                    let ticket = self.playback_presentation_ticket(snapshot);
                    let candidate = if already_visible {
                        PreviewPresentationCandidate::already_visible((), ticket)
                    } else {
                        PreviewPresentationCandidate::new((), ticket)
                    };
                    PreviewGpuFrameState::Current(candidate)
                }
                PreviewPreparedPromotion::Transparent => self.transparent_gpu_candidate(snapshot),
            };
        }
        if transport.is_playing()
            && transport.demand().is_none()
            && !transport.is_successor_preparation()
        {
            if transport.is_priming() {
                // Presenting the priming current frame consumes its Frame
                // Demand before bounded future-media preroll is necessarily
                // complete. This is not a terminal Late/Failed state. Viewer
                // generation freshness decides only whether an output remains
                // exactly publishable; it must not suppress media prefetch.
                self.execution.borrow_mut().set_pending(false);
                self.schedule_media_prefetches(
                    snapshot,
                    proxy_demands,
                    sequence,
                    frame,
                    width,
                    height,
                );
                self.scheduler.prune_obsolete();
                if matches!(generation_binding, PreviewGenerationBinding::Current(_))
                    && self.execution.borrow().has_exact_current_output()
                {
                    bump(&self.metrics.gpu_preview_candidate_current);
                    return PreviewGpuFrameState::Current(PreviewPresentationCandidate::new(
                        (),
                        self.playback_presentation_ticket(snapshot),
                    ));
                }
                self.execution.borrow_mut().set_pending(true);
                bump(&self.metrics.gpu_preview_candidate_loading);
                self.last_gpu_loading_reason.set(Some("priming_without_demand"));
                return PreviewGpuFrameState::Loading;
            }
            if matches!(generation_binding, PreviewGenerationBinding::Current(_))
                && self.execution.borrow().has_exact_current_output()
            {
                self.scheduler.prune_obsolete();
                bump(&self.metrics.gpu_preview_candidate_current);
                return PreviewGpuFrameState::Current(PreviewPresentationCandidate::new(
                    (),
                    self.playback_presentation_ticket(snapshot),
                ));
            }
            // A terminal Late/Failed/Canceled fact consumed this frame's
            // authority. Retain any prior output as stale and wait for the
            // Playback Engine to issue another demand; a no-ticket retry must
            // never turn the rejected artifact into a current output.
            self.execution.borrow_mut().set_pending(true);
            self.scheduler.prune_obsolete();
            bump(&self.metrics.gpu_preview_candidate_loading);
            self.last_gpu_loading_reason.set(Some("playing_without_demand"));
            return PreviewGpuFrameState::Loading;
        }
        if !transport.is_playing()
            && matches!(generation_binding, PreviewGenerationBinding::Current(_))
            && self.execution.borrow().has_exact_current_output()
        {
            self.scheduler.prune_obsolete();
            self.try_release_settled_transport_media_residency();
            bump(&self.metrics.gpu_preview_candidate_current);
            return PreviewGpuFrameState::Current(PreviewPresentationCandidate::new(
                (),
                self.playback_presentation_ticket(snapshot),
            ));
        }
        let evaluation_key = FrameEvaluationKey {
            sequence_id: sequence.id,
            sequence_revision: sequence.revision,
            author_generation: snapshot
                .authoring()
                .map(PreviewAuthoringSnapshot::author_generation)
                .unwrap_or(0),
            frame,
            width,
            height,
            runtime_scale: transport.runtime_scale(),
            display_color_space,
            display_contract_identity: display_snapshot
                .as_ref()
                .map(DisplayOutputSnapshot::contract_identity),
        };
        let resolved = match self.acquire_frame_evaluation(
            snapshot,
            proxy_demands,
            sequence,
            frame,
            width,
            height,
            color_context,
            evaluation_key,
        ) {
            FrameResolutionOutcome::Ready(evaluation) => {
                self.execution.borrow_mut().set_presentation_quality(
                    resolved_preview_presentation_quality(&evaluation.elements),
                );
                // The GPU output identity overlays the monitor adaptation on
                // the evaluation's plan identity; the shared evaluation stays
                // display-independent.
                let cache_key = evaluation.output_key.with_monitor_adaptation(&monitor_adaptation);
                let cache_reusable =
                    matches!(evaluation.reuse_policy, EvaluationReusePolicy::Reusable);
                if transport.is_successor_preparation()
                    && cache_reusable
                    && self
                        .execution
                        .borrow_mut()
                        .prepare_successor_from_current(playback_intent, &cache_key)
                {
                    self.scheduler.prune_obsolete();
                    return PreviewGpuFrameState::Prepared;
                }
                if cache_reusable && self.execution.borrow_mut().output_for(&cache_key).is_some() {
                    self.schedule_media_prefetches(
                        snapshot,
                        proxy_demands,
                        sequence,
                        frame,
                        width,
                        height,
                    );
                    self.scheduler.prune_obsolete();
                    self.try_release_settled_transport_media_residency();
                    bump(&self.metrics.gpu_preview_candidate_current);
                    return PreviewGpuFrameState::Current(PreviewPresentationCandidate::new(
                        (),
                        self.playback_presentation_ticket(snapshot),
                    ));
                }
                ResolvedPlanView {
                    elements: Arc::clone(&evaluation.elements),
                    cache_key,
                    cpu_cache_key: evaluation.output_key.clone(),
                    cache_reusable,
                    color_context: evaluation.color_context.clone(),
                }
            }
            FrameResolutionOutcome::Empty => {
                if transport.is_successor_preparation() {
                    self.execution
                        .borrow_mut()
                        .register_prepared_transparent_successor(playback_intent);
                    self.scheduler.prune_obsolete();
                    return PreviewGpuFrameState::Prepared;
                }
                let decision = self.execution.borrow_mut().plan_candidate(None);
                self.execution
                    .borrow_mut()
                    .set_presentation_quality(mondrian_playback::FramePresentationQuality::Ready);
                self.schedule_media_prefetches(
                    snapshot,
                    proxy_demands,
                    sequence,
                    frame,
                    width,
                    height,
                );
                self.scheduler.prune_obsolete();
                debug_assert!(matches!(decision, PreviewCandidateDecision::Unavailable));
                return self.transparent_gpu_candidate(snapshot);
            }
            FrameResolutionOutcome::Pending(dependency) => {
                let retryable_admission = matches!(
                    dependency,
                    crate::app::preview_timeline_execution::PreviewTimelinePendingDependency::Media {
                        wait: crate::app::preview_timeline_execution::PreviewTimelineMediaWait::RetryAdmission,
                        ..
                    }
                );
                let decision = self.execution.borrow_mut().plan_candidate(None);
                self.last_gpu_loading_reason.set(Some(match dependency {
                    crate::app::preview_timeline_execution::PreviewTimelinePendingDependency::Media { .. } => "timeline_media",
                    crate::app::preview_timeline_execution::PreviewTimelinePendingDependency::BasicTitle(_) => "timeline_basic_title",
                    crate::app::preview_timeline_execution::PreviewTimelinePendingDependency::Temporal { .. } => "timeline_temporal",
                }));
                self.schedule_media_prefetches(
                    snapshot,
                    proxy_demands,
                    sequence,
                    frame,
                    width,
                    height,
                );
                self.scheduler.prune_obsolete();
                // Prefetch planning can cancel or supersede the exact owner
                // that current Timeline evaluation just rebound to. Publish
                // an edge while the waiter is still retained; the ordinary
                // poll consumes it and authorizes exactly one re-evaluation.
                self.publish_existing_work_retry_if_actionable();
                return match decision {
                    PreviewCandidateDecision::Loading => {
                        bump(&self.metrics.gpu_preview_candidate_loading);
                        PreviewGpuFrameState::Loading
                    }
                    PreviewCandidateDecision::Unavailable => {
                        if retryable_admission {
                            bump(&self.metrics.gpu_preview_candidate_loading);
                            PreviewGpuFrameState::Loading
                        } else {
                            self.unavailable_gpu_candidate(PreviewUnavailability::failed(
                                PreviewOutputStage::TimelineEvaluation,
                                "Timeline reported pending media without registering pending work",
                            ))
                        }
                    }
                    PreviewCandidateDecision::Current | PreviewCandidateDecision::Execute(_) => {
                        unreachable!("unresolved preview intent cannot select an output")
                    }
                };
            }
            FrameResolutionOutcome::Unavailable(reason) => {
                let _ = self.execution.borrow_mut().plan_candidate(None);
                self.schedule_media_prefetches(
                    snapshot,
                    proxy_demands,
                    sequence,
                    frame,
                    width,
                    height,
                );
                self.scheduler.prune_obsolete();
                return self.unavailable_gpu_candidate(reason);
            }
        };
        if self.viewer_cpu_fallback_active.get() {
            let state =
                self.schedule_cpu_fallback(generation, transport.epoch(), &resolved, width, height);
            self.schedule_media_prefetches(snapshot, proxy_demands, sequence, frame, width, height);
            self.scheduler.prune_obsolete();
            return state;
        }
        let mut media_residency_protections =
            resolved_preview_media_protections(&resolved.elements);
        let mut cache_key = resolved.cache_key.clone();
        let program_output_boundary =
            match output_boundary_from_color_context(&resolved.color_context) {
                Ok(boundary) => boundary,
                Err(error) => {
                    return self.unavailable_gpu_candidate(error.unavailability());
                }
            };
        let decode_execution = resolved_preview_decode_execution(&resolved.elements);
        let heterogeneous_decision = self.heterogeneous_effect_decision.get();
        let prepared_gpu_layers = {
            let mut scratch = self.scratch.borrow_mut();
            prepare_gpu_composite_layers_with_heterogeneous_effects(
                &resolved.elements,
                resolved.color_context.working_color_space,
                &mut scratch,
                heterogeneous_decision.cpu_prefix_grant(),
            )
            .map_err(|error| {
                let blocker = match &error {
                    PreviewViewerGpuLayerPreparationError::Compositing { reason } => Some(*reason),
                    _ => None,
                };
                (blocker, error.to_string())
            })
        };
        let prepared_gpu_layers = match prepared_gpu_layers {
            Ok(prepared) => prepared,
            Err((blocker, detail)) => {
                self.record_gpu_compositing(GpuCompositingDiagnostics {
                    first_blocker: blocker,
                    ..GpuCompositingDiagnostics::default()
                });
                self.schedule_media_prefetches(
                    snapshot,
                    proxy_demands,
                    sequence,
                    frame,
                    width,
                    height,
                );
                self.scheduler.prune_obsolete();
                return self.unavailable_gpu_candidate(PreviewUnavailability::blocked(
                    PreviewOutputStage::GpuComposite,
                    detail,
                ));
            }
        };

        let (layers, heterogeneous_execution) = match prepared_gpu_layers {
            PreparedPreviewViewerGpuLayers::Ordinary { layers } => (layers, None),
            PreparedPreviewViewerGpuLayers::Heterogeneous { layers, cpu_prefix, continuations } => {
                let visual_fingerprint = if resolved.cache_reusable {
                    cache_key.plan_identity.semantic_fingerprint()
                } else {
                    // An Uncacheable graph may keep one in-progress attempt
                    // stable across UI polls, but it must never rebound work
                    // across Preview generations merely because its semantic
                    // graph fingerprint is unchanged.
                    let mut identity = PreviewSemanticIdentityBuilder::new(
                        b"mondrian.preview.visual-execution-attempt.v1",
                    );
                    cache_key.plan_identity.hash(&mut identity);
                    generation.hash(&mut identity);
                    identity.finish_identity().semantic_fingerprint()
                };
                let visual_key =
                    VisualExecutionTaskKey::from_complete_semantic_fingerprint(visual_fingerprint);
                if self.visual_execution_health_failed.get() {
                    self.schedule_media_prefetches(
                        snapshot,
                        proxy_demands,
                        sequence,
                        frame,
                        width,
                        height,
                    );
                    self.scheduler.prune_obsolete();
                    return self.unavailable_gpu_candidate(PreviewUnavailability::failed(
                        PreviewOutputStage::GpuComposite,
                        "Preview visual execution worker terminated",
                    ));
                }
                if let Some(failure) = self.visual_failures.borrow().get(&visual_key).cloned() {
                    self.schedule_media_prefetches(
                        snapshot,
                        proxy_demands,
                        sequence,
                        frame,
                        width,
                        height,
                    );
                    self.scheduler.prune_obsolete();
                    return self.unavailable_gpu_candidate(PreviewUnavailability::failed(
                        PreviewOutputStage::GpuComposite,
                        failure,
                    ));
                }

                let ready = self.visual_ready.borrow_mut().remove(&visual_key);
                let ready = ready.filter(|ready| {
                    ready.generation() == generation && ready.epoch() == transport.epoch()
                });
                let Some(ready) = ready else {
                    let decision = heterogeneous_decision;
                    let access_intent = media_preview_viewer_access_intent(
                        transport.is_playing(),
                        transport.seek_source(),
                    );
                    let access_mode = media_preview_access_mode_for_intent(access_intent);
                    let work_class = media_preview_frame_work_class(access_mode);
                    let demand = transport.is_playing().then(|| transport.demand()).flatten();
                    let gpu_grant = decision.gpu_continuation_grant();
                    if let Err(error) = cpu_prefix.validate_gpu_recording_grant(gpu_grant) {
                        if let Some(demand) = demand {
                            self.queue_visual_terminal_candidate(
                                mondrian_playback::FrameDeliveryCandidate::for_demand(
                                    demand.identity(),
                                    mondrian_playback::FrameDeliveryKind::Failed,
                                ),
                            );
                        }
                        self.schedule_media_prefetches(
                            snapshot,
                            proxy_demands,
                            sequence,
                            frame,
                            width,
                            height,
                        );
                        self.scheduler.prune_obsolete();
                        return self.unavailable_gpu_candidate(PreviewUnavailability::failed(
                            PreviewOutputStage::GpuComposite,
                            format!("Preview heterogeneous GPU admission failed: {error}"),
                        ));
                    }
                    let Some(task) = self.visual_execution.as_ref() else {
                        let detail = self
                            .visual_execution_start_failure
                            .as_deref()
                            .unwrap_or("Preview visual execution worker is unavailable");
                        if let Some(demand) = demand {
                            self.queue_visual_terminal_candidate(
                                mondrian_playback::FrameDeliveryCandidate::for_demand(
                                    demand.identity(),
                                    mondrian_playback::FrameDeliveryKind::Failed,
                                ),
                            );
                        }
                        self.schedule_media_prefetches(
                            snapshot,
                            proxy_demands,
                            sequence,
                            frame,
                            width,
                            height,
                        );
                        self.scheduler.prune_obsolete();
                        return self.unavailable_gpu_candidate(PreviewUnavailability::failed(
                            PreviewOutputStage::GpuComposite,
                            detail,
                        ));
                    };
                    let sampled_at = Instant::now();
                    let deadline = demand
                        .and_then(|demand| demand.adapter_deadline())
                        .map(|deadline| task.project_adapter_deadline(deadline, sampled_at));
                    let admission = VisualExecutionAdmission::new(
                        visual_key,
                        generation,
                        if transport.is_successor_preparation() {
                            mondrian_playback::FrameWorkPriority::Prefetch
                        } else {
                            mondrian_playback::FrameWorkPriority::Current
                        },
                        work_class,
                        demand.map(|demand| demand.identity()),
                        deadline,
                        VisualExecutionTaskPayload::heterogeneous_cpu_prefix_batch(
                            transport.epoch(),
                            cpu_prefix,
                            gpu_grant,
                            std::mem::take(&mut media_residency_protections),
                        ),
                    );
                    let submission = task.submit(admission);
                    self.execution.borrow_mut().set_pending(true);
                    self.schedule_media_prefetches(
                        snapshot,
                        proxy_demands,
                        sequence,
                        frame,
                        width,
                        height,
                    );
                    self.scheduler.prune_obsolete();
                    return match submission {
                        mondrian_playback::FrameWorkSubmission::Queued { .. }
                        | mondrian_playback::FrameWorkSubmission::UpdatedQueued { .. }
                        | mondrian_playback::FrameWorkSubmission::ReusedInFlight
                        | mondrian_playback::FrameWorkSubmission::DroppedBackpressure => {
                            bump(&self.metrics.gpu_preview_candidate_loading);
                            self.last_gpu_loading_reason.set(Some("visual_execution"));
                            PreviewGpuFrameState::Loading
                        }
                        mondrian_playback::FrameWorkSubmission::DroppedObsoleteGeneration => {
                            self.execution.borrow_mut().set_pending(false);
                            if let Some(demand) = demand {
                                self.queue_visual_terminal_candidate(
                                    mondrian_playback::FrameDeliveryCandidate::for_demand(
                                        demand.identity(),
                                        mondrian_playback::FrameDeliveryKind::Canceled,
                                    ),
                                );
                            }
                            self.unavailable_gpu_candidate(PreviewUnavailability::failed(
                                PreviewOutputStage::GpuComposite,
                                "Preview visual execution generation was superseded before admission",
                            ))
                        }
                        mondrian_playback::FrameWorkSubmission::DroppedInvalidClass
                        | mondrian_playback::FrameWorkSubmission::Closed => {
                            self.execution.borrow_mut().set_pending(false);
                            if let Some(demand) = demand {
                                self.queue_visual_terminal_candidate(
                                    mondrian_playback::FrameDeliveryCandidate::for_demand(
                                        demand.identity(),
                                        mondrian_playback::FrameDeliveryKind::Failed,
                                    ),
                                );
                            }
                            self.unavailable_gpu_candidate(PreviewUnavailability::failed(
                                PreviewOutputStage::GpuComposite,
                                "Preview visual execution Broker rejected current work",
                            ))
                        }
                    };
                };

                let (output, lease) = ready.into_parts();
                let (output, gpu_grant, async_media_residency_protections) =
                    output.into_heterogeneous_cpu_prefix_batch();
                media_residency_protections.extend(async_media_residency_protections);
                let completions = output.into_completions().into_vec();
                if completions.len() != continuations.len()
                    || completions.iter().zip(continuations.iter()).any(
                        |(completion, continuation)| completion.address() != continuation.address(),
                    )
                {
                    let disposition = self.visual_gpu_failure_disposition(lease.fail_gpu());
                    self.queue_visual_terminal_disposition(disposition);
                    self.schedule_media_prefetches(
                        snapshot,
                        proxy_demands,
                        sequence,
                        frame,
                        width,
                        height,
                    );
                    self.scheduler.prune_obsolete();
                    return self.unavailable_gpu_candidate(PreviewUnavailability::failed(
                        PreviewOutputStage::GpuComposite,
                        "heterogeneous CPU completions do not match Viewer continuation addresses",
                    ));
                }
                let inputs = completions
                    .into_iter()
                    .zip(continuations.into_vec())
                    .map(|(completion, continuation)| {
                        let (_, completion) = completion.into_parts();
                        ViewerHeterogeneousGpuInput {
                            request: continuation.gpu_continuation_request(generation, gpu_grant),
                            completion,
                        }
                    })
                    .collect();
                let execution = match PreviewGpuHeterogeneousExecution::new(
                    inputs,
                    lease,
                    resolved.cache_reusable,
                ) {
                    Ok(execution) => execution,
                    Err((error, lease)) => {
                        let disposition = self.visual_gpu_failure_disposition(lease.fail_gpu());
                        self.queue_visual_terminal_disposition(disposition);
                        self.schedule_media_prefetches(
                            snapshot,
                            proxy_demands,
                            sequence,
                            frame,
                            width,
                            height,
                        );
                        self.scheduler.prune_obsolete();
                        return self.unavailable_gpu_candidate(PreviewUnavailability::failed(
                            PreviewOutputStage::GpuComposite,
                            error.to_string(),
                        ));
                    }
                };
                (layers, Some(execution))
            }
        };
        self.record_composite(TimelineCompositeDiagnostics {
            elements: resolved.elements.len() as u64,
            float_linear_composites: 1,
            ..TimelineCompositeDiagnostics::default()
        });
        let working_input = PreviewGpuWorkingInput::GpuComposite { layers };
        let candidate_id = match self
            .execution
            .borrow_mut()
            .plan_candidate_with_reuse(Some(&cache_key), resolved.cache_reusable)
        {
            PreviewCandidateDecision::Current => {
                self.schedule_media_prefetches(
                    snapshot,
                    proxy_demands,
                    sequence,
                    frame,
                    width,
                    height,
                );
                self.scheduler.prune_obsolete();
                self.try_release_settled_transport_media_residency();
                bump(&self.metrics.gpu_preview_candidate_current);
                return PreviewGpuFrameState::Current(PreviewPresentationCandidate::new(
                    (),
                    self.playback_presentation_ticket(snapshot),
                ));
            }
            PreviewCandidateDecision::Execute(candidate_id) => candidate_id,
            PreviewCandidateDecision::Loading | PreviewCandidateDecision::Unavailable => {
                unreachable!("a resolved Viewer plan must select current output or execution")
            }
        };
        if !resolved.cache_reusable {
            cache_key = cache_key.with_execution_nonce(candidate_id);
        }
        self.schedule_media_prefetches(snapshot, proxy_demands, sequence, frame, width, height);
        self.scheduler.prune_obsolete();
        bump(&self.metrics.gpu_preview_candidate_ready);
        add_cell(
            &self.metrics.gpu_preview_candidate_pixels,
            (width as u64).saturating_mul(height as u64),
        );
        PreviewGpuFrameState::Ready(Box::new(PreviewGpuFrame::new(
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
            if transport.is_successor_preparation() {
                crate::app::preview_execution::PreviewGpuFramePurpose::SuccessorPreparation
            } else {
                crate::app::preview_execution::PreviewGpuFramePurpose::Current
            },
            playback_intent,
            self.playback_presentation_ticket(snapshot),
            decode_execution,
            heterogeneous_execution,
            media_residency_protections,
        )))
    }

    /// Build the exact ticket that a Presentation Adapter may complete only
    /// after it makes the current output usable.
    pub(crate) fn playback_presentation_ticket(
        &self,
        snapshot: &PreviewExecutionSnapshot<'_>,
    ) -> Option<mondrian_playback::FramePresentationTicket> {
        snapshot.presentation_ticket(self.execution.borrow().presentation_quality())
    }

    /// Commit a fully prepared GPU output made usable by the active Adapter.
    ///
    /// Output validation and every fallible operation must precede the
    /// presentation commit seam. Registration is therefore an infallible,
    /// bounded current-output replacement plus one metric update. In
    /// particular, it must not prune work or release decoded-media residency;
    /// the Adapter performs that maintenance only after presentation
    /// finalization returns `Presented` or `NoDemand`.
    pub(crate) fn register_gpu_output(&self, output_key: PreviewOutputKey, output: O) {
        self.execution.borrow_mut().register_output(output_key, output);
        bump(&self.metrics.gpu_preview_external_frames_registered);
    }

    /// Retain one ticketless immediate-successor GPU output for later promotion.
    ///
    /// This never changes the currently visible output. Generation rotation or
    /// a later successor replacement retires the slot automatically.
    pub(crate) fn register_prepared_gpu_successor(
        &self,
        playback_intent: crate::app::preview_execution::PreviewPlaybackIntent,
        output_key: PreviewOutputKey,
        output: O,
    ) {
        self.execution.borrow_mut().register_prepared_successor(
            playback_intent,
            output_key,
            output,
        );
        bump(&self.metrics.gpu_preview_external_frames_registered);
    }

    /// Whether ticketless work already proves this exact running coordinate.
    ///
    /// This is a bounded scheduling observation only. Presentation still
    /// requires a fresh current request carrying the active Frame Demand.
    #[cfg(test)]
    pub(crate) fn has_prepared_successor_for_intent(
        &self,
        playback_intent: crate::app::preview_execution::PreviewPlaybackIntent,
    ) -> bool {
        self.execution.borrow().has_prepared_successor_for_intent(playback_intent)
    }

    /// Exact successor output that was already visible before its boundary.
    #[cfg(test)]
    pub(crate) fn already_visible_successor_output_key(
        &self,
        playback_intent: crate::app::preview_execution::PreviewPlaybackIntent,
    ) -> Option<PreviewOutputKey> {
        self.execution.borrow().already_visible_successor_key(playback_intent).cloned()
    }

    /// Whether the coordinator retains any physically usable GPU output,
    /// including a stale output that is not bound to the current intent.
    #[cfg(test)]
    pub(crate) fn has_retained_gpu_output(&self) -> bool {
        self.execution.borrow().current_output().is_some()
    }

    /// Whether the sole registered output has this complete resolved identity.
    #[cfg(test)]
    pub(crate) fn has_gpu_output_for_key(&self, key: &PreviewOutputKey) -> bool {
        self.execution
            .borrow()
            .current_output()
            .is_some_and(|(current, _)| current == key)
    }

    /// Complete identity of the sole physically retained GPU output.
    #[cfg(any(test, feature = "validation"))]
    pub(crate) fn registered_gpu_output_key(&self) -> Option<PreviewOutputKey> {
        self.execution.borrow().current_output().map(|(key, _)| key.clone())
    }

    /// Revoke a registered GPU output whose exact completion callback was lost.
    ///
    /// A force-retired quarantined submission may already have published
    /// queue-order; without its callback evidence the artifact must not stay
    /// current. Only the exact key is revoked; a newer output is untouched.
    #[cfg(any(test, feature = "validation"))]
    pub(crate) fn clear_registered_output_for_key(&self, key: &PreviewOutputKey) {
        self.execution.borrow_mut().clear_current_output_for_key(key);
    }

    /// Revoke one prepared successor whose exact completion callback was lost.
    #[cfg(any(test, feature = "validation"))]
    pub(crate) fn clear_prepared_successor_for_intent(&self, intent: PreviewPlaybackIntent) {
        self.execution.borrow_mut().clear_prepared_successor_for_intent(intent);
    }

    /// Complete identity of an already-published output proved under the
    /// active Preview generation.
    ///
    /// Unlike candidate resolution, this query cannot reactivate a stale
    /// output or schedule work. Presentation Adapters use it to observe a
    /// durable current artifact after its Frame Demand was already consumed.
    pub(crate) fn registered_exact_current_gpu_output_key(&self) -> Option<PreviewOutputKey> {
        self.execution.borrow().exact_current_output().map(|(key, _)| key.clone())
    }

    /// Whether the retained output is the exact semantic and physical artifact.
    ///
    /// `matches_artifact` compares Adapter-owned physical identity (for
    /// example a Window texture key or a Headless resource key). Callers must
    /// not infer physical identity from the semantic [`PreviewOutputKey`]:
    /// multiple submissions may resolve the same semantic output.
    pub(crate) fn has_gpu_output_artifact(
        &self,
        key: &PreviewOutputKey,
        matches_artifact: impl Fn(&O) -> bool,
    ) -> bool {
        self.execution
            .borrow()
            .current_output()
            .is_some_and(|(current, output)| current == key && matches_artifact(output))
    }

    /// Resolve a Broker-owned heterogeneous candidate only after renderer
    /// completion evidence proves the exact GPU submission finished.
    pub(crate) fn finalize_heterogeneous_gpu_completion(
        &self,
        execution: PreviewGpuHeterogeneousExecution,
        completed: &mondrian_renderer::ViewerHeterogeneousGpuCompletedBatch,
    ) -> Result<PreviewVisualGpuCompletionDisposition, PreviewGpuHeterogeneousCompletionError> {
        let reusable = execution.reusable();
        if let Err(error) = execution.validate_completed(completed) {
            let disposition = self.fail_heterogeneous_gpu_execution(execution);
            self.queue_visual_terminal_disposition(disposition);
            return Err(error);
        }
        let lease = execution.into_lease()?;
        let active_generation = self.execution.borrow().generation();
        let active_epoch = self.transport_epoch.get();
        let lifecycle_current =
            lease.generation() == active_generation && active_epoch == Some(lease.epoch());
        let finalization = lease.finalize_gpu(lifecycle_current && reusable);
        if !lifecycle_current || !finalization.completion_recorded() {
            return Ok(PreviewVisualGpuCompletionDisposition::Release);
        }
        if finalization.deadline_status().is_missed() {
            return Ok(
                match (finalization.work_class(), finalization.demand_identity()) {
                    (mondrian_playback::FrameWorkClass::Playback, Some(identity)) => {
                        PreviewVisualGpuCompletionDisposition::TerminalCandidate(
                            mondrian_playback::FrameDeliveryCandidate::for_demand(
                                identity,
                                mondrian_playback::FrameDeliveryKind::Late,
                            ),
                        )
                    }
                    _ => PreviewVisualGpuCompletionDisposition::Release,
                },
            );
        }
        if finalization.may_publish_current() {
            Ok(PreviewVisualGpuCompletionDisposition::PublishCurrent)
        } else {
            Ok(PreviewVisualGpuCompletionDisposition::Release)
        }
    }

    /// Fail one Broker-owned heterogeneous candidate before actual GPU
    /// completion. A latest exact Playback binding receives one timestamp-free
    /// `Failed` candidate; superseded or non-Playback work is released silently.
    pub(crate) fn fail_heterogeneous_gpu_execution(
        &self,
        execution: PreviewGpuHeterogeneousExecution,
    ) -> PreviewVisualGpuCompletionDisposition {
        execution
            .fail()
            .map_or(PreviewVisualGpuCompletionDisposition::Release, |failure| {
                self.visual_gpu_failure_disposition(failure)
            })
    }

    fn visual_gpu_failure_disposition(
        &self,
        failure: VisualExecutionGpuFailure,
    ) -> PreviewVisualGpuCompletionDisposition {
        let active_generation = self.execution.borrow().generation();
        let active_epoch = self.transport_epoch.get();
        if failure.generation() != active_generation || active_epoch != Some(failure.epoch()) {
            return PreviewVisualGpuCompletionDisposition::Release;
        }
        match (failure.work_class(), failure.demand_identity()) {
            (Some(mondrian_playback::FrameWorkClass::Playback), Some(identity)) => {
                PreviewVisualGpuCompletionDisposition::TerminalCandidate(
                    mondrian_playback::FrameDeliveryCandidate::for_demand(
                        identity,
                        mondrian_playback::FrameDeliveryKind::Failed,
                    ),
                )
            }
            _ => PreviewVisualGpuCompletionDisposition::Release,
        }
    }

    fn queue_visual_terminal_disposition(
        &self,
        disposition: PreviewVisualGpuCompletionDisposition,
    ) {
        if let PreviewVisualGpuCompletionDisposition::TerminalCandidate(candidate) = disposition {
            self.queue_visual_terminal_candidate(candidate);
        }
    }

    fn queue_visual_terminal_candidate(
        &self,
        candidate: mondrian_playback::FrameDeliveryCandidate,
    ) {
        let mut candidates = self.visual_terminal_candidates.borrow_mut();
        if !candidates.iter().any(|existing| existing.identity() == candidate.identity()) {
            candidates.push(candidate);
        }
    }

    /// Drain the bounded visual worker handoff without blocking the caller.
    ///
    /// Ready CPU prefixes remain move-only and bounded by the Broker/result
    /// capacities. Failed attempts retain only a bounded diagnostic string and
    /// an optional exact terminal Playback candidate.
    fn pump_visual_execution_results(
        &self,
        pending_demand: Option<mondrian_playback::FrameDemandIdentity>,
    ) -> PreviewWorkPoll {
        let Some(task) = self.visual_execution.as_ref() else {
            return PreviewWorkPoll::default();
        };
        let active_generation = self.execution.borrow().generation();
        let active_epoch = self.transport_epoch.get();
        let mut outcome = PreviewWorkPoll::default();
        let mut drained = 0usize;
        for _ in 0..MEDIA_PREVIEW_MAX_COMPLETED_RESULTS_PER_POLL {
            let result = match task.try_poll() {
                VisualExecutionTaskPoll::Result(result) => *result,
                VisualExecutionTaskPoll::Empty => break,
                VisualExecutionTaskPoll::Disconnected => {
                    outcome.visible_change |=
                        self.observe_visual_execution_disconnect(pending_demand);
                    break;
                }
            };
            drained = drained.saturating_add(1);
            match result {
                VisualExecutionTaskResult::PrefixReady(ready) => {
                    if ready.generation() == active_generation
                        && active_epoch == Some(ready.epoch())
                    {
                        self.visual_failures.borrow_mut().remove(&ready.key());
                        self.visual_ready.borrow_mut().insert(ready.key(), ready);
                        outcome.visible_change = true;
                    }
                    // A stale result drops here and its lease removes the
                    // unresolved Broker binding through RAII.
                }
                VisualExecutionTaskResult::Failed(failed) => {
                    self.observe_visual_execution_failure(&failed, active_generation, active_epoch);
                    outcome.visible_change |= failed.generation() == active_generation
                        && active_epoch == Some(failed.epoch());
                }
            }
        }
        // Reaching the exact bound cannot prove the channel is empty. Request
        // one more bounded turn even when every result was published before
        // the native event sampled its final work-watch revision.
        outcome.needs_follow_up_poll = visual_execution_drain_needs_follow_up(drained);
        outcome
    }

    fn pump_cpu_fallback_results(&self) -> PreviewWorkPoll {
        let Some(task) = self.cpu_fallback_task.as_ref() else {
            return PreviewWorkPoll::default();
        };
        let active_generation = self.execution.borrow().generation();
        let active_epoch = self.transport_epoch.get();
        let mut outcome = PreviewWorkPoll::default();
        while let Some(result) = task.try_poll() {
            match result {
                PreviewCpuFallbackResult::Ready(ready) => {
                    if ready.generation == active_generation
                        && active_epoch == Some(ready.epoch)
                        && self.viewer_cpu_fallback_active.get()
                    {
                        self.cpu_fallback_in_flight.borrow_mut().take();
                        self.cpu_fallback_failure.borrow_mut().take();
                        self.record_composite(ready.execution.composite_diagnostics);
                        for diagnostics in &ready.execution.input_color_diagnostics {
                            self.record_color_transform(*diagnostics);
                        }
                        if ready.execution.input_color_stage_diagnostics
                            != RenderColorStageDiagnostics::default()
                        {
                            self.record_color_stage(ready.execution.input_color_stage_diagnostics);
                        }
                        if ready.execution.composite_diagnostics.legacy_rgba8_composites > 0 {
                            self.record_preview_gpu_output_blocker(
                                &PreviewGpuOutputBlocker::LegacyRgba8CompositeBoundary {
                                    legacy_composites: ready
                                        .execution
                                        .composite_diagnostics
                                        .legacy_rgba8_composites,
                                },
                            );
                        }
                        self.record_color_transform(ready.execution.color_diagnostics);
                        if let Some(diagnostics) = ready.execution.monitor_color_diagnostics {
                            self.record_color_transform(diagnostics);
                        }
                        self.record_color_stage(ready.execution.color_stage_diagnostics);
                        let mut stage_durations = PreviewRenderStageDurations::default();
                        stage_durations
                            .accumulate_cpu_execution(ready.execution.execution_durations);
                        let total_duration_us = ready
                            .execution
                            .execution_durations
                            .working_prepare_us
                            .saturating_add(ready.execution.execution_durations.cpu_composite_us)
                            .saturating_add(
                                ready.execution.execution_durations.cpu_output_boundary_us,
                            );
                        self.record_render_stage_durations(total_duration_us, stage_durations);
                        self.frame_store
                            .borrow_mut()
                            .insert_viewer_frame(ready.output_key, ready.frame);
                        self.execution.borrow_mut().set_pending(false);
                        outcome.visible_change = true;
                    }
                }
                PreviewCpuFallbackResult::Failed(failed) => {
                    if failed.generation == active_generation
                        && active_epoch == Some(failed.epoch)
                        && self.viewer_cpu_fallback_active.get()
                    {
                        self.cpu_fallback_in_flight.borrow_mut().take();
                        self.cpu_fallback_failure
                            .borrow_mut()
                            .replace((failed.output_key, failed.reason));
                        self.execution.borrow_mut().set_pending(false);
                        outcome.visible_change = true;
                    }
                }
            }
        }
        outcome
    }

    fn observe_visual_execution_disconnect(
        &self,
        pending_demand: Option<mondrian_playback::FrameDemandIdentity>,
    ) -> bool {
        if !worker_disconnect_is_terminal_health_failure(
            usize::from(self.visual_execution.is_some()),
            self.shutdown.is_requested(),
            self.visual_execution_health_failed.get(),
        ) {
            return false;
        }
        self.visual_execution_health_failed.set(true);
        if let Some(task) = self.visual_execution.as_ref() {
            task.close_after_worker_disconnect();
        }
        self.visual_ready.borrow_mut().clear();
        self.visual_failures.borrow_mut().clear();
        self.execution.borrow_mut().set_pending(false);
        if let Some(identity) = pending_demand {
            self.queue_visual_terminal_candidate(
                mondrian_playback::FrameDeliveryCandidate::for_demand(
                    identity,
                    mondrian_playback::FrameDeliveryKind::Failed,
                ),
            );
        }
        tracing::error!(
            "Preview visual execution result channel disconnected; visual admission is terminally unavailable"
        );
        true
    }

    /// Publish the one terminal media-worker health transition.
    ///
    /// Workerless test/runtime configurations and intentional shutdown are not
    /// failures. A production receiver disconnect means every configured
    /// result producer has exited, so no queued decode may remain authoritative.
    pub(crate) fn observe_media_worker_result_disconnect(&self) -> bool {
        if !worker_disconnect_is_terminal_health_failure(
            self.decode_worker_count,
            self.shutdown.is_requested(),
            self.media_worker_health_failed.get(),
        ) {
            return false;
        }
        self.media_worker_health_failed.set(true);
        self.scheduler.close();
        self.execution.borrow_mut().set_pending(false);
        tracing::error!(
            decode_worker_count = self.decode_worker_count,
            "Preview media result channel disconnected before shutdown; decode admission is terminally unavailable"
        );
        true
    }

    /// Whether the configured media worker group terminated unexpectedly.
    pub(crate) fn media_worker_health_failed(&self) -> bool {
        self.media_worker_health_failed.get()
    }

    fn observe_visual_execution_failure(
        &self,
        failed: &VisualExecutionFailed,
        active_generation: u64,
        active_epoch: Option<mondrian_playback::PlaybackEpoch>,
    ) {
        if failed.owns_terminal_binding()
            && failed.terminal_evidence().is_some()
            && failed.generation() == active_generation
            && active_epoch == Some(failed.epoch())
        {
            self.visual_failures
                .borrow_mut()
                .insert(failed.key(), failed.failure().to_string());
        }
        let Some(identity) = failed.demand_identity() else {
            return;
        };
        let kind = match failed.failure() {
            VisualExecutionTaskFailure::BrokerCanceled {
                evidence:
                    mondrian_playback::FrameExecutionCancellationEvidence {
                        cancellation:
                            mondrian_playback::FrameExecutionCancellation::DeadlineExpired { .. },
                        ..
                    },
            } => Some(mondrian_playback::FrameDeliveryKind::Late),
            VisualExecutionTaskFailure::BrokerCanceled { .. } => None,
            VisualExecutionTaskFailure::LeaseUnavailable
            | VisualExecutionTaskFailure::RendererBatch(_)
            | VisualExecutionTaskFailure::WorkerPanicked { .. } => {
                Some(mondrian_playback::FrameDeliveryKind::Failed)
            }
        };
        let Some(kind) = kind else {
            return;
        };
        self.queue_visual_terminal_candidate(
            mondrian_playback::FrameDeliveryCandidate::for_demand(identity, kind),
        );
    }

    fn take_visual_terminal_candidates(
        &self,
        pending_demand: Option<mondrian_playback::FrameDemandIdentity>,
    ) -> Vec<mondrian_playback::FrameDeliveryCandidate> {
        let candidates = self.visual_terminal_candidates.take();
        pending_demand
            .and_then(|pending| {
                candidates.into_iter().find(|candidate| candidate.identity() == pending)
            })
            .into_iter()
            .collect()
    }

    /// Record rejection by a concrete presentation Adapter before publication.
    pub(crate) fn reject_gpu_output_registration(&self) {
        bump(&self.metrics.gpu_preview_external_frames_rejected);
    }

    /// Clear any external GPU viewer frame currently advertised by the service.
    pub(crate) fn clear_external_viewer_frame(&self) {
        self.execution.borrow_mut().clear_output();
        bump(&self.metrics.gpu_preview_external_frames_cleared);
    }

    /// Clear a quarantined output only if it is still the exact semantic and
    /// physical artifact registered by its Adapter.
    ///
    /// A late callback may share [`PreviewOutputKey`] with a newer submission.
    /// The Adapter-supplied predicate therefore remains part of the authority
    /// check and must compare a unique physical publication identity.
    pub(crate) fn clear_external_viewer_frame_for_artifact(
        &self,
        key: &PreviewOutputKey,
        matches_artifact: impl Fn(&O) -> bool,
    ) -> bool {
        let cleared = self.execution.borrow_mut().clear_output_if(key, matches_artifact);
        if cleared {
            bump(&self.metrics.gpu_preview_external_frames_cleared);
        }
        cleared
    }

    /// Synchronize the display output contract snapshot used by preview scheduling.
    pub(crate) fn set_display_output_snapshot(&self, snapshot: Option<&DisplayOutputSnapshot>) {
        let previous_identity = self
            .display_snapshot
            .borrow()
            .as_ref()
            .map(DisplayOutputSnapshot::contract_identity);
        let next_identity = snapshot.map(DisplayOutputSnapshot::contract_identity);

        if previous_identity != next_identity {
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

    fn apply_visual_dependency_refreshes(
        &self,
    ) -> std::result::Result<bool, PreviewUnavailability> {
        self.ensure_visual_dependency_observer_healthy()?;
        let refreshes = self.visual_dependencies.poll_refreshes();
        self.ensure_visual_dependency_observer_healthy()?;
        if refreshes.is_empty() {
            return Ok(false);
        }
        {
            let mut programs = self.visual_programs.borrow_mut();
            for refresh in &refreshes {
                programs.invalidate_sequence(refresh.sequence_id);
                tracing::info!(
                    sequence_id = %refresh.sequence_id,
                    sequence_revision = ?refresh.sequence_revision,
                    effect_registry_revision = refresh.effect_registry_revision,
                    reason = %refresh.reason,
                    "evicted stale Preview visual program"
                );
            }
        }
        // A nested Sequence may contribute to any retained root Viewer frame.
        // Clear only the derived Viewer product; decoded-media residency has a
        // separate physical identity and remains valid.
        self.frame_store.borrow_mut().clear_viewer_frames();
        self.execution.borrow_mut().clear_output();
        self.invalidate_preview_generation();
        self.scheduler.prune_obsolete();
        Ok(true)
    }

    fn ensure_visual_dependency_observer_healthy(
        &self,
    ) -> std::result::Result<(), PreviewUnavailability> {
        if !self.visual_dependencies.is_healthy() {
            if !self.visual_dependency_health_failed.replace(true) {
                self.visual_programs.borrow_mut().clear();
                self.frame_store.borrow_mut().clear_viewer_frames();
                self.execution.borrow_mut().clear_output();
                self.invalidate_preview_generation();
                self.scheduler.prune_obsolete();
            }
            return Err(PreviewUnavailability::blocked(
                PreviewOutputStage::TimelineEvaluation,
                "Preview visual dependency observer is unavailable",
            ));
        }
        self.visual_dependency_health_failed.set(false);
        Ok(())
    }

    fn activate_preview_generation(
        &self,
        key: ViewerPreviewGenerationKey,
    ) -> PreviewGenerationBinding {
        // A stopped transport may retain decoded hardware surfaces until its
        // final Viewer output is independently usable. Registration can race
        // the worker lease's final Broker resolution, so retry at the last
        // safe boundary before changing the generation proof. Rotating first
        // would make the retained output stale and permanently miss this
        // release window, allowing decoder surface pools to accumulate across
        // exact seeks.
        let will_rotate = !self.execution.borrow().is_current_generation_key(&key);
        if will_rotate {
            self.try_release_settled_transport_media_residency();
        }
        let preserve_playback_locality = self
            .execution
            .borrow()
            .current_generation_key()
            .is_some_and(|current| key.has_compatible_playback_media_authority(current));
        let binding = self.execution.borrow_mut().bind_generation(key, || {
            if preserve_playback_locality {
                self.scheduler.begin_generation_preserving_playback_locality()
            } else {
                self.scheduler.begin_generation()
            }
        });
        let generation = match binding {
            PreviewGenerationBinding::Current(generation)
            | PreviewGenerationBinding::Rotated(generation) => generation,
        };
        if matches!(binding, PreviewGenerationBinding::Rotated(_)) {
            self.decode_residency_waiting.set(None);
            self.media_aggregate_capacity_waiting.set(false);
            self.media_existing_work_waiters.borrow_mut().clear();
            self.media_existing_work_retry_pending.set(false);
            if let Some(task) = &self.visual_execution {
                task.prune_before(generation);
            }
            self.visual_ready.borrow_mut().clear();
            self.visual_failures.borrow_mut().clear();
            self.media_execution_failures.borrow_mut().clear();
        }
        self.scratch.borrow_mut().bind_effect_execution_generation(generation);
        binding
    }

    fn invalidate_preview_generation(&self) {
        self.decode_residency_waiting.set(None);
        self.media_aggregate_capacity_waiting.set(false);
        self.media_existing_work_waiters.borrow_mut().clear();
        self.media_existing_work_retry_pending.set(false);
        let generation =
            self.execution.borrow_mut().invalidate(|| self.scheduler.begin_generation());
        if let Some(task) = &self.visual_execution {
            task.prune_before(generation);
        }
        self.visual_ready.borrow_mut().clear();
        self.visual_failures.borrow_mut().clear();
        self.media_execution_failures.borrow_mut().clear();
        self.scratch.borrow_mut().bind_effect_execution_generation(generation);
    }

    fn registered_gpu_output_for_key(&self, key: &ViewerPreviewCacheKey) -> Option<O> {
        self.execution.borrow_mut().output_for(key)
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
            tracing::warn!("production preview worker panicked during shutdown");
        }
    }
}

/// Borrowed projection of a resolved evaluation consumed by the GPU producer.
///
/// Preserves the legacy field access of `ResolvedPreviewPlan` while the
/// evaluation itself becomes the single authoritative construction point
/// (output key, elements, color context, reuse policy).
struct ResolvedPlanView {
    elements: Arc<[ResolvedPreviewElement]>,
    cache_key: PreviewOutputKey,
    cpu_cache_key: PreviewOutputKey,
    cache_reusable: bool,
    color_context: ProgramColorContext,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ViewerPreviewGenerationKey {
    sequence_id: SequenceId,
    sequence_revision: mondrian_core::SequenceRevision,
    project_author_generation: u64,
    display_contract_identity: Option<DisplayOutputIdentity>,
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
    /// Whether a Viewer-generation rotation leaves the underlying playback
    /// media work identity unchanged.
    ///
    /// Monitor/color contracts and presentation source may rotate final Viewer
    /// work while the queued media keys remain reusable. Output dimensions are
    /// deliberately part of this predicate: a scale change may reuse the open
    /// decoder Session, but old-size queued work would occupy the entire bounded
    /// prefetch window and starve admission of the new Half/Quarter keys.
    fn has_compatible_playback_media_authority(&self, current: &Self) -> bool {
        self.playing
            && current.playing
            && self.playback_epoch == current.playback_epoch
            && self.sequence_id == current.sequence_id
            && self.sequence_revision == current.sequence_revision
            && self.project_author_generation == current.project_author_generation
            && self.width == current.width
            && self.height == current.height
    }

    fn from_snapshot(
        snapshot: &PreviewExecutionSnapshot<'_>,
        sequence: &Sequence,
        frame: i64,
        width: u32,
        height: u32,
        display_color_space: ColorSpace,
        display_contract_identity: Option<DisplayOutputIdentity>,
    ) -> Self {
        let transport = snapshot.transport();
        let playing = transport.is_playing();
        Self {
            sequence_id: sequence.id,
            sequence_revision: sequence.revision,
            project_author_generation: snapshot
                .authoring()
                .map(PreviewAuthoringSnapshot::author_generation)
                .unwrap_or(0),
            display_contract_identity,
            playback_epoch: playing.then(|| transport.epoch()),
            still_frame: (!playing).then_some(frame),
            width,
            height,
            display_color_space,
            playing,
            seek_source: transport.seek_source(),
        }
    }
}

mod input;
pub(crate) use input::*;
mod frame_evaluation;
pub(crate) use frame_evaluation::*;
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
mod title_adapter;

use crate::app::preview_execution::PreviewOutputKey as ViewerPreviewCacheKey;

const fn visual_execution_drain_needs_follow_up(drained: usize) -> bool {
    drained == MEDIA_PREVIEW_MAX_COMPLETED_RESULTS_PER_POLL
}

impl<O: Clone> PreviewProductionRuntime<O> {
    fn consume_decode_residency_candidate_retry(&self) -> bool {
        let Some(access_mode) = self.decode_residency_waiting.get() else {
            return false;
        };
        if !self.decode_residency.admission_ready(access_mode) {
            return false;
        }
        self.decode_residency_waiting.set(None);
        self.observed_decode_residency_retry_revision
            .set(self.decode_residency.actionable_retry_revision());
        true
    }
}

impl<O: Clone> PlaybackPreviewAdapter for PreviewProductionRuntime<O> {
    fn poll_playback_work(
        &self,
        pending_demand: Option<mondrian_playback::FrameDemandIdentity>,
        transport_intent: PreviewTransportIntent,
    ) -> PreviewWorkPoll {
        self.synchronize_transport_intent(transport_intent);
        let mut outcome = self.poll_finished_outcome(pending_demand);
        let dependency_health_was_failed = self.visual_dependency_health_failed.get();
        match self.apply_visual_dependency_refreshes() {
            Ok(invalidated) => outcome.visible_change |= invalidated,
            Err(_) => {
                // The candidate path retains the typed unavailability. Here
                // only the first health transition owns repaint authority for
                // the output/cache invalidation performed by the health seam.
                outcome.visible_change |=
                    !dependency_health_was_failed && self.visual_dependency_health_failed.get();
            }
        }
        outcome.merge(self.pump_visual_execution_results(pending_demand));
        outcome.merge(self.pump_cpu_fallback_results());
        for candidate in self.take_visual_terminal_candidates(pending_demand) {
            if !outcome
                .frame_delivery_candidates
                .iter()
                .any(|existing| existing.identity() == candidate.identity())
            {
                outcome.frame_delivery_candidates.push(candidate);
            }
        }
        let title_poll = self.title_task.borrow_mut().poll_finished();
        outcome.candidate_retry_required |=
            title_poll.candidate_retry_required(self.execution.borrow().is_pending());
        outcome.needs_follow_up_poll |= title_poll.needs_follow_up_poll;
        if transport_intent.allows_playback_stall_expiration() {
            outcome.merge(self.expire_stalled_realtime_current(pending_demand));
        }
        outcome.candidate_retry_required |= self.consume_decode_residency_candidate_retry();
        self.try_release_settled_transport_media_residency();
        outcome
    }

    fn video_preroll(
        &self,
        request: PreviewVideoPrerollRequest<'_>,
    ) -> Option<PreviewVideoPreroll> {
        self.playback_video_preroll_readiness(request.snapshot(), request.proxy_demands())
    }
}

impl<O: Clone> Default for PreviewProductionRuntime<O> {
    fn default() -> Self {
        Self::new()
    }
}

impl<O: Clone> Drop for PreviewProductionRuntime<O> {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl<O: Clone> PreviewProductionRuntime<O> {
    /// Apply one immutable product resource decision.
    ///
    /// Preview remains admitted and realtime. Resource policy may release
    /// optional residency on a stronger trim transition; it never changes
    /// source, proxy, color, or terminal semantics. The decision's revision is
    /// diagnostic only and cannot suppress a changed policy.
    pub(crate) fn apply_resource_decision(
        &self,
        decision: &crate::app::execution_resource_coordination::PreviewExecutionDecision,
    ) {
        bump(&self.metrics.resource_decision_applications);
        self.heterogeneous_effect_decision.set(decision.heterogeneous_effects);
        self.frame_store.borrow_mut().reconfigure(decision.frame_store);
        self.title_task
            .borrow_mut()
            .set_cache_byte_budget(decision.title_cache_budget_bytes);
        self.visual_programs.borrow_mut().reconfigure(decision.visual_program_cache);
        {
            let mut scratch = self.scratch.borrow_mut();
            scratch.reconfigure_effect_execution(EffectExecutionSessionConfig {
                max_cache_entries: decision.effect_cache.max_entries,
                max_cache_bytes: decision.effect_cache.max_bytes,
                max_working_bytes: decision.effect_cache.max_working_bytes,
                max_gpu_plan_entries: decision.effect_cache.max_gpu_plan_entries,
                max_gpu_plan_bytes: decision.effect_cache.max_gpu_plan_bytes,
            });
            scratch.reconfigure_cpu_working_set(decision.cpu_composite_working_set);
            scratch.reconfigure_color_execution(decision.cpu_color_processor_capacity);
        }
        self.decode_worker_resources
            .seek_index_cache()
            .reconfigure(decision.seek_index_cache);
        self.decode_worker_resources
            .hardware_device_context_pool()
            .reconfigure(decision.hardware_device_contexts);
        let previous_trim = self.applied_resource_trim.replace(decision.trim);
        let stronger_trim = resource_trim_rank(decision.trim) > resource_trim_rank(previous_trim);
        if !stronger_trim {
            return;
        }
        match decision.trim {
            crate::app::execution_resource_coordination::ResourceTrimRequest::None => {}
            crate::app::execution_resource_coordination::ResourceTrimRequest::Speculative => {
                self.clear_decoder_resource_preview_residency();
                self.decode_worker_resources.hardware_device_context_pool().release_idle();
            }
            crate::app::execution_resource_coordination::ResourceTrimRequest::Aggressive => {
                self.clear_media_preview_residency();
                self.decode_worker_resources.seek_index_cache().clear();
                self.decode_worker_resources.hardware_device_context_pool().release_idle();
            }
        }
    }

    fn record_color_rejection(&self, rejection: PreviewColorRejection) {
        self.last_color_rejection.replace(Some(rejection));
    }

    fn unavailable_gpu_candidate(&self, reason: PreviewUnavailability) -> PreviewGpuFrameState {
        self.clear_terminal_viewer_state();
        bump(&self.metrics.gpu_preview_candidate_unavailable);
        self.unavailability_evidence.borrow_mut().observe(&reason);
        PreviewGpuFrameState::Unavailable(reason)
    }

    fn transparent_gpu_candidate(
        &self,
        snapshot: &PreviewExecutionSnapshot<'_>,
    ) -> PreviewGpuFrameState {
        self.clear_terminal_viewer_state();
        bump(&self.metrics.gpu_preview_candidate_transparent);
        PreviewGpuFrameState::Transparent(PreviewPresentationCandidate::new(
            (),
            self.playback_presentation_ticket(snapshot),
        ))
    }

    /// Terminal absence is a content discontinuity. Only pending work may
    /// retain a same-scope stale output for later presentation.
    fn clear_terminal_viewer_state(&self) {
        self.execution.borrow_mut().clear_output();
        self.frame_store.borrow_mut().clear_pinned_viewer_frame();
    }
}

fn resource_trim_rank(
    trim: crate::app::execution_resource_coordination::ResourceTrimRequest,
) -> u8 {
    match trim {
        crate::app::execution_resource_coordination::ResourceTrimRequest::None => 0,
        crate::app::execution_resource_coordination::ResourceTrimRequest::Speculative => 1,
        crate::app::execution_resource_coordination::ResourceTrimRequest::Aggressive => 2,
    }
}

/// Collect preview input color-resolution source counts for one timeline frame.
///
/// This uses the same preview-intent render-plan evaluation as the viewer. It
/// intentionally returns source counts only; output/display differences are
/// handled after Program Output and must not change input interpretation
/// source categories.
pub fn preview_input_color_resolution_counts_for_frame(
    sequence: &Sequence,
    sequences: &[Sequence],
    asset_executable_color_spaces: &HashMap<AssetId, ColorSpace>,
    asset_interpretations: &HashMap<AssetId, AssetMediaInterpretation>,
    color_environment: &mondrian_core::ProjectColorEnvironment,
    frame: i64,
) -> Result<InputColorResolutionSourceCounts, PreviewUnavailability> {
    let color_context = sequence.settings.root_program_color_context(color_environment);
    let target_resolution = preview_execution_resolution(
        sequence.settings.resolution,
        sequence.settings.preview.resolution_scale,
        mondrian_playback::PreviewResolutionScale::Full,
    );
    let demands = crate::app::preview_timeline_execution::collect_preview_timeline_media_demands(
        sequence,
        sequences,
        frame,
        target_resolution,
        mondrian_playback::PreviewResolutionScale::Full,
        color_context,
    )?;
    let mut counts = InputColorResolutionSourceCounts::default();
    for demand in demands {
        let executable_color_space = asset_executable_color_spaces.get(&demand.asset_id).copied();
        let asset_interpretation =
            asset_interpretations.get(&demand.asset_id).copied().unwrap_or_default();
        let resolution = resolve_preview_input_color_space(
            demand.color_space_override,
            asset_interpretation,
            executable_color_space,
            &demand.input_color,
        );
        counts.record(resolution.source);
    }
    Ok(counts)
}

#[derive(Default)]
struct PreviewMetrics {
    resource_decision_applications: Cell<u64>,
    render_requests: Cell<u64>,
    ready_frames: Cell<u64>,
    loading_frames: Cell<u64>,
    stale_frames: Cell<u64>,
    unavailable_frames: Cell<u64>,
    playback_current_stalled_expirations: Cell<u64>,
    /// Total authoritative timeline resolves through the evaluation
    /// coordinator; deduplicated acquires do not count.
    timeline_resolve_count: Cell<u64>,
    timeline_evaluation_hits: Cell<u64>,
    timeline_evaluation_wait_hits: Cell<u64>,
    timeline_evaluation_misses: Cell<u64>,
    gpu_preview_candidate_requests: Cell<u64>,
    gpu_preview_candidate_ready: Cell<u64>,
    gpu_preview_candidate_current: Cell<u64>,
    gpu_preview_candidate_transparent: Cell<u64>,
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
    decode_cancellation_checkpoints: Cell<mondrian_media::PreviewDecodeCancellationEvidence>,
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
    decode_expired_queue_wait: Cell<PreviewDecodeQueueWaitProfile>,
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
    decode_max_frame_bottleneck: Cell<PreviewDecodeBottleneck>,
    decode_access_mode_profiles: Cell<PreviewDecodeAccessModeProfiles>,
    render_timed_frames: Cell<u64>,
    render_total_duration_us: Cell<u64>,
    render_max_duration_us: Cell<u64>,
    render_last_duration_us: Cell<u64>,
    render_stage_durations: Cell<PreviewRenderStageDurations>,
    render_max_frame_stage_durations: Cell<PreviewRenderStageDurations>,
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

#[cfg(test)]
fn preview_dimensions_for_sequence(sequence: &Sequence) -> (u32, u32) {
    preview_dimensions_for_sequence_at_runtime_scale(
        sequence,
        mondrian_playback::PreviewResolutionScale::Full,
    )
}

fn preview_dimensions_for_snapshot(
    snapshot: &PreviewExecutionSnapshot<'_>,
    sequence: &Sequence,
) -> (u32, u32) {
    preview_dimensions_for_sequence_at_runtime_scale(sequence, snapshot.transport().runtime_scale())
}

fn preview_dimensions_for_sequence_at_runtime_scale(
    sequence: &Sequence,
    runtime_scale: mondrian_playback::PreviewResolutionScale,
) -> (u32, u32) {
    let resolution = preview_execution_resolution(
        sequence.settings.resolution,
        sequence.settings.preview.resolution_scale,
        runtime_scale,
    );
    (resolution.width, resolution.height)
}

fn preview_display_color_space(
    sequence: &Sequence,
    viewer_display_management: &mondrian_core::DisplayManagementPolicy,
    display_snapshot: Option<&DisplayOutputSnapshot>,
) -> Result<ColorSpace, crate::app::preview_gpu_output_blocker::PreviewGpuOutputBlocker> {
    let sequence_output = sequence.settings.color.program_output.color_space;
    let profile_space = match viewer_display_management.monitor_profile {
        mondrian_core::MonitorProfileReference::IccProfile { .. } => {
            preview_icc_display_color_space(display_snapshot)?
        }
        ref monitor => monitor.managed_color_space(sequence_output).unwrap_or(sequence_output),
    };

    Ok(
        match viewer_display_management.viewer_mode.resolve(profile_space) {
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
        return Err(preview_blockers_from_snapshot(snapshot)
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

const fn worker_disconnect_is_terminal_health_failure(
    configured_worker_count: usize,
    shutdown_requested: bool,
    health_already_failed: bool,
) -> bool {
    configured_worker_count > 0 && !shutdown_requested && !health_already_failed
}

#[cfg(test)]
mod worker_health_contract_tests {
    use super::worker_disconnect_is_terminal_health_failure;

    #[test]
    fn only_first_unexpected_configured_worker_disconnect_is_terminal() {
        assert!(worker_disconnect_is_terminal_health_failure(
            1, false, false
        ));
        assert!(!worker_disconnect_is_terminal_health_failure(
            0, false, false
        ));
        assert!(!worker_disconnect_is_terminal_health_failure(
            1, true, false
        ));
        assert!(!worker_disconnect_is_terminal_health_failure(
            1, false, true
        ));
    }
}

#[cfg(test)]
#[path = "preview_runtime/tests.rs"]
mod tests;
