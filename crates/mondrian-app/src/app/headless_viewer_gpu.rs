//! Headless Viewer GPU presentation Adapter used by execution/performance gates.
//!
//! This Adapter owns a real wgpu device and calls the production
//! [`ViewerGpuExecutionRuntime`] Interface. It deliberately has no UI texture
//! registry; successful completion means commands were submitted and the GPU
//! queue reached the recorded presentation output.

use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use std::time::Duration;
use std::time::Instant;

use crate::app::execution_resource_coordination::{
    apply_preview_viewer_gpu_resource_decision, PreviewViewerGpuExecutionDecision,
};
use crate::app::preview_execution::{
    PreviewDecodeExecutionSummary, PreviewGpuFrame, PreviewGpuHeterogeneousExecution,
    PreviewGpuWorkingInput,
};
use crate::app::viewer_gpu_device_progress::{
    ViewerGpuDeviceGenerationMember, ViewerGpuDeviceGenerationRetirement,
    ViewerGpuDeviceGenerationTerminal, ViewerGpuDeviceProgressObservation,
    ViewerGpuDeviceProgressOwner, ViewerGpuDeviceProgressReserveError,
    ViewerGpuDeviceProgressStartError, ViewerGpuDeviceProgressWake,
};
use crate::app::viewer_gpu_publication::{ViewerGpuPhysicalPublication, ViewerGpuPublicationSlots};
use crate::app::viewer_gpu_submission::{
    ViewerGpuCompletedSubmission, ViewerGpuRetiredSubmission, ViewerGpuSubmissionAdmissionError,
    ViewerGpuSubmissionId, ViewerGpuSubmissionLifecycle, ViewerGpuSubmissionPoll,
    ViewerGpuSubmissionQuarantine, ViewerGpuSubmissionQuarantineReason,
};
use crate::app::FramePresentationDisposition;
#[cfg(test)]
use mondrian_renderer::profile::GpuTimestampSample;
#[cfg(test)]
use mondrian_renderer::NativeVideoImportGpuTimingDiagnostics;
use mondrian_renderer::{
    native_video_texture_device_features, ocio_lut_filtering_device_features,
    profile::gpu_timestamp_query_device_features,
    profile::{GpuTimestampQueryRing, GpuTimestampStageMarker, GpuTimestampToken},
    request_adapter_with_native_video_preference, GpuCompositingDiagnostics,
    GpuCompositorTextureBindingDiagnostics, GpuCompositorUniformArenaDiagnostics,
    GpuNativeDecodedFrameImportSupport, GpuViewerSpatialRuntimeDiagnostics,
    NativeVideoImportCandidateTimingReceipt, NativeVideoImportGpuTimingPolicy,
    NativeVideoImportGpuTimingSample, RenderColorStageDiagnostics,
    ViewerGpuExecutionCpuStageTimings, ViewerGpuExecutionGpuStage, ViewerGpuExecutionRequest,
    ViewerGpuExecutionRuntime, ViewerGpuExecutionRuntimeCreateError, ViewerGpuExecutionStageMarker,
    ViewerGpuOutputPrecision, ViewerGpuPresentationOutputLease,
    ViewerHeterogeneousGpuCompletedBatch, ViewerSourceRect,
};
const HEADLESS_GPU_TIMESTAMP_RING_CAPACITY: usize = 16;
const HEADLESS_NATIVE_IMPORT_GPU_TIMING_DEFAULT_OBSERVATION_CAPACITY: usize = 65_536;
const HEADLESS_NATIVE_IMPORT_GPU_TIMING_MAX_OBSERVATION_CAPACITY: usize = 1_048_576;
static NEXT_HEADLESS_NATIVE_IMPORT_GPU_TIMING_SESSION_ID: AtomicU64 = AtomicU64::new(1);

/// Safety deadline for proving completion of already submitted Headless GPU work.
///
/// This is intentionally not a frame-presentation deadline. Presentation
/// timeliness remains on the exact [`mondrian_playback::FramePresentationTicket`];
/// this deadline only bounds how long the Adapter may retain an unproved GPU
/// submission before it fails closed and quarantines its resources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HeadlessGpuCompletionDeadline(Instant);

impl HeadlessGpuCompletionDeadline {
    /// Bind an existing absolute safety deadline.
    pub(crate) const fn at(deadline: Instant) -> Self {
        Self(deadline)
    }

    /// Derive a safety deadline independently from a frame cadence deadline.
    #[cfg(test)]
    pub(crate) fn after(now: Instant, timeout: Duration) -> Result<Self, HeadlessViewerGpuError> {
        now.checked_add(timeout)
            .map(Self)
            .ok_or(HeadlessViewerGpuError::DeadlineExceeded)
    }

    const fn instant(self) -> Instant {
        self.0
    }
}

/// Process-local identity of one Headless native-import timing session.
///
/// Renderer candidate/import tokens are runtime-local, so serialized evidence
/// must carry this identity to remain unambiguous across adapter rebuilds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(transparent)]
pub(crate) struct HeadlessNativeVideoImportGpuTimingSessionId(u64);

impl HeadlessNativeVideoImportGpuTimingSessionId {
    #[cfg(test)]
    pub(crate) const fn get(self) -> u64 {
        self.0
    }
}

/// Cloneable App evidence derived once from a renderer move-only receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub(crate) struct HeadlessNativeVideoImportGpuTimingReceipt {
    pub(crate) session_id: HeadlessNativeVideoImportGpuTimingSessionId,
    pub(crate) candidate_token: u64,
    pub(crate) submitted_imports: u64,
    pub(crate) scheduled_samples: u64,
    pub(crate) missing_samples: u64,
    pub(crate) dropped_samples: u64,
}

/// Session-qualified native-import hardware timing sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub(crate) struct HeadlessNativeVideoImportGpuTimingSample {
    pub(crate) session_id: HeadlessNativeVideoImportGpuTimingSessionId,
    pub(crate) candidate_token: u64,
    pub(crate) import_token: u64,
    pub(crate) yuv_decode_marker_bracket_us: u64,
    pub(crate) input_color_marker_bracket_us: u64,
    pub(crate) decode_fence_ready_at_admission: Option<bool>,
}

/// Point-in-time renderer coverage plus bounded App observation health.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[cfg(test)]
pub(crate) struct HeadlessNativeVideoImportGpuTimingDiagnostics {
    pub(crate) session_id: HeadlessNativeVideoImportGpuTimingSessionId,
    pub(crate) renderer: NativeVideoImportGpuTimingDiagnostics,
    pub(crate) candidate_receipts: u64,
    pub(crate) observation_capacity: usize,
    pub(crate) buffered_samples: usize,
    pub(crate) drained_samples: u64,
    pub(crate) adapter_overflow_samples: u64,
}

/// Final no-additional-wait native-import timing evidence.
///
/// Callers first finish the Viewer suffix timing interval. That existing wait
/// drives the device once; this value then drains only already-collected
/// native-prefix observations.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[cfg(test)]
pub(crate) struct HeadlessNativeVideoImportGpuTimingFinalEvidence {
    pub(crate) diagnostics: HeadlessNativeVideoImportGpuTimingDiagnostics,
    pub(crate) samples: Vec<HeadlessNativeVideoImportGpuTimingSample>,
}

/// Evidence for one real headless Viewer GPU execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HeadlessViewerGpuExecution {
    /// Exact process-local GPU queue submission identity.
    pub submission_id: u64,
    /// Completed Adapter artifact. It becomes usable only after the visual
    /// lifecycle and exact presentation ticket both authorize publication.
    pub output: HeadlessViewerGpuOutput,
    /// Exact output width submitted to the shared Viewer GPU Runtime.
    pub output_width: u32,
    /// Exact output height submitted to the shared Viewer GPU Runtime.
    pub output_height: u32,
    /// Wall time through recording, submission, and any observed completion.
    pub duration_us: u64,
    /// CPU wall time through command recording and queue submission.
    pub record_submit_us: u64,
    /// CPU wall time waiting for the submitted presentation work to complete.
    pub completion_wait_us: u64,
    /// Whether completion of this exact output was observed before returning.
    pub gpu_completion_observed: bool,
    /// Deferred timestamp token, absent if unsupported or the bounded ring discarded it.
    pub gpu_timestamp_token: Option<u64>,
    /// CPU attribution inside the renderer record call.
    pub cpu_stage_timings: Option<ViewerGpuExecutionCpuStageTimings>,
    /// Session-qualified native-import candidate coverage, derived once from
    /// the renderer's move-only receipt.
    pub native_import_gpu_timing_receipt: Option<HeadlessNativeVideoImportGpuTimingReceipt>,
    /// Frame-local working-space compositing evidence.
    pub compositing_diagnostics: Option<GpuCompositingDiagnostics>,
    /// Cumulative bounded uniform-arena reuse evidence after this frame.
    pub compositor_uniform_arena: Option<GpuCompositorUniformArenaDiagnostics>,
    /// Cumulative compositor texture-binding reuse evidence after this frame.
    pub compositor_texture_bindings: Option<GpuCompositorTextureBindingDiagnostics>,
    /// Cumulative spatial-runtime evidence after this frame.
    pub spatial_diagnostics: Option<GpuViewerSpatialRuntimeDiagnostics>,
    /// Structured GPU color-stage evidence for a newly rendered output.
    pub stage_diagnostics: Option<RenderColorStageDiagnostics>,
    /// Explicit native/GPU-input fallback reasons for a newly rendered output.
    pub fallback_reasons: Vec<String>,
    /// Frame-local decode provenance bound to this exact Viewer candidate.
    pub decode_execution: PreviewDecodeExecutionSummary,
    /// Native-import contract pools retained after this execution.
    pub native_import_contract_pools: usize,
    /// Native-import bridge entries retained after this execution.
    pub native_import_bridge_entries: usize,
    /// Decoder sources still retained after observing GPU completion.
    pub native_import_retained_sources: usize,
    /// Other submitted owners still active after this exact completion.
    ///
    /// Native sources retained in this state belong to later pipelined work;
    /// only residency observed with this value at zero is an orphan/leak.
    pub remaining_submissions_after_completion: usize,
}

/// One completed Headless Viewer candidate plus optional typed heterogeneous
/// GPU terminal evidence awaiting Preview Broker resolution.
pub(crate) struct HeadlessViewerGpuCompletedCandidate {
    pub(crate) submission_id: ViewerGpuSubmissionId,
    pub(crate) frame: PreviewGpuFrame,
    pub(crate) execution: HeadlessViewerGpuExecution,
    pub(crate) heterogeneous_completion: Option<ViewerHeterogeneousGpuCompletedBatch>,
    pub(crate) queued_publication: Option<FramePresentationDisposition>,
    pub(crate) successor_prepared: bool,
    pub(crate) quarantine_reason: Option<ViewerGpuSubmissionQuarantineReason>,
    pub(crate) completion_error: Option<String>,
    pub(crate) revoked_current_physical_output: Option<HeadlessViewerGpuOutput>,
    presentation_lease: Option<ViewerGpuPresentationOutputLease>,
}

/// Successful queue admission for one exact Headless candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HeadlessViewerGpuSubmittedCandidate {
    pub(crate) submission_id: ViewerGpuSubmissionId,
    pub(crate) heterogeneous: bool,
}

/// One non-blocking observation of the real Headless GPU submission slot.
pub(crate) enum HeadlessViewerGpuCompletionPoll {
    /// No submitted owner remains.
    Idle,
    /// One owner remains retained until its exact callback.
    Pending {
        submission_id: ViewerGpuSubmissionId,
        quarantined: bool,
    },
    /// Completion made the exact retained owner safe to retire.
    Completed(Box<HeadlessViewerGpuCompletedCandidate>),
    /// Publication authority was revoked while ownership stays retained.
    QuarantineStarted {
        quarantine: ViewerGpuSubmissionQuarantine,
        revoked_current_physical_output: Option<HeadlessViewerGpuOutput>,
    },
    /// A quarantined submission never produced its callback within the bounded
    /// grace; the slot was force-released and the owner must be retired.
    RetiredAfterQuarantine(Box<HeadlessViewerGpuRetiredCandidate>),
}

/// One force-retired Headless owner whose exact callback never arrived.
pub(crate) struct HeadlessViewerGpuRetiredCandidate {
    pub(crate) submission_id: ViewerGpuSubmissionId,
    pub(crate) frame: PreviewGpuFrame,
    pub(crate) queued_publication: Option<FramePresentationDisposition>,
    pub(crate) successor_prepared: bool,
    pub(crate) quarantine_reason: ViewerGpuSubmissionQuarantineReason,
    pub(crate) completion_error: Option<String>,
    pub(crate) revoked_current_physical_output: Option<HeadlessViewerGpuOutput>,
    pub(crate) presentation_lease: Option<ViewerGpuPresentationOutputLease>,
}

/// Opaque usable output payload registered with the production Preview Runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HeadlessViewerGpuOutput {
    /// Renderer resource identity retained by the Headless Adapter.
    pub resource_key: String,
    /// Presented output width.
    pub width: u32,
    /// Presented output height.
    pub height: u32,
}

/// Frame-level completion safety deadline for one Headless Viewer submission.
///
/// The exact GPU work for one Viewer frame is millisecond-scale; a bounded
/// safety window must therefore be much shorter than any caller observation
/// deadline. When the work-done callback is lost, this deadline starts the
/// quarantine and the bounded release grace frees the exact slot, so a
/// single lost callback cannot stall the pipeline for the caller's full wait.
pub(crate) const HEADLESS_GPU_COMPLETION_SAFETY_DEADLINE: std::time::Duration =
    std::time::Duration::from_secs(2);

/// Stable adapter identity serialized by real-GPU execution gates.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct HeadlessViewerGpuAdapterInfo {
    pub name: String,
    pub vendor: u32,
    pub device: u32,
    pub device_type: String,
    pub backend: String,
    pub driver: String,
    pub driver_info: String,
}

/// Real no-Surface Adapter over the shared Viewer GPU Preview Runtime.
pub(crate) struct HeadlessViewerGpuAdapter {
    // `Drop` transfers these move-only members to the non-caller progress
    // domain; Headless teardown does not synchronously poll or join.
    device_progress: ViewerGpuDeviceGenerationMember<ViewerGpuDeviceProgressOwner>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    runtime: ViewerGpuDeviceGenerationMember<ViewerGpuExecutionRuntime>,
    timestamp_ring: Option<GpuTimestampQueryRing>,
    adapter_info: HeadlessViewerGpuAdapterInfo,
    native_import_gpu_timing: HeadlessNativeVideoImportGpuTimingSession,
    reported_orphaned_completion_count: u64,
    submission_lifecycle: ViewerGpuSubmissionLifecycle<
        HeadlessViewerGpuSubmissionOwner,
        ViewerHeterogeneousGpuCompletedBatch,
    >,
    physical_outputs: ViewerGpuPublicationSlots<
        crate::app::preview_execution::PreviewOutputKey,
        HeadlessViewerGpuOutput,
        ViewerGpuPresentationOutputLease,
    >,
}

struct HeadlessViewerGpuSubmissionOwner {
    frame: PreviewGpuFrame,
    execution: HeadlessViewerGpuExecution,
    queued_publication: Option<FramePresentationDisposition>,
    successor_prepared: bool,
    presentation_lease: Option<ViewerGpuPresentationOutputLease>,
    started_at: Instant,
    completion_started_at: Instant,
}

struct HeadlessNativeVideoImportGpuTimingSession {
    id: HeadlessNativeVideoImportGpuTimingSessionId,
    observation_capacity: usize,
    completed: Vec<HeadlessNativeVideoImportGpuTimingSample>,
    #[cfg(test)]
    candidate_receipts: u64,
    #[cfg(test)]
    drained_samples: u64,
    #[cfg(test)]
    adapter_overflow_samples: u64,
    #[cfg(test)]
    offline_completion_poll_observed: bool,
}

impl HeadlessNativeVideoImportGpuTimingSession {
    fn new(observation_capacity: usize) -> Result<Self, HeadlessViewerGpuError> {
        let id = allocate_headless_native_import_gpu_timing_session_id()?;
        Ok(Self {
            id,
            observation_capacity,
            completed: Vec::with_capacity(observation_capacity.min(4_096)),
            #[cfg(test)]
            candidate_receipts: 0,
            #[cfg(test)]
            drained_samples: 0,
            #[cfg(test)]
            adapter_overflow_samples: 0,
            #[cfg(test)]
            offline_completion_poll_observed: false,
        })
    }

    fn bind_receipt(
        &mut self,
        receipt: NativeVideoImportCandidateTimingReceipt,
    ) -> HeadlessNativeVideoImportGpuTimingReceipt {
        #[cfg(test)]
        {
            self.candidate_receipts = self.candidate_receipts.saturating_add(1);
        }
        HeadlessNativeVideoImportGpuTimingReceipt {
            session_id: self.id,
            candidate_token: receipt.candidate_token().get(),
            submitted_imports: receipt.submitted_imports(),
            scheduled_samples: receipt.scheduled_samples(),
            missing_samples: receipt.missing_samples(),
            dropped_samples: receipt.dropped_samples(),
        }
    }

    fn accept_samples(&mut self, samples: Vec<NativeVideoImportGpuTimingSample>) {
        for sample in samples {
            self.accept_sample(HeadlessNativeVideoImportGpuTimingSample {
                session_id: self.id,
                candidate_token: sample.candidate_token.get(),
                import_token: sample.import_token.get(),
                yuv_decode_marker_bracket_us: sample.yuv_decode_marker_bracket_us,
                input_color_marker_bracket_us: sample.input_color_marker_bracket_us,
                decode_fence_ready_at_admission: sample.decode_fence_ready_at_admission,
            });
        }
    }

    fn accept_sample(&mut self, sample: HeadlessNativeVideoImportGpuTimingSample) {
        if self.completed.len() >= self.observation_capacity {
            #[cfg(test)]
            {
                self.adapter_overflow_samples = self.adapter_overflow_samples.saturating_add(1);
            }
            return;
        }
        self.completed.push(sample);
    }

    #[cfg(test)]
    fn drain(&mut self) -> Vec<HeadlessNativeVideoImportGpuTimingSample> {
        let completed = std::mem::take(&mut self.completed);
        self.drained_samples = self.drained_samples.saturating_add(completed.len() as u64);
        completed
    }

    #[cfg(test)]
    fn diagnostics(
        &self,
        renderer: NativeVideoImportGpuTimingDiagnostics,
    ) -> HeadlessNativeVideoImportGpuTimingDiagnostics {
        HeadlessNativeVideoImportGpuTimingDiagnostics {
            session_id: self.id,
            renderer,
            candidate_receipts: self.candidate_receipts,
            observation_capacity: self.observation_capacity,
            buffered_samples: self.completed.len(),
            drained_samples: self.drained_samples,
            adapter_overflow_samples: self.adapter_overflow_samples,
        }
    }
}

type HeadlessViewerGpuOutputSlots = ViewerGpuPublicationSlots<
    crate::app::preview_execution::PreviewOutputKey,
    HeadlessViewerGpuOutput,
    ViewerGpuPresentationOutputLease,
>;

struct HeadlessViewerGpuGenerationRetirement {
    runtime: ViewerGpuExecutionRuntime,
    lifecycle: ViewerGpuSubmissionLifecycle<
        HeadlessViewerGpuSubmissionOwner,
        ViewerHeterogeneousGpuCompletedBatch,
    >,
    _device: wgpu::Device,
    _queue: wgpu::Queue,
    _timestamp_ring: Option<GpuTimestampQueryRing>,
    _physical_outputs: HeadlessViewerGpuOutputSlots,
    _completed_submissions: Vec<
        ViewerGpuCompletedSubmission<
            HeadlessViewerGpuSubmissionOwner,
            ViewerHeterogeneousGpuCompletedBatch,
        >,
    >,
    _lost_submission_owners: Vec<HeadlessViewerGpuSubmissionOwner>,
    native_retirement_error_logged: bool,
    native_device_removed_logged: bool,
}

impl ViewerGpuDeviceGenerationRetirement for HeadlessViewerGpuGenerationRetirement {
    fn label(&self) -> &'static str {
        "Headless Viewer GPU device generation"
    }

    fn poll_retirement(&mut self, terminal: Option<&ViewerGpuDeviceGenerationTerminal>) -> bool {
        let native_progress_proved = match self.runtime.retire_completed_native_import_sources() {
            Ok(_) => true,
            Err(error) if error.is_native_device_removed() => {
                if !self.native_device_removed_logged {
                    tracing::warn!(
                        %error,
                        "Headless Viewer GPU retirement accepted typed native device-removal proof"
                    );
                    self.native_device_removed_logged = true;
                }
                true
            }
            Err(error) => {
                if !self.native_retirement_error_logged {
                    tracing::error!(
                        %error,
                        "Headless Viewer GPU retirement could not prove native copy-fence progress"
                    );
                    self.native_retirement_error_logged = true;
                }
                false
            }
        };
        let native_copy_ready =
            native_progress_proved && self.runtime.native_import_retained_source_count() == 0;

        match self.lifecycle.poll(Instant::now()) {
            ViewerGpuSubmissionPoll::Completed(completed) => {
                self._completed_submissions.push(completed);
            }
            ViewerGpuSubmissionPoll::RetiredAfterQuarantine(retired) => {
                tracing::warn!(
                    submission_id = retired.submission_id.get(),
                    reason = ?retired.reason,
                    "Headless Viewer force-retired a quarantined GPU submission whose completion callback was lost"
                );
                self._lost_submission_owners.push(retired.owner);
            }
            ViewerGpuSubmissionPoll::Idle
            | ViewerGpuSubmissionPoll::Pending { .. }
            | ViewerGpuSubmissionPoll::QuarantineStarted(_) => {}
        }

        if native_copy_ready
            && terminal.is_some_and(ViewerGpuDeviceGenerationTerminal::wgpu_work_is_terminal)
            && self.lifecycle.is_occupied()
        {
            self._lost_submission_owners
                .extend(self.lifecycle.retire_owners_after_wgpu_device_loss());
        }

        native_copy_ready && !self.lifecycle.is_occupied()
    }
}

impl Drop for HeadlessViewerGpuAdapter {
    fn drop(&mut self) {
        let Some(progress) = self.device_progress.take() else {
            return;
        };
        let Some(runtime) = self.runtime.take() else {
            tracing::error!(
                "Headless Viewer GPU teardown lost its execution runtime; retaining progress authority indefinitely"
            );
            std::mem::forget(progress);
            return;
        };
        let retirement = HeadlessViewerGpuGenerationRetirement {
            runtime,
            lifecycle: std::mem::replace(
                &mut self.submission_lifecycle,
                ViewerGpuSubmissionLifecycle::new(),
            ),
            _device: self.device.clone(),
            _queue: self.queue.clone(),
            _timestamp_ring: self.timestamp_ring.take(),
            _physical_outputs: std::mem::take(&mut self.physical_outputs),
            _completed_submissions: Vec::new(),
            _lost_submission_owners: Vec::new(),
            native_retirement_error_logged: false,
            native_device_removed_logged: false,
        };
        progress.retire_device_generation(retirement);
    }
}

struct HeadlessGpuStageMarker<'a> {
    ring: &'a mut GpuTimestampQueryRing,
    token: GpuTimestampToken,
}

impl ViewerGpuExecutionStageMarker for HeadlessGpuStageMarker<'_> {
    fn mark(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        stage: ViewerGpuExecutionGpuStage,
    ) -> Result<(), String> {
        let marker = match stage {
            ViewerGpuExecutionGpuStage::WorkingComposite => {
                GpuTimestampStageMarker::AfterWorkingComposite
            }
            ViewerGpuExecutionGpuStage::Spatial => GpuTimestampStageMarker::AfterSpatial,
            ViewerGpuExecutionGpuStage::ProgramOutputBoundary => {
                GpuTimestampStageMarker::AfterProgramOutputBoundary
            }
            ViewerGpuExecutionGpuStage::ProgramScopes => {
                GpuTimestampStageMarker::AfterProgramScopes
            }
            ViewerGpuExecutionGpuStage::MonitorAdaptation => {
                GpuTimestampStageMarker::AfterMonitorAdaptation
            }
        };
        self.ring
            .mark_stage(encoder, self.token, marker)
            .map_err(|error| error.to_string())
    }
}

impl HeadlessViewerGpuAdapter {
    /// Create a high-performance headless device with the same native-video
    /// feature selection used by the production Window Adapter. Native-import
    /// GPU timing is explicitly disabled by default.
    pub(crate) fn new() -> Result<Self, HeadlessViewerGpuError> {
        Self::new_with_native_import_gpu_timing_policy(NativeVideoImportGpuTimingPolicy::Disabled)
    }

    /// Create a Headless adapter with explicit renderer native-import timing.
    ///
    /// The default App observation bound is suitable for short validation
    /// intervals. Long-running gates must choose an explicit bound from their
    /// frozen workload instead of relying on this convenience constructor.
    pub(crate) fn new_with_native_import_gpu_timing_policy(
        policy: NativeVideoImportGpuTimingPolicy,
    ) -> Result<Self, HeadlessViewerGpuError> {
        let observation_capacity = match policy {
            NativeVideoImportGpuTimingPolicy::Disabled => 0,
            NativeVideoImportGpuTimingPolicy::Enabled { .. } => {
                HEADLESS_NATIVE_IMPORT_GPU_TIMING_DEFAULT_OBSERVATION_CAPACITY
            }
        };
        Self::new_with_native_import_gpu_timing_policy_and_observation_capacity(
            policy,
            observation_capacity,
        )
    }

    /// Create a Headless adapter with independent renderer-ring and App
    /// observation bounds.
    pub(crate) fn new_with_native_import_gpu_timing_policy_and_observation_capacity(
        policy: NativeVideoImportGpuTimingPolicy,
        observation_capacity: usize,
    ) -> Result<Self, HeadlessViewerGpuError> {
        validate_native_import_gpu_timing_observation_capacity(policy, observation_capacity)?;
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = pollster::block_on(request_adapter_with_native_video_preference(
            &instance,
            &wgpu::RequestAdapterOptions {
                compatible_surface: None,
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                ..wgpu::RequestAdapterOptions::default()
            },
        ))
        .map_err(|error| HeadlessViewerGpuError::Adapter(error.to_string()))?;
        let supported_features = adapter.features();
        let raw_adapter_info = adapter.get_info();
        let viewer_suffix_timing_features = gpu_timestamp_query_device_features(supported_features);
        let native_import_timing_features = match policy {
            NativeVideoImportGpuTimingPolicy::Disabled => wgpu::Features::empty(),
            NativeVideoImportGpuTimingPolicy::Enabled { .. } => {
                gpu_timestamp_query_device_features(supported_features)
            }
        };
        let descriptor = wgpu::DeviceDescriptor {
            required_features: native_video_texture_device_features(supported_features)
                | ocio_lut_filtering_device_features(supported_features)
                | viewer_suffix_timing_features
                | native_import_timing_features,
            ..wgpu::DeviceDescriptor::default()
        };
        let (device, queue) = pollster::block_on(adapter.request_device(&descriptor))
            .map_err(|error| HeadlessViewerGpuError::Device(error.to_string()))?;
        // Install the one callback for this generation before constructing any
        // runtime, timestamp, or queue-consuming Adapter component.
        let device_progress =
            ViewerGpuDeviceProgressOwner::new(&device, ViewerGpuDeviceProgressWake::default())?;
        let runtime = ViewerGpuExecutionRuntime::new_with_native_import_gpu_timing_policy(
            &adapter, &device, &queue, policy,
        )?;
        let timestamp_ring =
            GpuTimestampQueryRing::new(&device, &queue, HEADLESS_GPU_TIMESTAMP_RING_CAPACITY);
        let native_import_gpu_timing =
            HeadlessNativeVideoImportGpuTimingSession::new(observation_capacity)?;
        let adapter_info = HeadlessViewerGpuAdapterInfo {
            name: raw_adapter_info.name,
            vendor: raw_adapter_info.vendor,
            device: raw_adapter_info.device,
            device_type: format!("{:?}", raw_adapter_info.device_type),
            backend: format!("{:?}", raw_adapter_info.backend),
            driver: raw_adapter_info.driver,
            driver_info: raw_adapter_info.driver_info,
        };
        Ok(Self {
            device_progress: ViewerGpuDeviceGenerationMember::new(device_progress),
            device,
            queue,
            runtime: ViewerGpuDeviceGenerationMember::new(runtime),
            timestamp_ring,
            adapter_info,
            native_import_gpu_timing,
            reported_orphaned_completion_count: 0,
            submission_lifecycle: ViewerGpuSubmissionLifecycle::new(),
            physical_outputs: ViewerGpuPublicationSlots::default(),
        })
    }

    /// Adapter identity bound to this execution device.
    pub(crate) fn adapter_info(&self) -> &HeadlessViewerGpuAdapterInfo {
        &self.adapter_info
    }

    /// Exact App session qualifying renderer-local native-import tokens.
    #[cfg(test)]
    pub(crate) const fn native_import_gpu_timing_session_id(
        &self,
    ) -> HeadlessNativeVideoImportGpuTimingSessionId {
        self.native_import_gpu_timing.id
    }

    /// Cumulative renderer coverage and bounded Adapter observation health.
    #[cfg(test)]
    pub(crate) fn native_import_gpu_timing_diagnostics(
        &self,
    ) -> HeadlessNativeVideoImportGpuTimingDiagnostics {
        let mut diagnostics = self
            .native_import_gpu_timing
            .diagnostics(self.runtime.native_import_gpu_timing_diagnostics());
        diagnostics.session_id = self.native_import_gpu_timing_session_id();
        diagnostics.adapter_overflow_samples =
            self.native_import_gpu_timing_adapter_overflow_samples();
        diagnostics
    }

    /// Drain only samples already observed after an existing device poll.
    ///
    /// This method never polls or waits for the device and therefore cannot
    /// change publication latency or deadline classification.
    #[cfg(test)]
    pub(crate) fn drain_native_import_gpu_timing_samples(
        &mut self,
    ) -> Vec<HeadlessNativeVideoImportGpuTimingSample> {
        self.native_import_gpu_timing.drain()
    }

    /// Adapter samples discarded because the explicit observation buffer was
    /// full. Renderer-ring drops remain separately visible in renderer
    /// diagnostics.
    #[cfg(test)]
    pub(crate) const fn native_import_gpu_timing_adapter_overflow_samples(&self) -> u64 {
        self.native_import_gpu_timing.adapter_overflow_samples
    }

    /// Install the payload-free Preview work-watch edge used by exact GPU
    /// completion callbacks. The callback carries no completion authority;
    /// consumers still drain this Adapter's typed lifecycle.
    pub(crate) fn install_completion_waker(&mut self, waker: impl Fn() + Send + Sync + 'static) {
        self.device_progress.install_waker(waker);
    }

    /// Native import support exposed to the preview scheduling Adapter.
    pub(crate) fn native_import_support(&self) -> GpuNativeDecodedFrameImportSupport {
        self.runtime.native_import_support()
    }

    /// Apply the same immutable Preview grant used by the Window Viewer owner.
    pub(crate) fn apply_resource_decision(
        &mut self,
        decision: &PreviewViewerGpuExecutionDecision,
    ) -> Result<(), HeadlessViewerGpuError> {
        // Reconfiguration may trim renderer pools still referenced by the
        // exact submitted frame-resource owner.
        if self.submission_lifecycle.is_occupied() {
            return Err(HeadlessViewerGpuError::Backpressure(
                "the Viewer GPU submission slot still owns frame resources".to_owned(),
            ));
        }
        apply_preview_viewer_gpu_resource_decision(&mut *self.runtime, decision);
        Ok(())
    }

    /// Finish deferred timestamp maps after the measured playback interval.
    #[cfg(test)]
    pub(crate) fn finish_gpu_timings(
        &mut self,
    ) -> Result<Vec<GpuTimestampSample>, HeadlessViewerGpuError> {
        if self.submission_lifecycle.is_occupied() {
            return Err(HeadlessViewerGpuError::Backpressure(
                "cannot finish timestamp maps while a Viewer GPU submission is in flight"
                    .to_owned(),
            ));
        }
        let native_timing_activated = self.runtime.native_import_gpu_timing_diagnostics().activated;
        let suffix_wait_attempted = self.timestamp_ring.is_some();
        let suffix_result = if let Some(ring) = &mut self.timestamp_ring {
            ring.finish_all(&self.device)
                .map_err(|error| HeadlessViewerGpuError::Timestamp(error.to_string()))
        } else if native_timing_activated {
            Err(HeadlessViewerGpuError::NativeImportTimingSuffixUnavailable)
        } else {
            Ok(Vec::new())
        };
        if suffix_wait_attempted {
            self.collect_native_import_gpu_timings_after_device_poll();
        }
        if suffix_wait_attempted && suffix_result.is_ok() {
            self.native_import_gpu_timing.offline_completion_poll_observed = true;
        }
        suffix_result
    }

    /// Final native-prefix evidence after [`Self::finish_gpu_timings`] supplied
    /// the session's one offline completion wait.
    ///
    /// This method performs no second `Device::poll` and is therefore safe to
    /// pair with the existing suffix timing gate.
    #[cfg(test)]
    pub(crate) fn finish_native_import_gpu_timings(
        &mut self,
    ) -> Result<HeadlessNativeVideoImportGpuTimingFinalEvidence, HeadlessViewerGpuError> {
        if self.submission_lifecycle.is_occupied() {
            return Err(HeadlessViewerGpuError::Backpressure(
                "cannot finish native-import timings while a Viewer GPU submission is in flight"
                    .to_owned(),
            ));
        }
        let native_timing_activated = self.runtime.native_import_gpu_timing_diagnostics().activated;
        if native_timing_activated
            && !self.native_import_gpu_timing.offline_completion_poll_observed
        {
            return Err(HeadlessViewerGpuError::NativeImportTimingNotFinished);
        }
        // `finish_gpu_timings` already collected after its device wait. This
        // drain deliberately has no hidden poll or synchronization.
        let samples = self.drain_native_import_gpu_timing_samples();
        let diagnostics = self.native_import_gpu_timing_diagnostics();
        Ok(HeadlessNativeVideoImportGpuTimingFinalEvidence { diagnostics, samples })
    }

    /// Samples discarded instead of blocking when every query slot was busy.
    #[cfg(test)]
    pub(crate) fn discarded_gpu_timings(&self) -> u64 {
        self.timestamp_ring.as_ref().map_or(0, GpuTimestampQueryRing::discarded_samples)
    }

    /// Record and queue one exact Viewer candidate without waiting for GPU
    /// completion.
    ///
    /// `deadline` is a non-renewing physical completion-safety bound, not the
    /// carried frame-presentation deadline. The returned submission keeps the
    /// complete frame owner resident in the single lifecycle slot.
    pub(crate) fn submit(
        &mut self,
        mut frame: PreviewGpuFrame,
        deadline: HeadlessGpuCompletionDeadline,
    ) -> Result<HeadlessViewerGpuSubmittedCandidate, HeadlessViewerGpuError> {
        ensure_headless_gpu_deadline(deadline)?;
        if let Some(terminal) = self.device_progress.generation_terminal() {
            return Err(HeadlessViewerGpuError::DeviceGenerationTerminal(terminal));
        }
        if frame.width == 0 || frame.height == 0 {
            return Err(HeadlessViewerGpuError::InvalidPresentation {
                width: frame.width,
                height: frame.height,
            });
        }
        let progress_permit = self.device_progress.reserve_submission()?;
        let completion_signal = progress_permit.completion_signal();
        let reservation = self.submission_lifecycle.reserve().map_err(|error| match error {
            ViewerGpuSubmissionAdmissionError::Backpressured => {
                HeadlessViewerGpuError::Backpressure(
                    "the bounded Viewer GPU submission capacity is full".to_owned(),
                )
            }
            ViewerGpuSubmissionAdmissionError::IdentityExhausted => {
                HeadlessViewerGpuError::SubmissionIdentityExhausted
            }
        })?;
        let submission_id = reservation.submission_id();
        let started = Instant::now();
        let output = HeadlessViewerGpuOutput {
            resource_key: headless_gpu_output_resource_key(
                &frame.external_texture_key(),
                submission_id.get(),
            ),
            width: frame.width,
            height: frame.height,
        };
        let heterogeneous = frame.has_heterogeneous_gpu_execution();
        self.runtime.clear_frame_resources();
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("headless_viewer_gpu_preview_encoder"),
        });
        let timestamp_token_result = self
            .timestamp_ring
            .as_mut()
            .map(|ring| ring.begin_frame(&self.device, &mut encoder))
            .transpose();
        if self.timestamp_ring.is_some() {
            // `GpuTimestampQueryRing::begin_frame` performs the existing
            // non-blocking telemetry poll. Reservation has not committed an
            // in-flight Viewer owner beyond the bounded submission capacity,
            // so this cannot become an unindexed Viewer
            // completion driver. Native-prefix callbacks made ready by that
            // poll must be collected before either success or error is
            // propagated.
            collect_headless_native_import_gpu_timings_after_device_poll(
                &mut self.runtime,
                &mut self.native_import_gpu_timing,
            );
        }
        let timestamp_token = timestamp_token_result
            .map_err(|error| HeadlessViewerGpuError::Timestamp(error.to_string()))?
            .flatten();
        let heterogeneous_inputs = frame.take_heterogeneous_gpu_inputs();
        let layers = match &frame.working_input {
            PreviewGpuWorkingInput::GpuComposite { layers } => layers,
        };
        let request = ViewerGpuExecutionRequest {
            sequence_id: frame.sequence_id,
            timeline_frame: frame.frame,
            width: frame.width,
            height: frame.height,
            working_color_space: frame.working_color_space,
            layers,
            heterogeneous_inputs,
            program_output_boundary: &frame.program_output_boundary,
            monitor_adaptation: &frame.monitor_adaptation,
            source_rect: ViewerSourceRect { x: 0.0, y: 0.0, width: 1.0, height: 1.0 },
            output_width: frame.width,
            output_height: frame.height,
            output_precision: ViewerGpuOutputPrecision::minimum_for_display(
                frame.monitor_adaptation.monitor_color_space(),
                false,
            ),
            display_calibration: None,
            program_scopes: None,
        };
        let record_result =
            if let (Some(ring), Some(token)) = (&mut self.timestamp_ring, timestamp_token) {
                let mut stage_marker = HeadlessGpuStageMarker { ring, token };
                self.runtime.record_with_stage_marker(
                    &self.device,
                    &self.queue,
                    &mut encoder,
                    request,
                    Some(&mut stage_marker),
                )
            } else {
                self.runtime.record(&self.device, &self.queue, &mut encoder, request)
            };
        let mut record = match record_result {
            Ok(record) => record,
            Err(error) => {
                if let (Some(ring), Some(token)) = (&mut self.timestamp_ring, timestamp_token) {
                    ring.abandon_frame(token)
                        .map_err(|error| HeadlessViewerGpuError::Timestamp(error.to_string()))?;
                }
                if matches!(
                    error,
                    mondrian_renderer::ViewerGpuExecutionError::Backpressure(_)
                ) {
                    // Recording failed before the Viewer queue submission.
                    // Transfer this attempt's reserved progress capacity to a
                    // typed renderer-cleanup command; the Headless thread must
                    // not introduce an unindexed Viewer completion poll.
                    progress_permit.drive_renderer_cleanup(submission_id);
                    return Err(HeadlessViewerGpuError::Backpressure(error.to_string()));
                }
                return Err(HeadlessViewerGpuError::Record(error.to_string()));
            }
        };
        if let (Some(ring), Some(token)) = (&mut self.timestamp_ring, timestamp_token) {
            ring.finish_frame(&mut encoder, token)
                .map_err(|error| HeadlessViewerGpuError::Timestamp(error.to_string()))?;
        }
        let native_import_gpu_timing_receipt = record.take_native_video_import_timing_receipt();
        let presentation_lease = self
            .runtime
            .take_presentation_output(&mut record)
            .map_err(|error| HeadlessViewerGpuError::Record(error.to_string()))?;
        if let Some(terminal) = self.device_progress.generation_terminal() {
            if let (Some(ring), Some(token)) = (&mut self.timestamp_ring, timestamp_token) {
                ring.abandon_frame(token)
                    .map_err(|error| HeadlessViewerGpuError::Timestamp(error.to_string()))?;
            }
            drop(presentation_lease);
            return Err(HeadlessViewerGpuError::DeviceGenerationTerminal(terminal));
        }
        let submission = self.queue.submit(std::iter::once(encoder.finish()));
        let heterogeneous_submission = record.assert_adapter_submission(submission.clone());
        let native_import_gpu_timing_receipt = native_import_gpu_timing_receipt
            .map(|receipt| self.native_import_gpu_timing.bind_receipt(receipt));
        #[cfg(test)]
        {
            self.native_import_gpu_timing.offline_completion_poll_observed = false;
        }
        let record_submit_us = elapsed_us(started);
        let completion_started = Instant::now();
        let (native_import_contract_pools, native_import_bridge_entries) =
            self.runtime.native_import_pool_residency();
        let native_import_retained_sources = self.runtime.native_import_retained_source_count();
        let owner = HeadlessViewerGpuSubmissionOwner {
            execution: HeadlessViewerGpuExecution {
                submission_id: submission_id.get(),
                output,
                output_width: frame.width,
                output_height: frame.height,
                duration_us: record_submit_us,
                record_submit_us,
                completion_wait_us: 0,
                gpu_completion_observed: false,
                gpu_timestamp_token: timestamp_token.map(|token| token.id()),
                cpu_stage_timings: Some(record.cpu_stage_timings),
                native_import_gpu_timing_receipt,
                compositing_diagnostics: Some(record.compositing_diagnostics),
                compositor_uniform_arena: Some(self.runtime.compositor_uniform_arena_diagnostics()),
                compositor_texture_bindings: Some(
                    self.runtime.compositor_texture_binding_diagnostics(),
                ),
                spatial_diagnostics: Some(record.spatial_diagnostics),
                stage_diagnostics: Some(record.stage_diagnostics),
                fallback_reasons: record.fallback_reasons,
                decode_execution: frame.decode_execution(),
                native_import_contract_pools,
                native_import_bridge_entries,
                native_import_retained_sources,
                remaining_submissions_after_completion: 0,
            },
            frame,
            queued_publication: None,
            successor_prepared: false,
            presentation_lease: Some(presentation_lease),
            started_at: started,
            completion_started_at: completion_started,
        };
        let queue = &self.queue;
        // The completion deadline is a frame-level safety bound, not the
        // caller's overall observation deadline. A lost work-done callback
        // must quarantine the submission within this window and force-release
        // its exact bounded slot shortly after, so the presentation pipeline
        // can continue; the caller's own (much longer) deadline then observes
        // the recovery instead of racing the slot release.
        let completion_deadline =
            deadline.instant().min(Instant::now() + HEADLESS_GPU_COMPLETION_SAFETY_DEADLINE);
        reservation.commit(
            owner,
            completion_deadline,
            move |callback| {
                heterogeneous_submission.register_completion_callback(queue, callback);
            },
            move || {
                completion_signal.mark_observed();
            },
        );
        progress_permit.commit(submission_id, submission);
        if let (Some(ring), Some(token)) = (&mut self.timestamp_ring, timestamp_token)
            && let Err(error) = ring.after_submit(token)
        {
            let reason = format!(
                "GPU timestamp tracking was disabled after submission {submission_id:?}: {error}"
            );
            if let Some(owner) = self.submission_lifecycle.owner_mut(submission_id) {
                owner.execution.gpu_timestamp_token = None;
                owner.execution.fallback_reasons.push(reason.clone());
            }
            // Timestamp telemetry is not publication authority. The queue
            // callback still owns frame retirement and the render remains
            // valid; disable this optional ring for later frames.
            self.timestamp_ring = None;
            tracing::warn!("{reason}");
        }
        Ok(HeadlessViewerGpuSubmittedCandidate { submission_id, heterogeneous })
    }

    /// Queue-order publish an ordinary candidate and retain its exact terminal
    /// disposition alongside the submitted owner until callback retirement.
    pub(crate) fn publish_ordinary_submission(
        &mut self,
        submission_id: ViewerGpuSubmissionId,
        publish: impl FnOnce(&PreviewGpuFrame, &HeadlessViewerGpuOutput) -> FramePresentationDisposition,
    ) -> Result<FramePresentationDisposition, HeadlessViewerGpuError> {
        if let Some(terminal) = self.device_progress.generation_terminal() {
            return Err(HeadlessViewerGpuError::DeviceGenerationTerminal(terminal));
        }
        let (submission_lifecycle, physical_outputs) =
            (&mut self.submission_lifecycle, &mut self.physical_outputs);
        let owner = submission_lifecycle.owner_mut(submission_id).ok_or(
            HeadlessViewerGpuError::UnknownSubmission(submission_id.get()),
        )?;
        if owner.frame.has_heterogeneous_gpu_execution() {
            return Err(HeadlessViewerGpuError::InvalidPublicationOrder(
                "heterogeneous candidates require exact callback validation".to_owned(),
            ));
        }
        let HeadlessViewerGpuSubmissionOwner {
            frame,
            execution,
            queued_publication,
            presentation_lease,
            ..
        } = owner;
        ensure_physical_publication_available(presentation_lease, submission_id)?;
        let disposition =
            publish_ordinary_once(queued_publication, || publish(frame, &execution.output))?;
        if let Some(terminal) = self.device_progress.generation_terminal() {
            return Err(HeadlessViewerGpuError::DeviceGenerationTerminal(terminal));
        }
        commit_physical_publication(
            physical_outputs,
            submission_id,
            &frame.output_key,
            &execution.output,
            presentation_lease,
            disposition,
        )?;
        Ok(disposition)
    }

    /// Retain an ordinary ticketless successor without changing visible output.
    #[cfg(test)]
    pub(crate) fn retain_ordinary_successor(
        &mut self,
        submission_id: ViewerGpuSubmissionId,
        retain: impl FnOnce(&PreviewGpuFrame, &HeadlessViewerGpuOutput),
    ) -> Result<(), HeadlessViewerGpuError> {
        if let Some(terminal) = self.device_progress.generation_terminal() {
            return Err(HeadlessViewerGpuError::DeviceGenerationTerminal(terminal));
        }
        let (submission_lifecycle, physical_outputs) =
            (&mut self.submission_lifecycle, &mut self.physical_outputs);
        let owner = submission_lifecycle.owner_mut(submission_id).ok_or(
            HeadlessViewerGpuError::UnknownSubmission(submission_id.get()),
        )?;
        if !owner.frame.is_successor_preparation()
            || owner.frame.presentation_ticket().is_some()
            || owner.frame.has_heterogeneous_gpu_execution()
        {
            return Err(HeadlessViewerGpuError::InvalidPublicationOrder(
                "successor retention requires an ordinary ticketless successor candidate"
                    .to_owned(),
            ));
        }
        ensure_physical_publication_available(&owner.presentation_lease, submission_id)?;
        if owner.successor_prepared || owner.queued_publication.is_some() {
            return Err(HeadlessViewerGpuError::InvalidPublicationOrder(
                "ordinary successor submission was already retained".to_owned(),
            ));
        }
        retain(&owner.frame, &owner.execution.output);
        owner.successor_prepared = true;
        let Some(lease) = owner.presentation_lease.take() else {
            return Err(HeadlessViewerGpuError::MissingPresentationOutput(
                submission_id.get(),
            ));
        };
        let _ = physical_outputs.publish_prepared(
            submission_id,
            owner.frame.output_key.clone(),
            owner.execution.output.clone(),
            lease,
        );
        Ok(())
    }

    /// Promote the exact prepared physical artifact to the visible slot.
    pub(crate) fn promote_prepared_successor(
        &mut self,
        output_key: &crate::app::preview_execution::PreviewOutputKey,
    ) -> bool {
        let promotion = self.physical_outputs.promote_prepared_exact(output_key);
        let exact_output_available = promotion.exact_output_available();
        drop(promotion.into_retired());
        exact_output_available
    }

    /// Publish a completed heterogeneous candidate through the same physical
    /// current-output slot used by ordinary queue-ordered publication.
    ///
    /// Callback validation and Broker finalization must precede this call.
    /// Rejected publication retains the lease in `completed` until that
    /// already-retired candidate is dropped.
    pub(crate) fn publish_completed_heterogeneous(
        &mut self,
        completed: &mut HeadlessViewerGpuCompletedCandidate,
        publish: impl FnOnce(&PreviewGpuFrame, &HeadlessViewerGpuOutput) -> FramePresentationDisposition,
    ) -> Result<FramePresentationDisposition, HeadlessViewerGpuError> {
        if let Some(terminal) = self.device_progress.generation_terminal() {
            return Err(HeadlessViewerGpuError::DeviceGenerationTerminal(terminal));
        }
        if completed.quarantine_reason.is_some() {
            return Err(HeadlessViewerGpuError::InvalidPublicationOrder(
                "a quarantined Viewer completion is retirement-only".to_owned(),
            ));
        }
        if completed.queued_publication.is_some() || completed.heterogeneous_completion.is_none() {
            return Err(HeadlessViewerGpuError::InvalidPublicationOrder(
                "completed heterogeneous publication requires unconsumed callback evidence"
                    .to_owned(),
            ));
        }
        ensure_physical_publication_available(
            &completed.presentation_lease,
            completed.submission_id,
        )?;
        let disposition = publish(&completed.frame, &completed.execution.output);
        if let Some(terminal) = self.device_progress.generation_terminal() {
            return Err(HeadlessViewerGpuError::DeviceGenerationTerminal(terminal));
        }
        commit_physical_publication(
            &mut self.physical_outputs,
            completed.submission_id,
            &completed.frame.output_key,
            &completed.execution.output,
            &mut completed.presentation_lease,
            disposition,
        )?;
        Ok(disposition)
    }

    /// Whether Preview's complete cloneable artifact has the exact live
    /// move-only physical owner.
    pub(crate) fn has_current_physical_output_artifact(
        &self,
        output_key: &crate::app::preview_execution::PreviewOutputKey,
        output: &HeadlessViewerGpuOutput,
    ) -> bool {
        if self.device_progress.generation_terminal().is_some() {
            return false;
        }
        self.physical_outputs
            .current_artifact_for_key(output_key)
            .is_some_and(|current| current == output)
    }

    /// Whether the visible physical slot has this exact semantic key.
    #[cfg(test)]
    pub(crate) fn has_current_physical_output_for_key(
        &self,
        output_key: &crate::app::preview_execution::PreviewOutputKey,
    ) -> bool {
        self.device_progress.generation_terminal().is_none()
            && self.physical_outputs.current_artifact_for_key(output_key).is_some()
    }

    /// Clear all GPU publications after an accepted non-GPU presentation.
    pub(crate) fn clear_physical_outputs(&mut self) -> bool {
        self.physical_outputs.drain().into_iter().flatten().count() != 0
    }

    /// Release only the retained prepared physical owner.
    ///
    /// A prepared successor can never be promoted once the transport reached
    /// its natural end (no next frame demand exists), so its retained
    /// capacity-one lease would otherwise report as a second live presentation
    /// output and reject every later ordinary record.
    pub(crate) fn clear_prepared_physical_output(&mut self) -> bool {
        self.physical_outputs.take_prepared().is_some()
    }

    /// Retire a prepared physical output that is no longer proved by Preview's
    /// active semantic generation.
    pub(crate) fn retire_stale_prepared_physical_output(
        &mut self,
        expected_output_key: Option<&crate::app::preview_execution::PreviewOutputKey>,
    ) -> Option<(
        crate::app::preview_execution::PreviewOutputKey,
        HeadlessViewerGpuOutput,
    )> {
        self.physical_outputs
            .retire_prepared_unless(expected_output_key)
            .map(ViewerGpuPhysicalPublication::into_key_and_artifact)
    }

    /// Clear only the physical publication produced by one exact submission.
    ///
    /// A late callback or cleanup failure from an older submission must not
    /// erase a newer current output.
    pub(crate) fn clear_current_physical_output_for_submission(
        &mut self,
        submission_id: ViewerGpuSubmissionId,
    ) -> Option<HeadlessViewerGpuOutput> {
        self.take_physical_output_for_submission(submission_id)
    }

    fn take_physical_output_for_submission(
        &mut self,
        submission_id: ViewerGpuSubmissionId,
    ) -> Option<HeadlessViewerGpuOutput> {
        self.physical_outputs
            .take_for_submission(submission_id)
            .map(ViewerGpuPhysicalPublication::into_artifact)
    }

    /// Revoke every physical publication from a terminal device generation.
    ///
    /// Unlike timeout/cancellation cleanup, a device-generation failure is
    /// not scoped to the submission that first reported it. The current output
    /// may have been published by an older, already-completed submission.
    pub(crate) fn enter_device_generation_retirement(
        &mut self,
        reason: String,
    ) -> (
        Vec<ViewerGpuSubmissionId>,
        [Option<(
            crate::app::preview_execution::PreviewOutputKey,
            HeadlessViewerGpuOutput,
        )>; 2],
    ) {
        let active_submissions = self
            .submission_lifecycle
            .quarantine_all_after_device_failure(reason)
            .into_iter()
            .map(|quarantine| quarantine.submission_id)
            .collect();
        let revoked_outputs = self.physical_outputs.drain().map(|publication| {
            publication.map(ViewerGpuPhysicalPublication::into_key_and_artifact)
        });
        (active_submissions, revoked_outputs)
    }

    /// Whether the exact ordinary submission already ran queue-ordered
    /// publication.
    pub(crate) fn has_queued_publication(&self, submission_id: ViewerGpuSubmissionId) -> bool {
        self.submission_lifecycle
            .owner(submission_id)
            .is_some_and(|owner| owner.queued_publication.is_some())
    }

    /// Whether one exact in-flight submission already populated the successor slot.
    pub(crate) fn has_prepared_successor_submission(
        &self,
        submission_id: ViewerGpuSubmissionId,
    ) -> bool {
        self.submission_lifecycle
            .owner(submission_id)
            .is_some_and(|owner| owner.successor_prepared)
    }

    /// Presentation ticket carried by the retained candidate.
    pub(crate) fn presentation_ticket(
        &self,
        submission_id: ViewerGpuSubmissionId,
    ) -> Option<mondrian_playback::FramePresentationTicket> {
        self.submission_lifecycle
            .owner(submission_id)
            .and_then(|owner| owner.frame.presentation_ticket())
    }

    /// Complete resolved output identity retained by one exact submission.
    pub(crate) fn output_key(
        &self,
        submission_id: ViewerGpuSubmissionId,
    ) -> Option<crate::app::preview_execution::PreviewOutputKey> {
        self.submission_lifecycle
            .owner(submission_id)
            .map(|owner| owner.frame.output_key.clone())
    }

    /// Whether the retained candidate can describe the current consumer
    /// coordinate and any still-pending exact demand.
    pub(crate) fn matches_consumer_intent(
        &self,
        submission_id: ViewerGpuSubmissionId,
        frame: i64,
        pending_demand: Option<mondrian_playback::FrameDemandIdentity>,
    ) -> bool {
        self.submission_lifecycle.owner(submission_id).is_some_and(|owner| {
            owner.frame.frame == frame
                && pending_demand.is_none_or(|pending| {
                    owner
                        .frame
                        .presentation_ticket()
                        .is_some_and(|ticket| ticket.identity() == pending)
                })
        })
    }

    /// Revoke semantic authority while retaining every submitted GPU owner.
    pub(crate) fn quarantine_after_authority_revocation(
        &mut self,
        submission_id: ViewerGpuSubmissionId,
        reason: impl Into<String>,
    ) -> (
        Option<ViewerGpuSubmissionQuarantine>,
        Option<HeadlessViewerGpuOutput>,
    ) {
        if self.submission_lifecycle.owner(submission_id).is_none() {
            return (None, None);
        }
        let quarantine = self
            .submission_lifecycle
            .quarantine_submission_after_authority_revocation(submission_id, reason.into());
        let revoked_current_physical_output =
            self.take_physical_output_for_submission(submission_id);
        (quarantine, revoked_current_physical_output)
    }

    /// Move only the semantic heterogeneous terminal out of a quarantined
    /// owner; frame resources and media protections remain retained.
    pub(crate) fn take_heterogeneous_terminal(
        &mut self,
        submission_id: ViewerGpuSubmissionId,
    ) -> Option<PreviewGpuHeterogeneousExecution> {
        self.submission_lifecycle
            .owner_mut(submission_id)
            .and_then(|owner| owner.frame.take_heterogeneous_gpu_execution())
    }

    /// Whether a submitted owner still occupies the renderer frame slot.
    pub(crate) fn has_submission_in_flight(&self) -> bool {
        self.submission_lifecycle.is_occupied()
    }

    /// Whether both bounded submitted-owner slots are occupied.
    pub(crate) fn submission_capacity_is_full(&self) -> bool {
        self.submission_lifecycle.is_at_capacity()
    }

    /// Typed terminal state of this concrete device generation.
    pub(crate) fn device_generation_terminal(&self) -> Option<ViewerGpuDeviceGenerationTerminal> {
        self.device_progress.generation_terminal()
    }

    /// Non-renewing semantic completion deadline for the current submission.
    ///
    /// GPU progress itself owns a payload-free wake from the shared device
    /// progress domain; quarantined retirement has no timer-based wake.
    #[cfg(test)]
    pub(crate) fn next_submission_wake(&self) -> Option<Instant> {
        self.submission_lifecycle.next_wake()
    }

    /// Observe exact callback state and progress-domain failures without
    /// blocking the Headless caller.
    pub(crate) fn poll_completion(&mut self, now: Instant) -> HeadlessViewerGpuCompletionPoll {
        let mut progress_observed = false;
        let mut observation_time = now;
        let mut device_failure = None;
        let mut callback_barrier = None;
        while let Some(observation) = self.device_progress.try_observe() {
            progress_observed = true;
            match observation {
                ViewerGpuDeviceProgressObservation::WaitSatisfied {
                    submission_id,
                    observed_at,
                } => {
                    // This observation carries no completion authority. An
                    // older wait may arrive after its callback opened the next
                    // bounded slots, so only let the exact active identity
                    // influence this lifecycle observation.
                    if self.submission_lifecycle.contains(submission_id) {
                        observation_time = observation_time.max(observed_at);
                        callback_barrier = Some((submission_id, observed_at));
                        break;
                    }
                }
                ViewerGpuDeviceProgressObservation::RendererCleanupSatisfied { .. } => {
                    // The typed barrier exists only to reclaim renderer-owned
                    // pre-submit work and collect its optional timing samples.
                }
                ViewerGpuDeviceProgressObservation::DevicePollFailed {
                    submission_id,
                    reason,
                    observed_at,
                } => {
                    observation_time = observation_time.max(observed_at);
                    device_failure = Some((Some(submission_id), reason));
                }
            }
        }
        // Device loss is callback authority, not an observation-channel
        // payload. Read it unconditionally so an idle generation or a
        // work-done callback delivered earlier in the same poll cannot win.
        if let Some(terminal) = self.device_progress.generation_terminal() {
            observation_time = observation_time.max(terminal.observed_at);
            device_failure = Some((terminal.submission_id, terminal.reason));
        }
        if progress_observed {
            self.collect_native_import_gpu_timings_after_device_poll();
        }
        let initial = if let Some((submission_id, observed_at)) = callback_barrier {
            match self.submission_lifecycle.poll(observation_time) {
                ViewerGpuSubmissionPoll::Pending { .. } => self
                    .submission_lifecycle
                    .retire_submission_after_fence_barrier(submission_id, observed_at)
                    .unwrap_or_else(|| {
                        self.submission_lifecycle.poll_deadline_only(observation_time)
                    }),
                poll => poll,
            }
        } else {
            self.submission_lifecycle.poll_deadline_only(observation_time)
        };
        self.report_new_orphaned_completions();
        if matches!(
            &initial,
            ViewerGpuSubmissionPoll::Completed(_)
                | ViewerGpuSubmissionPoll::RetiredAfterQuarantine(_)
        ) {
            let generation_failure = device_failure.map(|(submission_id, reason)| {
                ViewerGpuSubmissionQuarantineReason::DevicePollFailed(format!(
                    "device generation terminal {}: {reason}",
                    headless_viewer_gpu_generation_failure_context(submission_id)
                ))
            });
            return self.map_submission_poll(initial, generation_failure);
        }
        if let ViewerGpuSubmissionPoll::Pending { submission_id, .. } = &initial {
            if let Some((failed_submission_id, error)) = device_failure {
                let generation_reason = format!(
                    "device generation terminal {} while submission {} remained active: {error}",
                    headless_viewer_gpu_generation_failure_context(failed_submission_id),
                    submission_id.get()
                );
                if let Some(quarantine) = self
                    .submission_lifecycle
                    .quarantine_all_after_device_failure(generation_reason)
                    .into_iter()
                    .next()
                {
                    let revoked_current_physical_output =
                        self.take_physical_output_for_submission(quarantine.submission_id);
                    return HeadlessViewerGpuCompletionPoll::QuarantineStarted {
                        quarantine,
                        revoked_current_physical_output,
                    };
                }
            }
        } else if let Some((failed_submission_id, error)) = device_failure {
            tracing::error!(
                failed_submission_id = ?failed_submission_id.map(ViewerGpuSubmissionId::get),
                %error,
                "Headless Viewer observed a GPU device-progress failure without an active submission"
            );
        }
        self.map_submission_poll(initial, None)
    }

    fn collect_native_import_gpu_timings_after_device_poll(&mut self) {
        collect_headless_native_import_gpu_timings_after_device_poll(
            &mut self.runtime,
            &mut self.native_import_gpu_timing,
        );
    }

    fn report_new_orphaned_completions(&mut self) {
        let observed = self.submission_lifecycle.orphaned_completion_count();
        if observed <= self.reported_orphaned_completion_count {
            return;
        }
        self.reported_orphaned_completion_count = observed;
        tracing::error!(
            orphaned_completion_count = observed,
            "Headless Viewer discarded a GPU completion callback without its exact owner"
        );
    }

    fn map_submission_poll(
        &mut self,
        poll: ViewerGpuSubmissionPoll<
            HeadlessViewerGpuSubmissionOwner,
            ViewerHeterogeneousGpuCompletedBatch,
        >,
        retirement_reason: Option<ViewerGpuSubmissionQuarantineReason>,
    ) -> HeadlessViewerGpuCompletionPoll {
        match poll {
            ViewerGpuSubmissionPoll::Idle => HeadlessViewerGpuCompletionPoll::Idle,
            ViewerGpuSubmissionPoll::Pending { submission_id, quarantined } => {
                HeadlessViewerGpuCompletionPoll::Pending { submission_id, quarantined }
            }
            ViewerGpuSubmissionPoll::QuarantineStarted(quarantine) => {
                let revoked_current_physical_output =
                    self.take_physical_output_for_submission(quarantine.submission_id);
                HeadlessViewerGpuCompletionPoll::QuarantineStarted {
                    quarantine,
                    revoked_current_physical_output,
                }
            }
            ViewerGpuSubmissionPoll::Completed(completed) => {
                HeadlessViewerGpuCompletionPoll::Completed(Box::new(
                    self.finish_completed_submission(completed, retirement_reason),
                ))
            }
            ViewerGpuSubmissionPoll::RetiredAfterQuarantine(retired) => {
                HeadlessViewerGpuCompletionPoll::RetiredAfterQuarantine(Box::new(
                    self.finish_retired_submission(retired),
                ))
            }
        }
    }

    fn finish_retired_submission(
        &mut self,
        retired: ViewerGpuRetiredSubmission<HeadlessViewerGpuSubmissionOwner>,
    ) -> HeadlessViewerGpuRetiredCandidate {
        let ViewerGpuRetiredSubmission { submission_id, owner, reason } = retired;
        let completion_error = self
            .runtime
            .retire_completed_native_import_sources()
            .err()
            .map(|error| error.to_string());
        let revoked_current_physical_output =
            self.take_physical_output_for_submission(submission_id);
        HeadlessViewerGpuRetiredCandidate {
            submission_id,
            frame: owner.frame,
            queued_publication: owner.queued_publication,
            successor_prepared: owner.successor_prepared,
            quarantine_reason: reason,
            completion_error,
            revoked_current_physical_output,
            presentation_lease: owner.presentation_lease,
        }
    }

    fn finish_completed_submission(
        &mut self,
        completed: ViewerGpuCompletedSubmission<
            HeadlessViewerGpuSubmissionOwner,
            ViewerHeterogeneousGpuCompletedBatch,
        >,
        retirement_reason: Option<ViewerGpuSubmissionQuarantineReason>,
    ) -> HeadlessViewerGpuCompletedCandidate {
        let ViewerGpuCompletedSubmission {
            submission_id,
            mut owner,
            completion,
            completion_observed_at,
            quarantine_reason,
        } = completed;
        owner.execution.duration_us = duration_us(owner.started_at, completion_observed_at);
        owner.execution.completion_wait_us =
            duration_us(owner.completion_started_at, completion_observed_at);
        owner.execution.gpu_completion_observed = true;
        let completion_error = self
            .runtime
            .retire_completed_native_import_sources()
            .err()
            .map(|error| error.to_string());
        let (native_import_contract_pools, native_import_bridge_entries) =
            self.runtime.native_import_pool_residency();
        owner.execution.native_import_contract_pools = native_import_contract_pools;
        owner.execution.native_import_bridge_entries = native_import_bridge_entries;
        owner.execution.native_import_retained_sources =
            self.runtime.native_import_retained_source_count();
        owner.execution.remaining_submissions_after_completion =
            self.submission_lifecycle.active_count();
        let heterogeneous_completion = (!completion.is_empty()).then_some(completion);
        let quarantine_reason = quarantine_reason.or(retirement_reason);
        let revoked_current_physical_output = if quarantine_reason.is_some() {
            self.take_physical_output_for_submission(submission_id)
        } else {
            None
        };
        HeadlessViewerGpuCompletedCandidate {
            submission_id,
            frame: owner.frame,
            execution: owner.execution,
            heterogeneous_completion,
            queued_publication: owner.queued_publication,
            successor_prepared: owner.successor_prepared,
            quarantine_reason,
            completion_error,
            revoked_current_physical_output,
            presentation_lease: owner.presentation_lease,
        }
    }
}

fn headless_viewer_gpu_generation_failure_context(
    submission_id: Option<ViewerGpuSubmissionId>,
) -> String {
    submission_id.map_or_else(
        || "outside an active Viewer submission".to_owned(),
        |submission_id| format!("after submission attempt {}", submission_id.get()),
    )
}

fn collect_headless_native_import_gpu_timings_after_device_poll(
    runtime: &mut ViewerGpuExecutionRuntime,
    session: &mut HeadlessNativeVideoImportGpuTimingSession,
) {
    runtime.collect_native_import_gpu_timings_after_device_poll();
    session.accept_samples(runtime.take_completed_native_import_gpu_timings());
}

fn publish_ordinary_once(
    queued_publication: &mut Option<FramePresentationDisposition>,
    publish: impl FnOnce() -> FramePresentationDisposition,
) -> Result<FramePresentationDisposition, HeadlessViewerGpuError> {
    if queued_publication.is_some() {
        return Err(HeadlessViewerGpuError::InvalidPublicationOrder(
            "ordinary Viewer submission was already published".to_owned(),
        ));
    }
    let disposition = publish();
    *queued_publication = Some(disposition);
    Ok(disposition)
}

fn headless_gpu_output_resource_key(semantic_key: &str, submission_id: u64) -> String {
    format!("{semantic_key}:submission:{submission_id}")
}

fn ensure_physical_publication_available<L>(
    presentation_lease: &Option<L>,
    submission_id: ViewerGpuSubmissionId,
) -> Result<(), HeadlessViewerGpuError> {
    if presentation_lease.is_some() {
        Ok(())
    } else {
        Err(HeadlessViewerGpuError::MissingPresentationOutput(
            submission_id.get(),
        ))
    }
}

fn commit_physical_publication<K: Clone + PartialEq, O: Clone, L>(
    current: &mut ViewerGpuPublicationSlots<K, O, L>,
    source_submission_id: ViewerGpuSubmissionId,
    output_key: &K,
    output: &O,
    presentation_lease: &mut Option<L>,
    disposition: FramePresentationDisposition,
) -> Result<(), HeadlessViewerGpuError> {
    if !matches!(
        disposition,
        FramePresentationDisposition::Presented(_) | FramePresentationDisposition::NoDemand
    ) {
        return Ok(());
    }
    let Some(lease) = presentation_lease.take() else {
        return Err(HeadlessViewerGpuError::MissingPresentationOutput(
            source_submission_id.get(),
        ));
    };
    let _ = current.publish_current(
        source_submission_id,
        output_key.clone(),
        output.clone(),
        lease,
    );
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum HeadlessViewerGpuError {
    #[error("no headless GPU adapter is available: {0}")]
    Adapter(String),
    #[error("headless GPU device creation failed: {0}")]
    Device(String),
    #[error(transparent)]
    DeviceProgressStart(#[from] ViewerGpuDeviceProgressStartError),
    #[error(transparent)]
    DeviceProgressReserve(#[from] ViewerGpuDeviceProgressReserveError),
    #[error("headless Viewer device generation became terminal: {0:?}")]
    DeviceGenerationTerminal(ViewerGpuDeviceGenerationTerminal),
    #[error("headless Viewer GPU runtime creation failed: {0}")]
    Runtime(#[from] ViewerGpuExecutionRuntimeCreateError),
    #[error("invalid headless Viewer presentation extent {width}x{height}")]
    InvalidPresentation { width: u32, height: u32 },
    #[error("headless Viewer GPU execution is temporarily backpressured: {0}")]
    Backpressure(String),
    #[error("headless Viewer GPU recording failed: {0}")]
    Record(String),
    #[error("headless Viewer GPU execution exceeded its caller's monotonic deadline")]
    DeadlineExceeded,
    #[error("headless Viewer GPU submission identity space is exhausted")]
    SubmissionIdentityExhausted,
    #[error("Headless native-import GPU timing session identity space is exhausted")]
    NativeImportTimingSessionIdentityExhausted,
    #[error("invalid Headless native-import GPU timing observation capacity {capacity}: {reason}")]
    InvalidNativeImportTimingObservationCapacity {
        capacity: usize,
        reason: &'static str,
    },
    #[error("native-import GPU timing finalization requires the existing suffix completion wait")]
    #[cfg(test)]
    NativeImportTimingNotFinished,
    #[error(
        "native-import GPU timing requires the existing Viewer suffix timestamp wait, but that ring is unavailable"
    )]
    #[cfg(test)]
    NativeImportTimingSuffixUnavailable,
    #[error("unknown Headless Viewer GPU submission {0}")]
    UnknownSubmission(u64),
    #[error("Headless Viewer GPU submission {0} no longer owns its presentation output")]
    MissingPresentationOutput(u64),
    #[error("invalid Headless Viewer GPU publication ordering: {0}")]
    InvalidPublicationOrder(String),
    #[error("headless Viewer GPU timestamp query failed: {0}")]
    Timestamp(String),
}

fn ensure_headless_gpu_deadline(
    deadline: HeadlessGpuCompletionDeadline,
) -> Result<std::time::Duration, HeadlessViewerGpuError> {
    let remaining = deadline.instant().saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        Err(HeadlessViewerGpuError::DeadlineExceeded)
    } else {
        Ok(remaining)
    }
}

fn allocate_headless_native_import_gpu_timing_session_id(
) -> Result<HeadlessNativeVideoImportGpuTimingSessionId, HeadlessViewerGpuError> {
    NEXT_HEADLESS_NATIVE_IMPORT_GPU_TIMING_SESSION_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .map(HeadlessNativeVideoImportGpuTimingSessionId)
        .map_err(|_| HeadlessViewerGpuError::NativeImportTimingSessionIdentityExhausted)
}

fn validate_native_import_gpu_timing_observation_capacity(
    policy: NativeVideoImportGpuTimingPolicy,
    observation_capacity: usize,
) -> Result<(), HeadlessViewerGpuError> {
    match policy {
        NativeVideoImportGpuTimingPolicy::Disabled if observation_capacity != 0 => Err(
            HeadlessViewerGpuError::InvalidNativeImportTimingObservationCapacity {
                capacity: observation_capacity,
                reason: "disabled timing must not allocate an App observation buffer",
            },
        ),
        NativeVideoImportGpuTimingPolicy::Enabled { .. } if observation_capacity == 0 => Err(
            HeadlessViewerGpuError::InvalidNativeImportTimingObservationCapacity {
                capacity: observation_capacity,
                reason: "enabled timing requires a non-zero App observation buffer",
            },
        ),
        _ if observation_capacity > HEADLESS_NATIVE_IMPORT_GPU_TIMING_MAX_OBSERVATION_CAPACITY => {
            Err(
                HeadlessViewerGpuError::InvalidNativeImportTimingObservationCapacity {
                    capacity: observation_capacity,
                    reason: "capacity exceeds the Headless hard safety bound",
                },
            )
        }
        _ => Ok(()),
    }
}

fn elapsed_us(started: Instant) -> u64 {
    started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
}

fn duration_us(started: Instant, completed: Instant) -> u64 {
    completed
        .saturating_duration_since(started)
        .as_micros()
        .min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Clone)]
    struct TestOutput(&'static str);

    struct TestLease(Arc<AtomicUsize>);

    impl Drop for TestLease {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn headless_gpu_wait_uses_the_callers_nonrenewing_deadline() {
        assert!(matches!(
            ensure_headless_gpu_deadline(HeadlessGpuCompletionDeadline::at(Instant::now())),
            Err(HeadlessViewerGpuError::DeadlineExceeded)
        ));
        let remaining = ensure_headless_gpu_deadline(HeadlessGpuCompletionDeadline::at(
            Instant::now() + Duration::from_secs(1),
        ))
        .expect("future deadline");
        assert!(!remaining.is_zero());
        assert!(remaining <= Duration::from_secs(1));
    }

    #[test]
    fn gpu_completion_safety_budget_is_independent_from_frame_cadence() {
        let now = Instant::now();
        let frame_presentation_deadline = now + Duration::from_millis(1);
        let completion_deadline = HeadlessGpuCompletionDeadline::after(now, Duration::from_secs(1))
            .expect("completion safety deadline");

        assert!(completion_deadline.instant() > frame_presentation_deadline);
    }

    #[test]
    fn native_import_observation_capacity_is_explicit_and_policy_bounded() {
        assert!(validate_native_import_gpu_timing_observation_capacity(
            NativeVideoImportGpuTimingPolicy::Disabled,
            0,
        )
        .is_ok());
        assert!(matches!(
            validate_native_import_gpu_timing_observation_capacity(
                NativeVideoImportGpuTimingPolicy::Disabled,
                1,
            ),
            Err(HeadlessViewerGpuError::InvalidNativeImportTimingObservationCapacity { .. })
        ));
        assert!(matches!(
            validate_native_import_gpu_timing_observation_capacity(
                NativeVideoImportGpuTimingPolicy::Enabled { capacity: 1 },
                0,
            ),
            Err(HeadlessViewerGpuError::InvalidNativeImportTimingObservationCapacity { .. })
        ));
        assert!(matches!(
            validate_native_import_gpu_timing_observation_capacity(
                NativeVideoImportGpuTimingPolicy::Enabled { capacity: 1 },
                HEADLESS_NATIVE_IMPORT_GPU_TIMING_MAX_OBSERVATION_CAPACITY + 1,
            ),
            Err(HeadlessViewerGpuError::InvalidNativeImportTimingObservationCapacity { .. })
        ));
    }

    #[test]
    fn native_import_observation_buffer_is_bounded_and_session_qualified() {
        let mut session =
            HeadlessNativeVideoImportGpuTimingSession::new(2).expect("timing session");
        let session_id = session.id;
        let make_sample = move |import_token| HeadlessNativeVideoImportGpuTimingSample {
            session_id,
            candidate_token: 5,
            import_token,
            yuv_decode_marker_bracket_us: 11,
            input_color_marker_bracket_us: 13,
            decode_fence_ready_at_admission: Some(true),
        };
        session.accept_sample(make_sample(1));
        session.accept_sample(make_sample(2));
        session.accept_sample(make_sample(3));

        assert_eq!(session.completed.len(), 2);
        assert_eq!(session.adapter_overflow_samples, 1);
        assert!(session.completed.iter().all(|sample| sample.session_id == session.id));
        let drained = session.drain();
        assert_eq!(drained.len(), 2);
        assert_eq!(session.drained_samples, 2);
        assert!(session.completed.is_empty());
    }

    #[test]
    fn same_semantic_output_gets_a_unique_resource_key_per_submission() {
        let first = headless_gpu_output_resource_key("viewer.gpu:semantic", 41);
        let second = headless_gpu_output_resource_key("viewer.gpu:semantic", 42);

        assert_ne!(first, second);
        assert!(first.ends_with(":submission:41"));
        assert!(second.ends_with(":submission:42"));
    }

    #[test]
    fn ordinary_publication_is_exactly_once_and_rejects_before_running_closure() {
        let calls = Cell::new(0);
        let mut publication = None;
        assert_eq!(
            publish_ordinary_once(&mut publication, || {
                calls.set(calls.get() + 1);
                FramePresentationDisposition::NoDemand
            })
            .expect("first publication"),
            FramePresentationDisposition::NoDemand
        );

        assert!(matches!(
            publish_ordinary_once(&mut publication, || {
                calls.set(calls.get() + 1);
                FramePresentationDisposition::NoDemand
            }),
            Err(HeadlessViewerGpuError::InvalidPublicationOrder(_))
        ));
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn accepted_publication_moves_the_lease_into_the_physical_current_slot() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut slot = ViewerGpuPublicationSlots::<String, TestOutput, TestLease>::default();
        let mut lease = Some(TestLease(Arc::clone(&drops)));
        let output_key = "output-a".to_owned();
        let output = TestOutput("metadata-a");

        commit_physical_publication(
            &mut slot,
            ViewerGpuSubmissionId::for_test(7),
            &output_key,
            &output,
            &mut lease,
            FramePresentationDisposition::NoDemand,
        )
        .expect("accepted physical publication");

        assert!(lease.is_none());
        assert_eq!(
            slot.current_artifact_for_key(&output_key).map(|output| output.0),
            Some("metadata-a")
        );
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        assert_eq!(
            slot.take_for_submission(ViewerGpuSubmissionId::for_test(7))
                .map(ViewerGpuPhysicalPublication::into_artifact)
                .map(|output| output.0),
            Some("metadata-a")
        );
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn rejected_publication_retains_its_lease_until_submission_retirement() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut slot = ViewerGpuPublicationSlots::<String, TestOutput, TestLease>::default();
        let mut lease = Some(TestLease(Arc::clone(&drops)));

        commit_physical_publication(
            &mut slot,
            ViewerGpuSubmissionId::for_test(11),
            &"rejected".to_owned(),
            &TestOutput("metadata"),
            &mut lease,
            FramePresentationDisposition::OutputRejected,
        )
        .expect("rejection is not a physical-publication failure");

        assert!(lease.is_some());
        assert!(slot.current().is_none());
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        drop(lease);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }
}
