//! Renderer-owned CPU-F32 → GPU-F32 Effect continuation execution.
//!
//! This Module consumes the exact handoff produced by
//! [`mondrian_effects::PreparedHeterogeneousCpuCompletion`]. It validates the
//! caller binding and graph-value token chain, uploads the completed CPU value,
//! and records only the already-lowered GPU suffix. Recording, queue
//! submission, GPU completion, and optional CPU readback are distinct evidence
//! states; command recording is never reported as completion.

use crate::{
    CpuColorFrame, GpuColorFrameIdAllocator, GpuColorFrameReadback, GpuColorFrameReadbackError,
    GpuColorFrameReadbackPlan, GpuColorFrameResource, GpuColorFrameResourceTable,
    GpuColorFrameResourceTableError, GpuColorFrameTextureFormat, GpuColorFrameUploadError,
    GpuColorFrameUploadPlan, GpuColorFrameUploader, GpuColorFrameWgpuResource,
    GpuColorFrameWgpuResourcePool, GpuColorFrameWgpuResourcePoolOptions, GpuCompositeError,
    GpuContext, GpuFrameCompositor,
};
use mondrian_core::{ExecutionCancellationToken, WorkingColorSpace, WorkingRgbaF32Frame};
use mondrian_effects::{
    EffectColorDomain, EffectCompletionToken, EffectExecutionEnvironment,
    EffectExecutionEnvironmentError, EffectExecutionLane, EffectExecutionLaneId,
    EffectExecutionTransfer, EffectFrameExtent, EffectGraphExecutionBudget,
    EffectGraphExecutionRequest, EffectGraphExecutionStep, EffectProcessingBackend,
    EffectValueFormat, EffectValueResidency, EffectWorkingPrecision, EffectWorkingPrecisions,
    HeterogeneousCpuCompletionEvidence, PreparedHeterogeneousCpuCompletion,
    PreparedHeterogeneousGpuDispatch, PreparedHeterogeneousGpuStep, PreparedHeterogeneousGpuSuffix,
};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};

const CPU_F32_LANE: EffectExecutionLaneId = EffectExecutionLaneId::new(1);
const GPU_F32_LANE: EffectExecutionLaneId = EffectExecutionLaneId::new(2);
const CPU_DISPATCH_COST: u32 = 10;
const GPU_DISPATCH_COST: u32 = 1;
const CPU_TO_GPU_TRANSFER_COST: u32 = 1;
const HETEROGENEOUS_GPU_WAIT_SLICE: Duration = Duration::from_millis(10);
static NEXT_HETEROGENEOUS_GPU_BATCH_ID: AtomicU64 = AtomicU64::new(1);

/// Renderer-owned capability for the currently executable scene-linear
/// CPU-F32 → GPU-F32 Effect route.
///
/// Callers provide frame-local resource budgets, but never duplicate lane
/// identities, transfer direction, representation, or relative costs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeterogeneousGpuExecutionCapability {
    environment: EffectExecutionEnvironment,
}

impl HeterogeneousGpuExecutionCapability {
    /// Construct the renderer's exact heterogeneous execution environment.
    pub fn scene_linear_f32() -> Result<Self, EffectExecutionEnvironmentError> {
        let environment = EffectExecutionEnvironment::new(
            Arc::from([
                EffectExecutionLane::new(
                    CPU_F32_LANE,
                    EffectProcessingBackend::Cpu,
                    EffectWorkingPrecisions::FLOAT32,
                    CPU_DISPATCH_COST,
                ),
                EffectExecutionLane::new(
                    GPU_F32_LANE,
                    EffectProcessingBackend::Gpu,
                    EffectWorkingPrecisions::FLOAT32,
                    GPU_DISPATCH_COST,
                ),
            ]),
            Arc::from([EffectExecutionTransfer::new(
                CPU_F32_LANE,
                EffectWorkingPrecision::Float32,
                GPU_F32_LANE,
                EffectWorkingPrecision::Float32,
                CPU_TO_GPU_TRANSFER_COST,
            )]),
        )?;
        Ok(Self { environment })
    }

    /// Validated execution environment consumed by heterogeneous preparation.
    pub const fn environment(&self) -> &EffectExecutionEnvironment {
        &self.environment
    }

    /// Build the exact graph-value request for one scene-linear frame.
    pub const fn request(
        &self,
        frame_extent: EffectFrameExtent,
        budget: EffectGraphExecutionBudget,
    ) -> EffectGraphExecutionRequest {
        let format = EffectValueFormat::new(
            EffectWorkingPrecision::Float32,
            EffectColorDomain::SceneLinearRgb,
        );
        EffectGraphExecutionRequest::new(
            frame_extent,
            EffectValueResidency::new(CPU_F32_LANE, format),
            EffectValueResidency::new(GPU_F32_LANE, format),
            budget,
        )
    }
}

/// Immutable identity expected by the caller for one GPU continuation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeterogeneousGpuContinuationBinding {
    graph_fingerprint: [u8; 32],
    generation: u64,
    frame_extent: EffectFrameExtent,
    frame_seed: i64,
    working_color_space: WorkingColorSpace,
}

impl HeterogeneousGpuContinuationBinding {
    /// Bind a continuation to one graph, generation, frame, and working-space
    /// identity.
    pub const fn new(
        graph_fingerprint: [u8; 32],
        generation: u64,
        frame_extent: EffectFrameExtent,
        frame_seed: i64,
        working_color_space: WorkingColorSpace,
    ) -> Self {
        Self {
            graph_fingerprint,
            generation,
            frame_extent,
            frame_seed,
            working_color_space,
        }
    }

    /// Complete compiled-graph semantic fingerprint.
    pub const fn graph_fingerprint(self) -> [u8; 32] {
        self.graph_fingerprint
    }

    /// Caller-owned execution generation.
    pub const fn generation(self) -> u64 {
        self.generation
    }

    /// Exact frame extent.
    pub const fn frame_extent(self) -> EffectFrameExtent {
        self.frame_extent
    }

    /// Deterministic frame seed.
    pub const fn frame_seed(self) -> i64 {
        self.frame_seed
    }

    /// Sequence working-space identity carried by the CPU and GPU frame.
    pub const fn working_color_space(self) -> WorkingColorSpace {
        self.working_color_space
    }
}

/// Explicit renderer resource grant for one continuation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeterogeneousGpuResourceGrant {
    max_upload_bytes: u64,
    max_device_bytes: u64,
    max_readback_bytes: u64,
}

impl HeterogeneousGpuResourceGrant {
    /// Construct exact upload, live-device, and optional readback limits.
    pub const fn new(
        max_upload_bytes: u64,
        max_device_bytes: u64,
        max_readback_bytes: u64,
    ) -> Self {
        Self {
            max_upload_bytes,
            max_device_bytes,
            max_readback_bytes,
        }
    }

    /// Maximum bytes transferred into the GPU.
    pub const fn max_upload_bytes(self) -> u64 {
        self.max_upload_bytes
    }

    /// Maximum physical device bytes admitted for this Adapter recording.
    pub const fn max_device_bytes(self) -> u64 {
        self.max_device_bytes
    }

    /// Maximum padded readback bytes. Zero forbids readback.
    pub const fn max_readback_bytes(self) -> u64 {
        self.max_readback_bytes
    }
}

/// Immutable upload and physical device-residency demand known before any CPU
/// prefix pixels execute.
///
/// The device value describes this wgpu Adapter's current one-command-buffer,
/// non-aliasing recording strategy. It is deliberately distinct from the
/// abstract graph plan's optimal live-set peak.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeterogeneousGpuRecordingRequirements {
    upload_bytes: u64,
    device_bytes: u64,
}

impl HeterogeneousGpuRecordingRequirements {
    pub(crate) fn from_prepared(
        plan: &mondrian_effects::CompiledEffectValueExecutionPlan,
        suffix: &PreparedHeterogeneousGpuSuffix,
    ) -> Result<Self, HeterogeneousGpuContinuationError> {
        Ok(Self {
            upload_bytes: plan.transfer_bytes(),
            device_bytes: gpu_recording_device_bytes(plan, suffix)?.max(plan.peak_device_bytes()),
        })
    }

    /// Exact CPU-to-GPU upload bytes.
    pub const fn upload_bytes(self) -> u64 {
        self.upload_bytes
    }

    /// Conservative physical device bytes required while recording and
    /// executing the one-submission suffix.
    pub const fn device_bytes(self) -> u64 {
        self.device_bytes
    }

    /// Reject a resource decision before CPU prefix execution begins.
    pub fn validate(
        self,
        grant: HeterogeneousGpuResourceGrant,
    ) -> Result<(), HeterogeneousGpuContinuationError> {
        enforce_resource_grant(
            HeterogeneousGpuResourceKind::UploadBytes,
            self.upload_bytes,
            grant.max_upload_bytes(),
        )?;
        enforce_resource_grant(
            HeterogeneousGpuResourceKind::DeviceBytes,
            self.device_bytes,
            grant.max_device_bytes(),
        )
    }
}

/// Complete request for recording one exact GPU continuation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeterogeneousGpuContinuationRequest {
    binding: HeterogeneousGpuContinuationBinding,
    grant: HeterogeneousGpuResourceGrant,
}

impl HeterogeneousGpuContinuationRequest {
    /// Construct a request from caller identity and resource authority.
    pub const fn new(
        binding: HeterogeneousGpuContinuationBinding,
        grant: HeterogeneousGpuResourceGrant,
    ) -> Self {
        Self { binding, grant }
    }

    /// Caller identity binding.
    pub const fn binding(self) -> HeterogeneousGpuContinuationBinding {
        self.binding
    }

    /// Resource authority.
    pub const fn grant(self) -> HeterogeneousGpuResourceGrant {
        self.grant
    }
}

/// Opaque process-local identity binding one recorded continuation through
/// submission and completion evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HeterogeneousGpuBatchId(u64);

impl HeterogeneousGpuBatchId {
    /// Numeric identity for bounded diagnostics and tests.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Renderer resources borrowed while recording into a caller-owned command
/// encoder and texture table.
pub struct HeterogeneousGpuRecordResources<'a> {
    device: &'a wgpu::Device,
    queue: &'a wgpu::Queue,
    encoder: &'a mut wgpu::CommandEncoder,
    ids: &'a mut GpuColorFrameIdAllocator,
    table: &'a mut GpuColorFrameResourceTable<GpuColorFrameWgpuResource>,
    compositor: &'a GpuFrameCompositor,
    resource_pool: Option<&'a Arc<GpuColorFrameWgpuResourcePool>>,
}

impl<'a> HeterogeneousGpuRecordResources<'a> {
    /// Bind the caller-owned GPU recording resources.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        device: &'a wgpu::Device,
        queue: &'a wgpu::Queue,
        encoder: &'a mut wgpu::CommandEncoder,
        ids: &'a mut GpuColorFrameIdAllocator,
        table: &'a mut GpuColorFrameResourceTable<GpuColorFrameWgpuResource>,
        compositor: &'a GpuFrameCompositor,
        resource_pool: Option<&'a Arc<GpuColorFrameWgpuResourcePool>>,
    ) -> Self {
        Self {
            device,
            queue,
            encoder,
            ids,
            table,
            compositor,
            resource_pool,
        }
    }

    /// Consume one CPU completion and record its exact GPU suffix.
    pub fn record(
        &mut self,
        request: HeterogeneousGpuContinuationRequest,
        completion: PreparedHeterogeneousCpuCompletion,
    ) -> Result<HeterogeneousGpuRecordedContinuation, HeterogeneousGpuContinuationError> {
        record_heterogeneous_gpu_continuation(self, request, completion)
    }
}

/// Resource dimension rejected by the continuation grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeterogeneousGpuResourceKind {
    /// CPU-to-GPU payload bytes.
    UploadBytes,
    /// Plan-proven peak device residency.
    DeviceBytes,
    /// Padded GPU-to-CPU readback bytes.
    ReadbackBytes,
}

/// Why an exact heterogeneous GPU continuation could not advance.
#[derive(Debug, thiserror::Error)]
pub enum HeterogeneousGpuContinuationError {
    /// No new process-local continuation batch identity remains.
    #[error("heterogeneous GPU continuation batch identity space is exhausted")]
    BatchIdentityExhausted,
    /// A prior submitted attempt failed without complete reusable-resource
    /// evidence, so this runtime can no longer admit another frame.
    #[error("heterogeneous GPU continuation runtime is poisoned after submitted work failed")]
    RuntimePoisoned,
    /// Caller identity does not match the CPU completion.
    #[error("heterogeneous GPU continuation binding mismatch for {field}")]
    BindingMismatch {
        /// Stable mismatched field name.
        field: &'static str,
    },
    /// The completion's graph-value plan and evidence disagree.
    #[error("invalid heterogeneous GPU continuation plan: {reason}")]
    InvalidPlan {
        /// Stable invariant name.
        reason: &'static str,
    },
    /// The renderer currently executes only scene-linear Float32 suffixes.
    #[error("heterogeneous GPU continuation does not support Effect domain {domain:?}")]
    UnsupportedEffectDomain {
        /// Rejected suffix processing domain.
        domain: EffectColorDomain,
    },
    /// A required resource exceeds caller authority.
    #[error(
        "heterogeneous GPU continuation requires {required} {kind:?}, exceeding grant {limit}"
    )]
    ResourceGrantExceeded {
        /// Rejected resource dimension.
        kind: HeterogeneousGpuResourceKind,
        /// Required bytes.
        required: u64,
        /// Granted bytes.
        limit: u64,
    },
    /// GPU frame identity allocation failed.
    #[error(transparent)]
    FrameIdentity(#[from] crate::GpuColorFrameIdAllocationError),
    /// The compositor could not allocate its process-unique binding identity.
    #[error(transparent)]
    CompositorCreate(#[from] crate::GpuColorFrameBindGroupCacheKeyAllocationError),
    /// CPU pixels could not be packed for upload.
    #[error("heterogeneous GPU upload plan failed: {0:?}")]
    UploadPlan(GpuColorFrameUploadError),
    /// The shared renderer resource table rejected a resource.
    #[error("heterogeneous GPU resource table failed: {0:?}")]
    ResourceTable(GpuColorFrameResourceTableError),
    /// GPU suffix command recording failed.
    #[error("heterogeneous GPU suffix recording failed: {0}")]
    GpuRecord(#[from] GpuCompositeError),
    /// GPU output readback planning or recording failed.
    #[error("heterogeneous GPU readback failed: {0:?}")]
    Readback(GpuColorFrameReadbackError),
    /// Waiting for the exact submitted work failed.
    #[error("heterogeneous GPU submission wait failed: {reason}")]
    DevicePoll {
        /// Backend diagnostic.
        reason: String,
    },
    /// Caller cancellation became authoritative before readback completed.
    #[error("heterogeneous GPU continuation was canceled")]
    Canceled,
    /// The caller's non-renewing monotonic deadline expired before readback completed.
    #[error("heterogeneous GPU continuation exceeded its monotonic deadline")]
    DeadlineExceeded,
    /// The wgpu map callback disappeared before reporting a result.
    #[error("heterogeneous GPU readback map callback was dropped")]
    MapCallbackDropped,
    /// Mapping the readback buffer failed.
    #[error("heterogeneous GPU readback mapping failed: {reason}")]
    MapFailed {
        /// Backend diagnostic.
        reason: String,
    },
    /// Mapped scalar output could not form RGBA pixels.
    #[error("heterogeneous GPU readback returned {components} scalar components")]
    InvalidReadbackComponents {
        /// Scalar component count.
        components: usize,
    },
}

/// Evidence that CPU pixels were uploaded and the exact GPU suffix was
/// recorded into a caller-owned encoder.
///
/// This state proves no queue submission and no GPU completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeterogeneousGpuRecordedEvidence {
    batch_id: HeterogeneousGpuBatchId,
    graph_fingerprint: [u8; 32],
    generation: u64,
    frame_extent: EffectFrameExtent,
    frame_seed: i64,
    upload_wait: EffectCompletionToken,
    upload_signal: EffectCompletionToken,
    output_signal: EffectCompletionToken,
    gpu_nodes: Arc<[mondrian_effects::EffectGraphNodeId]>,
    upload_bytes: u64,
    peak_device_bytes: u64,
    recorded_device_bytes: u64,
}

impl HeterogeneousGpuRecordedEvidence {
    /// Opaque record/submission/completion correlation identity.
    pub const fn batch_id(&self) -> HeterogeneousGpuBatchId {
        self.batch_id
    }

    /// Compiled graph represented by the recorded continuation.
    pub const fn graph_fingerprint(&self) -> [u8; 32] {
        self.graph_fingerprint
    }

    /// Caller execution generation.
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Exact frame extent.
    pub const fn frame_extent(&self) -> EffectFrameExtent {
        self.frame_extent
    }

    /// Deterministic frame seed.
    pub const fn frame_seed(&self) -> i64 {
        self.frame_seed
    }

    /// CPU completion token the upload is ordered after.
    pub const fn upload_wait(&self) -> EffectCompletionToken {
        self.upload_wait
    }

    /// Token that remains pending until the recorded upload is submitted and
    /// completed.
    pub const fn upload_signal(&self) -> EffectCompletionToken {
        self.upload_signal
    }

    /// Final graph token that remains pending until GPU completion.
    pub const fn output_signal(&self) -> EffectCompletionToken {
        self.output_signal
    }

    /// Exact GPU suffix nodes in compiled topology order.
    pub fn gpu_nodes(&self) -> &[mondrian_effects::EffectGraphNodeId] {
        &self.gpu_nodes
    }

    /// Exact uploaded bytes.
    pub const fn upload_bytes(&self) -> u64 {
        self.upload_bytes
    }

    /// Graph-plan peak device bytes.
    pub const fn peak_device_bytes(&self) -> u64 {
        self.peak_device_bytes
    }

    /// Actual device bytes retained by this one-submission wgpu recording
    /// strategy. This can exceed the abstract plan peak when DAG releases
    /// cannot physically alias textures inside the same command buffer.
    pub const fn recorded_device_bytes(&self) -> u64 {
        self.recorded_device_bytes
    }
}

struct RetainedGpuResources {
    resources: Vec<GpuColorFrameResource<GpuColorFrameWgpuResource>>,
    pool: Option<Arc<GpuColorFrameWgpuResourcePool>>,
    reusable: bool,
}

impl RetainedGpuResources {
    fn new(
        resources: Vec<GpuColorFrameResource<GpuColorFrameWgpuResource>>,
        pool: Option<Arc<GpuColorFrameWgpuResourcePool>>,
    ) -> Self {
        Self { resources, pool, reusable: false }
    }

    fn mark_completed(&mut self) {
        self.reusable = true;
    }
}

impl Drop for RetainedGpuResources {
    fn drop(&mut self) {
        if self.reusable
            && let Some(pool) = &self.pool
        {
            for resource in self.resources.drain(..) {
                pool.release(resource);
            }
        }
    }
}

/// Exact GPU continuation recorded into a caller-owned encoder.
///
/// The output handle can immediately feed later passes recorded in the same
/// encoder and resource table.
pub struct HeterogeneousGpuRecordedContinuation {
    output: crate::GpuColorFrameHandle,
    evidence: HeterogeneousGpuRecordedEvidence,
    retained_resources: RetainedGpuResources,
}

impl HeterogeneousGpuRecordedContinuation {
    /// GPU output usable by later commands in the same encoder.
    pub const fn output(&self) -> &crate::GpuColorFrameHandle {
        &self.output
    }

    /// Recording-only evidence.
    pub const fn evidence(&self) -> &HeterogeneousGpuRecordedEvidence {
        &self.evidence
    }

    /// Assert that a Presentation Adapter submitted the command buffer
    /// containing this record.
    ///
    /// The renderer cannot inspect a caller-owned command buffer after
    /// submission. This is therefore an explicit trusted Adapter assertion,
    /// correlated by the renderer-owned batch identity; it is not self-proving
    /// execution evidence. The returned state still proves no GPU completion.
    pub fn assert_adapter_submission(
        self,
        submission_index: wgpu::SubmissionIndex,
    ) -> HeterogeneousGpuSubmittedContinuation {
        self.bind_submission(
            submission_index,
            HeterogeneousGpuSubmissionAuthority::TrustedAdapterAssertion,
        )
    }

    fn bind_renderer_owned_submission(
        self,
        submission_index: wgpu::SubmissionIndex,
    ) -> HeterogeneousGpuSubmittedContinuation {
        self.bind_submission(
            submission_index,
            HeterogeneousGpuSubmissionAuthority::RendererOwnedBatch,
        )
    }

    fn bind_submission(
        self,
        submission_index: wgpu::SubmissionIndex,
        authority: HeterogeneousGpuSubmissionAuthority,
    ) -> HeterogeneousGpuSubmittedContinuation {
        HeterogeneousGpuSubmittedContinuation {
            output: self.output,
            evidence: HeterogeneousGpuSubmittedEvidence {
                recorded: self.evidence,
                submission_index,
                authority,
            },
            retained_resources: self.retained_resources,
        }
    }
}

/// Authority that associated a recorded batch with one queue submission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeterogeneousGpuSubmissionAuthority {
    /// The renderer owned the encoder through `finish` and `Queue::submit`.
    RendererOwnedBatch,
    /// A Presentation Adapter asserted that its submitted command buffer
    /// contains this renderer-owned batch identity.
    TrustedAdapterAssertion,
}

/// Evidence that the encoder containing the continuation was submitted.
///
/// Upload and output tokens remain pending until the exact submission
/// completes.
#[derive(Debug, Clone)]
pub struct HeterogeneousGpuSubmittedEvidence {
    recorded: HeterogeneousGpuRecordedEvidence,
    submission_index: wgpu::SubmissionIndex,
    authority: HeterogeneousGpuSubmissionAuthority,
}

impl HeterogeneousGpuSubmittedEvidence {
    /// Opaque identity shared with Recorded and Completed evidence.
    pub const fn batch_id(&self) -> HeterogeneousGpuBatchId {
        self.recorded.batch_id
    }

    /// Recording evidence bound to this submission.
    pub const fn recorded(&self) -> &HeterogeneousGpuRecordedEvidence {
        &self.recorded
    }

    /// Exact wgpu queue submission identity.
    pub const fn submission_index(&self) -> &wgpu::SubmissionIndex {
        &self.submission_index
    }

    /// Authority that bound the renderer batch to this submission.
    pub const fn authority(&self) -> HeterogeneousGpuSubmissionAuthority {
        self.authority
    }
}

/// GPU continuation admitted to one exact queue submission.
pub struct HeterogeneousGpuSubmittedContinuation {
    output: crate::GpuColorFrameHandle,
    evidence: HeterogeneousGpuSubmittedEvidence,
    retained_resources: RetainedGpuResources,
}

impl HeterogeneousGpuSubmittedContinuation {
    /// GPU output retained while the submission is in flight.
    pub const fn output(&self) -> &crate::GpuColorFrameHandle {
        &self.output
    }

    /// Submission evidence.
    pub const fn evidence(&self) -> &HeterogeneousGpuSubmittedEvidence {
        &self.evidence
    }

    /// Wait for this exact submission before one non-renewing monotonic
    /// deadline and prove upload plus suffix completion.
    pub fn wait_until(
        self,
        device: &wgpu::Device,
        deadline: Instant,
    ) -> Result<HeterogeneousGpuCompletedContinuation, HeterogeneousGpuContinuationError> {
        let timeout = deadline.saturating_duration_since(Instant::now());
        if timeout.is_zero() {
            return Err(HeterogeneousGpuContinuationError::DeadlineExceeded);
        }
        match device.poll(wgpu::PollType::Wait {
            submission_index: Some(self.evidence.submission_index.clone()),
            timeout: Some(timeout),
        }) {
            Ok(_) => {}
            Err(wgpu::PollError::Timeout) => {
                return Err(HeterogeneousGpuContinuationError::DeadlineExceeded);
            }
            Err(error) => {
                return Err(HeterogeneousGpuContinuationError::DevicePoll {
                    reason: error.to_string(),
                });
            }
        }
        Ok(self.complete_after_observed_submission())
    }

    /// Register a short completion callback for an Adapter-owned submission.
    ///
    /// The Adapter must call this immediately after binding this continuation
    /// to the [`wgpu::SubmissionIndex`] returned by the `Queue::submit` that
    /// contains its recorded commands. `wgpu` invokes the callback only after
    /// all work submitted before this registration has completed. Work from a
    /// shared queue that races ahead of registration can therefore delay this
    /// callback conservatively, but cannot make it fire early.
    ///
    /// The callback runs inside whichever later `Queue::submit`,
    /// `Device::poll`, or Instance poll drives `wgpu`; it must remain short.
    /// Window adapters should only `try_send` the completed value to their
    /// event-loop inbox and perform generation validation plus publication on
    /// a later UI tick.
    pub fn register_completion_callback(
        self,
        queue: &wgpu::Queue,
        callback: impl FnOnce(HeterogeneousGpuCompletedContinuation) + Send + 'static,
    ) {
        queue.on_submitted_work_done(move || {
            callback(self.complete_after_observed_submission());
        });
    }

    pub(crate) fn complete_after_observed_submission(
        mut self,
    ) -> HeterogeneousGpuCompletedContinuation {
        self.retained_resources.mark_completed();
        HeterogeneousGpuCompletedContinuation {
            output: self.output,
            evidence: HeterogeneousGpuCompletedEvidence {
                submitted: self.evidence,
                readback_bytes: None,
            },
            _retained_resources: self.retained_resources,
        }
    }
}

/// Evidence that upload and the exact GPU suffix completed.
#[derive(Debug, Clone)]
pub struct HeterogeneousGpuCompletedEvidence {
    submitted: HeterogeneousGpuSubmittedEvidence,
    readback_bytes: Option<u64>,
}

impl HeterogeneousGpuCompletedEvidence {
    /// Opaque identity shared with Recorded and Submitted evidence.
    pub const fn batch_id(&self) -> HeterogeneousGpuBatchId {
        self.submitted.batch_id()
    }

    /// Exact submission whose GPU work completed.
    pub const fn submitted(&self) -> &HeterogeneousGpuSubmittedEvidence {
        &self.submitted
    }

    /// Recording identity and token chain that completed.
    pub const fn recorded(&self) -> &HeterogeneousGpuRecordedEvidence {
        self.submitted.recorded()
    }

    /// Completed upload token.
    pub const fn completed_upload_token(&self) -> EffectCompletionToken {
        self.submitted.recorded.upload_signal
    }

    /// Completed final graph-output token.
    pub const fn completed_output_token(&self) -> EffectCompletionToken {
        self.submitted.recorded.output_signal
    }

    /// Padded bytes read back to CPU, if an explicit readback completed.
    pub const fn readback_bytes(&self) -> Option<u64> {
        self.readback_bytes
    }
}

/// GPU continuation whose exact queue submission completed.
pub struct HeterogeneousGpuCompletedContinuation {
    output: crate::GpuColorFrameHandle,
    evidence: HeterogeneousGpuCompletedEvidence,
    _retained_resources: RetainedGpuResources,
}

impl HeterogeneousGpuCompletedContinuation {
    /// Completed GPU output.
    pub const fn output(&self) -> &crate::GpuColorFrameHandle {
        &self.output
    }

    /// GPU completion evidence.
    pub const fn evidence(&self) -> &HeterogeneousGpuCompletedEvidence {
        &self.evidence
    }
}

/// Completed GPU continuation materialized back into a CPU working frame.
pub struct HeterogeneousGpuCompletedFrame {
    frame: CpuColorFrame,
    evidence: HeterogeneousGpuCompletedEvidence,
}

impl HeterogeneousGpuCompletedFrame {
    /// CPU working frame after the exact GPU suffix.
    pub const fn frame(&self) -> &CpuColorFrame {
        &self.frame
    }

    /// Completion evidence including explicit readback.
    pub const fn evidence(&self) -> &HeterogeneousGpuCompletedEvidence {
        &self.evidence
    }

    /// Consume the result into its CPU frame.
    pub fn into_frame(self) -> CpuColorFrame {
        self.frame
    }
}

/// Record one exact CPU-completed heterogeneous continuation into
/// caller-owned renderer resources.
pub fn record_heterogeneous_gpu_continuation(
    resources: &mut HeterogeneousGpuRecordResources<'_>,
    request: HeterogeneousGpuContinuationRequest,
    completion: PreparedHeterogeneousCpuCompletion,
) -> Result<HeterogeneousGpuRecordedContinuation, HeterogeneousGpuContinuationError> {
    validate_completion(request, &completion)?;
    let batch_id = allocate_heterogeneous_gpu_batch_id()?;
    let (pixels, execution_plan, gpu_suffix, cpu_evidence) = completion.into_parts();
    let binding = request.binding();
    let recorded_device_bytes = gpu_recording_device_bytes(&execution_plan, &gpu_suffix)?;
    let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
        width: binding.frame_extent().width(),
        height: binding.frame_extent().height(),
        color_space: binding.working_color_space(),
        data: pixels,
    });
    let upload_plan = GpuColorFrameUploadPlan::from_cpu_color_frame(
        resources.ids.allocate()?,
        &frame,
        GpuColorFrameTextureFormat::Rgba32Float,
        "heterogeneous-effect-cpu-prefix",
    )
    .map_err(HeterogeneousGpuContinuationError::UploadPlan)?;
    let uploaded = match resources.resource_pool {
        Some(pool) => GpuColorFrameUploader::upload_with_pool(
            resources.device,
            resources.queue,
            &upload_plan,
            pool,
        ),
        None => GpuColorFrameUploader::upload(resources.device, resources.queue, &upload_plan),
    };
    let uploaded_handle = uploaded.handle().clone();
    resources
        .table
        .insert(uploaded)
        .map_err(HeterogeneousGpuContinuationError::ResourceTable)?;
    let (output, retained) = record_gpu_suffix(
        resources,
        &gpu_suffix,
        uploaded_handle,
        binding.frame_seed(),
        binding.working_color_space(),
    )?;
    Ok(HeterogeneousGpuRecordedContinuation {
        output,
        evidence: HeterogeneousGpuRecordedEvidence {
            batch_id,
            graph_fingerprint: binding.graph_fingerprint(),
            generation: binding.generation(),
            frame_extent: binding.frame_extent(),
            frame_seed: binding.frame_seed(),
            upload_wait: cpu_evidence.required_transfer_wait(),
            upload_signal: cpu_evidence.pending_gpu_input_token(),
            output_signal: cpu_evidence.pending_output_token(),
            gpu_nodes: gpu_suffix.node_ids().into(),
            upload_bytes: execution_plan.transfer_bytes(),
            peak_device_bytes: execution_plan.peak_device_bytes(),
            recorded_device_bytes,
        },
        retained_resources: RetainedGpuResources::new(
            retained,
            resources.resource_pool.map(Arc::clone),
        ),
    })
}

fn record_gpu_suffix(
    resources: &mut HeterogeneousGpuRecordResources<'_>,
    suffix: &PreparedHeterogeneousGpuSuffix,
    uploaded_handle: crate::GpuColorFrameHandle,
    frame_seed: i64,
    working_color_space: WorkingColorSpace,
) -> Result<
    (
        crate::GpuColorFrameHandle,
        Vec<GpuColorFrameResource<GpuColorFrameWgpuResource>>,
    ),
    HeterogeneousGpuContinuationError,
> {
    let mut handles = HashMap::from([(suffix.input_materialization(), uploaded_handle)]);
    let mut retained = Vec::new();
    let result = (|| {
        for step in suffix.steps() {
            match step {
                PreparedHeterogeneousGpuStep::Dispatch(
                    PreparedHeterogeneousGpuDispatch::PointChain { input, output, plan, .. },
                ) => {
                    let input_handle = handles.get(input).ok_or(
                        HeterogeneousGpuContinuationError::InvalidPlan {
                            reason: "gpu_point_input_not_materialized",
                        },
                    )?;
                    let record = resources.compositor.record_point_effect_pass(
                        resources.device,
                        resources.queue,
                        resources.encoder,
                        resources.ids,
                        resources.table,
                        resources.resource_pool.map(Arc::as_ref),
                        input_handle,
                        plan,
                        frame_seed,
                    )?;
                    if handles.insert(*output, record.output).is_some() {
                        return Err(HeterogeneousGpuContinuationError::InvalidPlan {
                            reason: "gpu_dispatch_replaced_materialization",
                        });
                    }
                }
                PreparedHeterogeneousGpuStep::Dispatch(
                    PreparedHeterogeneousGpuDispatch::NormalBlend {
                        base,
                        overlay,
                        output,
                        opacity,
                        ..
                    },
                ) => {
                    let base_handle = handles.get(base).ok_or(
                        HeterogeneousGpuContinuationError::InvalidPlan {
                            reason: "gpu_blend_base_not_materialized",
                        },
                    )?;
                    let overlay_handle = handles.get(overlay).ok_or(
                        HeterogeneousGpuContinuationError::InvalidPlan {
                            reason: "gpu_blend_overlay_not_materialized",
                        },
                    )?;
                    let record = resources.compositor.record_adjustment_blend_pass(
                        resources.device,
                        resources.queue,
                        resources.encoder,
                        resources.ids,
                        resources.table,
                        resources.resource_pool.map(Arc::as_ref),
                        base_handle,
                        overlay_handle,
                        *opacity,
                        mondrian_core::BlendMode::Normal,
                        working_color_space,
                    )?;
                    if handles.insert(*output, record.output).is_some() {
                        return Err(HeterogeneousGpuContinuationError::InvalidPlan {
                            reason: "gpu_dispatch_replaced_materialization",
                        });
                    }
                }
                PreparedHeterogeneousGpuStep::Release { materialization, .. } => {
                    let handle = handles.remove(materialization).ok_or(
                        HeterogeneousGpuContinuationError::InvalidPlan {
                            reason: "gpu_release_materialization_missing",
                        },
                    )?;
                    let resource = resources
                        .table
                        .take(&handle)
                        .map_err(HeterogeneousGpuContinuationError::ResourceTable)?;
                    retained.push(resource);
                }
            }
        }
        let output = handles.remove(&suffix.output_materialization()).ok_or(
            HeterogeneousGpuContinuationError::InvalidPlan {
                reason: "gpu_output_not_materialized",
            },
        )?;
        if !handles.is_empty() {
            return Err(HeterogeneousGpuContinuationError::InvalidPlan {
                reason: "gpu_suffix_left_unreleased_materializations",
            });
        }
        Ok(output)
    })();
    match result {
        Ok(output) => Ok((output, retained)),
        Err(error) => {
            for handle in handles.into_values() {
                if let Some(resource) = resources.table.remove(handle.id()) {
                    retained.push(resource);
                }
            }
            drop(retained);
            Err(error)
        }
    }
}

/// Owned renderer Module for offline submit, exact wait, and CPU readback.
///
/// Export can retain one instance per job and Preview/Headless can use the
/// lower-level recording Interface when it must share an encoder/table.
pub struct HeterogeneousGpuContinuationRuntime {
    context: Arc<GpuContext>,
    compositor: GpuFrameCompositor,
    ids: GpuColorFrameIdAllocator,
    table: GpuColorFrameResourceTable<GpuColorFrameWgpuResource>,
    resource_pool: Arc<GpuColorFrameWgpuResourcePool>,
    poisoned: bool,
}

/// Owns one pending or completed read mapping until every mapped view has been
/// dropped. Dropping the lease also cancels a still-pending mapping request, so
/// no terminal path can leave a buffer mapped across later queue submission.
struct HeterogeneousGpuReadbackMapLease<'a> {
    buffer: &'a wgpu::Buffer,
}

impl<'a> HeterogeneousGpuReadbackMapLease<'a> {
    fn new(buffer: &'a wgpu::Buffer) -> Self {
        Self { buffer }
    }
}

impl Drop for HeterogeneousGpuReadbackMapLease<'_> {
    fn drop(&mut self) {
        self.buffer.unmap();
    }
}

impl HeterogeneousGpuContinuationRuntime {
    /// Construct a job-local runtime over a caller-selected GPU context.
    pub fn new(
        context: Arc<GpuContext>,
        resource_pool_options: GpuColorFrameWgpuResourcePoolOptions,
    ) -> Result<Self, HeterogeneousGpuContinuationError> {
        Self::with_resource_pool(
            context,
            Arc::new(GpuColorFrameWgpuResourcePool::new(resource_pool_options)),
        )
    }

    /// Construct a job-local runtime that shares an existing renderer texture
    /// pool with other attempt-local GPU paths.
    pub fn with_resource_pool(
        context: Arc<GpuContext>,
        resource_pool: Arc<GpuColorFrameWgpuResourcePool>,
    ) -> Result<Self, HeterogeneousGpuContinuationError> {
        let compositor = GpuFrameCompositor::new(&context.device)
            .map_err(HeterogeneousGpuContinuationError::CompositorCreate)?;
        let ids = GpuColorFrameIdAllocator::new(1)?;
        Ok(Self {
            context,
            compositor,
            ids,
            table: GpuColorFrameResourceTable::new(),
            resource_pool,
            poisoned: false,
        })
    }

    /// Whether submitted work failed without complete reusable-resource
    /// evidence. A poisoned runtime rejects every later frame.
    pub const fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    /// Execute upload plus exact GPU suffix, wait for that submission, and
    /// read the working Float32 output back to CPU.
    ///
    /// `deadline` is one non-renewing monotonic bound for submission completion
    /// and readback mapping. Cancellation or deadline expiry before submission
    /// rolls the candidate back. Once submitted, a terminal wait failure
    /// abandons rather than reuses resources whose completion is unproved and
    /// poisons this job-local runtime.
    pub fn execute_to_cpu(
        &mut self,
        request: HeterogeneousGpuContinuationRequest,
        completion: PreparedHeterogeneousCpuCompletion,
        cancellation: &ExecutionCancellationToken,
        deadline: Instant,
    ) -> Result<HeterogeneousGpuCompletedFrame, HeterogeneousGpuContinuationError> {
        if self.poisoned {
            return Err(HeterogeneousGpuContinuationError::RuntimePoisoned);
        }
        if let Some(error) =
            heterogeneous_gpu_execution_stop(cancellation.is_canceled(), Instant::now(), deadline)
        {
            return Err(error);
        }
        let mut encoder =
            self.context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("heterogeneous-effect-continuation"),
            });
        let recorded_result = {
            let mut resources = HeterogeneousGpuRecordResources::new(
                &self.context.device,
                &self.context.queue,
                &mut encoder,
                &mut self.ids,
                &mut self.table,
                &self.compositor,
                Some(&self.resource_pool),
            );
            resources.record(request, completion)
        };
        let recorded = match recorded_result {
            Ok(recorded) => recorded,
            Err(error) => {
                self.compositor.clear_frame_resources();
                return Err(error);
            }
        };
        let output_id = recorded.output().id();
        let readback_plan =
            match GpuColorFrameReadbackPlan::encoded_rgba32float(recorded.output().clone())
                .map_err(HeterogeneousGpuContinuationError::Readback)
            {
                Ok(plan) => plan,
                Err(error) => {
                    self.rollback_before_submission(output_id);
                    drop(recorded);
                    return Err(error);
                }
            };
        if let Err(error) = enforce_resource_grant(
            HeterogeneousGpuResourceKind::ReadbackBytes,
            readback_plan.buffer_size,
            request.grant().max_readback_bytes(),
        ) {
            self.rollback_before_submission(output_id);
            drop(recorded);
            return Err(error);
        }
        let readback_result = self
            .table
            .get(recorded.output())
            .map_err(HeterogeneousGpuContinuationError::ResourceTable)
            .and_then(|output_resource| {
                GpuColorFrameReadback::record_copy(
                    &self.context.device,
                    &mut encoder,
                    &readback_plan,
                    output_resource,
                )
                .map_err(HeterogeneousGpuContinuationError::Readback)
            });
        let readback = match readback_result {
            Ok(readback) => readback,
            Err(error) => {
                self.rollback_before_submission(output_id);
                drop(recorded);
                return Err(error);
            }
        };
        if let Some(error) =
            heterogeneous_gpu_execution_stop(cancellation.is_canceled(), Instant::now(), deadline)
        {
            self.rollback_before_submission(output_id);
            drop(recorded);
            return Err(error);
        }
        let submission = self.context.queue.submit(std::iter::once(encoder.finish()));
        let submitted = recorded.bind_renderer_owned_submission(submission);
        let submission_index = submitted.evidence().submission_index().clone();
        let slice = readback.slice(..);
        let (sender, receiver) = mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        let map_lease = HeterogeneousGpuReadbackMapLease::new(&readback);
        let map_result = loop {
            match receiver.try_recv() {
                Ok(result) => {
                    break result.map_err(|error| HeterogeneousGpuContinuationError::MapFailed {
                        reason: error.to_string(),
                    });
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    break Err(HeterogeneousGpuContinuationError::MapCallbackDropped);
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }

            let poll_timeout = match heterogeneous_gpu_poll_timeout(
                cancellation.is_canceled(),
                Instant::now(),
                deadline,
            ) {
                Ok(timeout) => timeout,
                Err(error) => {
                    drop(map_lease);
                    drop(submitted);
                    self.poison_after_submission(output_id, false);
                    return Err(error);
                }
            };
            match self.context.device.poll(wgpu::PollType::Wait {
                submission_index: Some(submission_index.clone()),
                timeout: Some(poll_timeout),
            }) {
                Ok(_) | Err(wgpu::PollError::Timeout) => {}
                Err(error) => {
                    drop(map_lease);
                    drop(submitted);
                    self.poison_after_submission(output_id, false);
                    return Err(HeterogeneousGpuContinuationError::DevicePoll {
                        reason: error.to_string(),
                    });
                }
            }
        };
        if let Err(error) = map_result {
            drop(map_lease);
            drop(submitted);
            self.poison_after_submission(output_id, false);
            return Err(error);
        }

        // A successful MAP_READ callback for this copy destination is stronger
        // than recording evidence: the exact submission has completed and its
        // output/upload resources may now become reusable.
        let mut completed = submitted.complete_after_observed_submission();
        let mapped = match slice.get_mapped_range() {
            Ok(mapped) => mapped,
            Err(error) => {
                drop(map_lease);
                drop(completed);
                self.poison_after_submission(output_id, true);
                return Err(HeterogeneousGpuContinuationError::MapFailed {
                    reason: error.to_string(),
                });
            }
        };
        let components_result = readback_plan
            .unpack_mapped_rgba32float(&mapped)
            .map_err(HeterogeneousGpuContinuationError::Readback);
        drop(mapped);
        drop(map_lease);
        if let Some(error) =
            heterogeneous_gpu_execution_stop(cancellation.is_canceled(), Instant::now(), deadline)
        {
            drop(completed);
            self.release_output(output_id);
            self.compositor.clear_frame_resources();
            return Err(error);
        }
        let components = match components_result {
            Ok(components) => components,
            Err(error) => {
                drop(completed);
                self.poison_after_submission(output_id, true);
                return Err(error);
            }
        };
        let pixels = match rgba_components_to_pixels(components) {
            Ok(pixels) => pixels,
            Err(error) => {
                drop(completed);
                self.poison_after_submission(output_id, true);
                return Err(error);
            }
        };
        let binding = request.binding();
        completed.evidence.readback_bytes = Some(readback_plan.buffer_size);
        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            width: binding.frame_extent().width(),
            height: binding.frame_extent().height(),
            color_space: binding.working_color_space(),
            data: pixels,
        });
        let evidence = completed.evidence.clone();
        self.release_output(completed.output.id());
        drop(completed);
        self.compositor.clear_frame_resources();
        Ok(HeterogeneousGpuCompletedFrame { frame, evidence })
    }

    fn rollback_before_submission(&mut self, output: crate::GpuColorFrameId) {
        self.release_output(output);
        self.compositor.clear_frame_resources();
    }

    fn poison_after_submission(&mut self, output: crate::GpuColorFrameId, completed: bool) {
        if let Some(resource) = self.table.remove(output)
            && completed
        {
            self.resource_pool.release(resource);
        }
        self.resource_pool.clear();
        self.compositor.clear_frame_resources();
        self.poisoned = true;
    }

    fn release_output(&mut self, output: crate::GpuColorFrameId) {
        if let Some(resource) = self.table.remove(output) {
            self.resource_pool.release(resource);
        }
    }
}

fn heterogeneous_gpu_execution_stop(
    canceled: bool,
    now: Instant,
    deadline: Instant,
) -> Option<HeterogeneousGpuContinuationError> {
    if canceled {
        Some(HeterogeneousGpuContinuationError::Canceled)
    } else if now >= deadline {
        Some(HeterogeneousGpuContinuationError::DeadlineExceeded)
    } else {
        None
    }
}

fn heterogeneous_gpu_poll_timeout(
    canceled: bool,
    now: Instant,
    deadline: Instant,
) -> Result<Duration, HeterogeneousGpuContinuationError> {
    if let Some(error) = heterogeneous_gpu_execution_stop(canceled, now, deadline) {
        return Err(error);
    }
    let remaining = deadline.saturating_duration_since(now);
    let timeout = remaining.min(HETEROGENEOUS_GPU_WAIT_SLICE);
    if timeout.is_zero() {
        Err(HeterogeneousGpuContinuationError::DeadlineExceeded)
    } else {
        Ok(timeout)
    }
}

fn validate_completion(
    request: HeterogeneousGpuContinuationRequest,
    completion: &PreparedHeterogeneousCpuCompletion,
) -> Result<(), HeterogeneousGpuContinuationError> {
    let binding = request.binding();
    let evidence = completion.evidence();
    let plan = completion.execution_plan();
    let gpu_suffix = completion.gpu_suffix();
    require_binding(
        binding.graph_fingerprint() == evidence.graph_fingerprint(),
        "graph_fingerprint",
    )?;
    require_binding(binding.generation() == evidence.generation(), "generation")?;
    require_binding(
        binding.frame_extent() == evidence.frame_extent(),
        "frame_extent",
    )?;
    require_binding(binding.frame_seed() == evidence.frame_seed(), "frame_seed")?;
    require_binding(
        binding.working_color_space() == evidence.working_color_space(),
        "working_color_space",
    )?;
    require_plan(
        plan.graph_fingerprint() == evidence.graph_fingerprint(),
        "execution_plan_graph_fingerprint",
    )?;
    require_plan(
        plan.frame_extent() == evidence.frame_extent(),
        "execution_plan_frame_extent",
    )?;
    require_plan(
        plan.output_materialization() == gpu_suffix.output_materialization(),
        "gpu_output_materialization",
    )?;
    require_plan(!gpu_suffix.node_ids().is_empty(), "empty_gpu_suffix")?;
    let expected_pixels = u64::from(binding.frame_extent().width())
        .checked_mul(u64::from(binding.frame_extent().height()))
        .ok_or(HeterogeneousGpuContinuationError::InvalidPlan { reason: "pixel_count_overflow" })?;
    let actual_pixels = u64::try_from(completion.pixels().len()).map_err(|_| {
        HeterogeneousGpuContinuationError::InvalidPlan { reason: "pixel_count_conversion" }
    })?;
    require_plan(expected_pixels == actual_pixels, "cpu_pixel_count")?;
    let upload_bytes =
        expected_pixels
            .checked_mul(16)
            .ok_or(HeterogeneousGpuContinuationError::InvalidPlan {
                reason: "upload_byte_count_overflow",
            })?;
    require_plan(plan.transfer_bytes() == upload_bytes, "transfer_byte_count")?;
    let requirements = HeterogeneousGpuRecordingRequirements::from_prepared(plan, gpu_suffix)?;
    require_plan(
        requirements.upload_bytes() == upload_bytes,
        "upload_requirement_bytes",
    )?;
    requirements.validate(request.grant())?;
    validate_token_chain(plan.steps(), evidence, gpu_suffix)?;
    validate_gpu_suffix_schedule(plan, evidence, gpu_suffix)
}

fn gpu_recording_device_bytes(
    plan: &mondrian_effects::CompiledEffectValueExecutionPlan,
    suffix: &PreparedHeterogeneousGpuSuffix,
) -> Result<u64, HeterogeneousGpuContinuationError> {
    let input = plan.materialization(suffix.input_materialization()).ok_or(
        HeterogeneousGpuContinuationError::InvalidPlan {
            reason: "gpu_input_materialization_missing",
        },
    )?;
    let mut bytes = input.bytes();
    let mut outputs = HashSet::new();
    for step in suffix.steps() {
        let PreparedHeterogeneousGpuStep::Dispatch(dispatch) = step else {
            continue;
        };
        let output = dispatch.output();
        if !outputs.insert(output) {
            return Err(HeterogeneousGpuContinuationError::InvalidPlan {
                reason: "gpu_output_materialization_reused",
            });
        }
        let materialization =
            plan.materialization(output)
                .ok_or(HeterogeneousGpuContinuationError::InvalidPlan {
                    reason: "gpu_output_materialization_missing",
                })?;
        bytes = bytes.checked_add(materialization.bytes()).ok_or(
            HeterogeneousGpuContinuationError::InvalidPlan {
                reason: "gpu_recording_byte_count_overflow",
            },
        )?;
    }
    Ok(bytes)
}

fn validate_token_chain(
    steps: &[EffectGraphExecutionStep],
    evidence: &HeterogeneousCpuCompletionEvidence,
    gpu_suffix: &PreparedHeterogeneousGpuSuffix,
) -> Result<(), HeterogeneousGpuContinuationError> {
    let mut cpu_nodes = Vec::new();
    let mut gpu_nodes = Vec::new();
    let mut transfer_seen = false;
    let mut cpu_lane = None;
    let mut gpu_lane = None;
    for step in steps {
        match step {
            EffectGraphExecutionStep::Dispatch {
                node,
                lane,
                backend: EffectProcessingBackend::Cpu,
                precision,
                signal,
                ..
            } => {
                require_plan(!transfer_seen, "cpu_dispatch_after_transfer")?;
                require_plan(
                    *precision == EffectWorkingPrecision::Float32,
                    "cpu_dispatch_precision",
                )?;
                require_plan(
                    cpu_lane.is_none_or(|expected| expected == *lane),
                    "cpu_dispatch_lane",
                )?;
                cpu_lane = Some(*lane);
                cpu_nodes.push(*node);
                let _ = signal;
            }
            EffectGraphExecutionStep::Transfer { output, from, to, wait, signal, .. } => {
                require_plan(!transfer_seen, "multiple_transfers")?;
                require_plan(
                    *wait == evidence.required_transfer_wait(),
                    "transfer_wait_token",
                )?;
                require_plan(
                    *signal == evidence.pending_gpu_input_token(),
                    "transfer_signal_token",
                )?;
                require_plan(
                    from.format().precision() == EffectWorkingPrecision::Float32
                        && to.format().precision() == EffectWorkingPrecision::Float32,
                    "transfer_precision",
                )?;
                require_plan(
                    from.format().domain() == EffectColorDomain::SceneLinearRgb
                        && to.format().domain() == EffectColorDomain::SceneLinearRgb,
                    "transfer_domain",
                )?;
                require_plan(cpu_lane == Some(from.lane()), "transfer_cpu_lane")?;
                require_plan(
                    *output == gpu_suffix.input_materialization(),
                    "transfer_gpu_materialization",
                )?;
                transfer_seen = true;
                gpu_lane = Some(to.lane());
            }
            EffectGraphExecutionStep::Dispatch {
                node,
                lane,
                backend: EffectProcessingBackend::Gpu,
                precision,
                waits: _,
                signal: _,
                ..
            } => {
                require_plan(transfer_seen, "gpu_dispatch_before_transfer")?;
                require_plan(
                    *precision == EffectWorkingPrecision::Float32,
                    "gpu_dispatch_precision",
                )?;
                require_plan(gpu_lane == Some(*lane), "gpu_dispatch_lane")?;
                gpu_nodes.push(*node);
            }
            EffectGraphExecutionStep::Dispatch { .. } => {
                return Err(HeterogeneousGpuContinuationError::InvalidPlan {
                    reason: "external_dispatch_in_cpu_gpu_tracer",
                });
            }
            EffectGraphExecutionStep::Release { .. } => {}
        }
    }
    require_plan(transfer_seen, "missing_cpu_to_gpu_transfer")?;
    require_plan(
        cpu_nodes == evidence.completed_cpu_nodes(),
        "completed_cpu_node_order",
    )?;
    require_plan(gpu_nodes == gpu_suffix.node_ids(), "gpu_suffix_node_order")?;
    let output_token = steps
        .iter()
        .filter_map(|step| match step {
            EffectGraphExecutionStep::Dispatch { signal, .. }
            | EffectGraphExecutionStep::Transfer { signal, .. } => Some(*signal),
            EffectGraphExecutionStep::Release { .. } => None,
        })
        .next_back();
    require_plan(
        output_token == Some(evidence.pending_output_token()),
        "output_completion_token",
    )?;
    require_plan(
        evidence.completed_cpu_token() == evidence.required_transfer_wait(),
        "completed_cpu_transfer_wait_token",
    )
}

fn validate_gpu_suffix_schedule(
    plan: &mondrian_effects::CompiledEffectValueExecutionPlan,
    evidence: &HeterogeneousCpuCompletionEvidence,
    suffix: &PreparedHeterogeneousGpuSuffix,
) -> Result<(), HeterogeneousGpuContinuationError> {
    let mut live = HashSet::from([suffix.input_materialization()]);
    let mut completed = HashSet::from([evidence.pending_gpu_input_token()]);
    let mut nodes = Vec::new();
    for step in suffix.steps() {
        match step {
            PreparedHeterogeneousGpuStep::Dispatch(
                PreparedHeterogeneousGpuDispatch::PointChain {
                    nodes: dispatch_nodes,
                    input,
                    output,
                    waits,
                    signal,
                    plan: point_plan,
                },
            ) => {
                require_plan(live.contains(input), "gpu_point_input_not_live")?;
                require_plan(
                    waits.len() == 1 && completed.contains(&waits[0]),
                    "gpu_point_wait_not_completed",
                )?;
                let input_value = plan.materialization(*input).ok_or(
                    HeterogeneousGpuContinuationError::InvalidPlan {
                        reason: "gpu_point_input_materialization_missing",
                    },
                )?;
                let output_value = plan.materialization(*output).ok_or(
                    HeterogeneousGpuContinuationError::InvalidPlan {
                        reason: "gpu_point_output_materialization_missing",
                    },
                )?;
                require_plan(
                    input_value.completion() == waits[0]
                        && output_value.completion() == *signal
                        && input_value.value() == point_plan.source_value()
                        && output_value.value() == point_plan.output_value(),
                    "gpu_point_value_or_token_mismatch",
                )?;
                require_plan(
                    point_plan.graph_fingerprint() == evidence.graph_fingerprint(),
                    "gpu_point_graph_fingerprint",
                )?;
                if point_plan.processing_domain() != EffectColorDomain::SceneLinearRgb {
                    return Err(HeterogeneousGpuContinuationError::UnsupportedEffectDomain {
                        domain: point_plan.processing_domain(),
                    });
                }
                require_plan(
                    point_plan.node_ids() == dispatch_nodes.as_ref(),
                    "gpu_point_node_identity",
                )?;
                require_plan(live.insert(*output), "gpu_point_output_already_live")?;
                require_plan(completed.insert(*signal), "gpu_point_signal_reused")?;
                nodes.extend(dispatch_nodes.iter().copied());
            }
            PreparedHeterogeneousGpuStep::Dispatch(
                PreparedHeterogeneousGpuDispatch::NormalBlend {
                    node,
                    base,
                    overlay,
                    output,
                    waits,
                    signal,
                    ..
                },
            ) => {
                require_plan(
                    live.contains(base) && live.contains(overlay),
                    "gpu_blend_input_not_live",
                )?;
                require_plan(
                    waits.len() == 2 && waits.iter().all(|wait| completed.contains(wait)),
                    "gpu_blend_wait_not_completed",
                )?;
                let base_value = plan.materialization(*base).ok_or(
                    HeterogeneousGpuContinuationError::InvalidPlan {
                        reason: "gpu_blend_base_materialization_missing",
                    },
                )?;
                let overlay_value = plan.materialization(*overlay).ok_or(
                    HeterogeneousGpuContinuationError::InvalidPlan {
                        reason: "gpu_blend_overlay_materialization_missing",
                    },
                )?;
                let output_value = plan.materialization(*output).ok_or(
                    HeterogeneousGpuContinuationError::InvalidPlan {
                        reason: "gpu_blend_output_materialization_missing",
                    },
                )?;
                require_plan(
                    base_value.completion() == waits[0]
                        && overlay_value.completion() == waits[1]
                        && output_value.completion() == *signal
                        && output_value.value() == *node,
                    "gpu_blend_value_or_token_mismatch",
                )?;
                require_plan(live.insert(*output), "gpu_blend_output_already_live")?;
                require_plan(completed.insert(*signal), "gpu_blend_signal_reused")?;
                nodes.push(*node);
            }
            PreparedHeterogeneousGpuStep::Release { materialization, after } => {
                require_plan(
                    *materialization != suffix.output_materialization(),
                    "gpu_output_released",
                )?;
                require_plan(completed.contains(after), "gpu_release_before_completion")?;
                require_plan(live.remove(materialization), "gpu_release_not_live")?;
            }
        }
    }
    require_plan(nodes == suffix.node_ids(), "gpu_schedule_node_order")?;
    require_plan(
        live == HashSet::from([suffix.output_materialization()]),
        "gpu_schedule_live_set_not_closed",
    )?;
    let output = plan.materialization(suffix.output_materialization()).ok_or(
        HeterogeneousGpuContinuationError::InvalidPlan {
            reason: "gpu_schedule_output_materialization_missing",
        },
    )?;
    require_plan(
        output.completion() == evidence.pending_output_token()
            && completed.contains(&evidence.pending_output_token()),
        "gpu_schedule_output_token",
    )
}

fn allocate_heterogeneous_gpu_batch_id(
) -> Result<HeterogeneousGpuBatchId, HeterogeneousGpuContinuationError> {
    NEXT_HETEROGENEOUS_GPU_BATCH_ID
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            current.checked_add(1)
        })
        .map(HeterogeneousGpuBatchId)
        .map_err(|_| HeterogeneousGpuContinuationError::BatchIdentityExhausted)
}

fn require_binding(
    condition: bool,
    field: &'static str,
) -> Result<(), HeterogeneousGpuContinuationError> {
    if condition {
        Ok(())
    } else {
        Err(HeterogeneousGpuContinuationError::BindingMismatch { field })
    }
}

fn require_plan(
    condition: bool,
    reason: &'static str,
) -> Result<(), HeterogeneousGpuContinuationError> {
    if condition {
        Ok(())
    } else {
        Err(HeterogeneousGpuContinuationError::InvalidPlan { reason })
    }
}

fn enforce_resource_grant(
    kind: HeterogeneousGpuResourceKind,
    required: u64,
    limit: u64,
) -> Result<(), HeterogeneousGpuContinuationError> {
    if required <= limit {
        Ok(())
    } else {
        Err(HeterogeneousGpuContinuationError::ResourceGrantExceeded { kind, required, limit })
    }
}

fn rgba_components_to_pixels(
    components: Vec<f32>,
) -> Result<Vec<[f32; 4]>, HeterogeneousGpuContinuationError> {
    let mut chunks = components.chunks_exact(4);
    let pixels = chunks
        .by_ref()
        .map(|rgba| [rgba[0], rgba[1], rgba[2], rgba[3]])
        .collect::<Vec<_>>();
    if chunks.remainder().is_empty() {
        Ok(pixels)
    } else {
        Err(
            HeterogeneousGpuContinuationError::InvalidReadbackComponents {
                components: components.len(),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::automation::{PropertyHost, PropertyMutation, PropertyValue};
    use mondrian_core::{
        effect_data::{EffectNode, EffectType},
        BlendMode, TimelineTime,
    };
    use mondrian_effects::{
        apply_compiled_effect_graph_rgba_f32, compile_reference_render_graph, CompiledEffectGraph,
        EffectExecutionSession, EffectExecutionSessionConfig, EffectGraphBuilderState,
        EffectNodeExt, EffectRenderOp, PreparedEffectProgram, PreparedHeterogeneousEffectWork,
    };

    const GENERATION: u64 = 17;
    const FRAME_SEED: i64 = 29;
    const EXTENT: EffectFrameExtent = EffectFrameExtent::new(4, 3);
    const WORKING_SPACE: WorkingColorSpace = WorkingColorSpace::LinearRec2020;

    fn generous_graph_budget() -> EffectGraphExecutionBudget {
        EffectGraphExecutionBudget::new(
            16 * 1024 * 1024,
            16 * 1024 * 1024,
            16 * 1024 * 1024,
            32,
            64,
        )
    }

    fn generous_gpu_grant() -> HeterogeneousGpuResourceGrant {
        HeterogeneousGpuResourceGrant::new(16 * 1024 * 1024, 16 * 1024 * 1024, 16 * 1024 * 1024)
    }

    fn gpu_test_deadline() -> Instant {
        Instant::now() + Duration::from_secs(30)
    }

    fn tracer_graph() -> Arc<CompiledEffectGraph> {
        let blur = EffectNode::with_defaults(EffectType::GaussianBlur);
        let mut correction = EffectNode::with_defaults(EffectType::BasicCorrection);
        correction
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::BasicCorrection.property_path("exposure"),
                value: PropertyValue::Float(0.25),
            })
            .expect("set Basic Correction exposure");
        let mut grain = EffectNode::with_defaults(EffectType::Grain);
        grain
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::Grain.property_path("amount"),
                value: PropertyValue::Float(0.1),
            })
            .expect("set Grain amount");
        let effects = [blur, correction, grain];
        PreparedEffectProgram::prepare(&effects, &[], WORKING_SPACE)
            .expect("prepare heterogeneous test program")
            .evaluate(TimelineTime::ZERO)
            .expect("compile heterogeneous test graph")
    }

    fn gpu_dag_graph() -> Arc<CompiledEffectGraph> {
        let mut builder = EffectGraphBuilderState::new();
        let source = builder.source();
        let blurred = builder.add_unary_from(source, EffectRenderOp::GaussianBlur { radius: 1.0 });
        let left = builder.add_unary_from(
            blurred,
            EffectRenderOp::ColorAdjust {
                exposure: 0.25,
                contrast: 1.0,
                saturation: 1.0,
                working_color_space: WORKING_SPACE,
            },
        );
        let right = builder.add_unary_from(blurred, EffectRenderOp::Grain { amount: 0.1 });
        let output = builder.add_blend(left, right, BlendMode::Normal, 0.4);
        builder.set_current_output(output);
        compile_reference_render_graph(builder.finish()).expect("compile GPU DAG graph")
    }

    fn test_input() -> Vec<[f32; 4]> {
        (0..EXTENT.width() * EXTENT.height())
            .map(|index| {
                let value = index as f32 / 11.0;
                [
                    -0.1 + value * 1.2,
                    1.1 - value * 0.8,
                    0.2 + value * 0.9,
                    if index == 0 { 0.0 } else { 1.0 },
                ]
            })
            .collect()
    }

    fn cpu_completion(
        graph: Arc<CompiledEffectGraph>,
        input: &[[f32; 4]],
    ) -> PreparedHeterogeneousCpuCompletion {
        let capability =
            HeterogeneousGpuExecutionCapability::scene_linear_f32().expect("renderer capability");
        let prepared = PreparedHeterogeneousEffectWork::prepare(
            graph,
            capability.environment(),
            capability.request(EXTENT, generous_graph_budget()),
        )
        .expect("prepare heterogeneous GPU work");
        assert_eq!(prepared.cpu_nodes().len(), 1);
        assert_eq!(prepared.gpu_suffix().node_ids().len(), 2);
        let mut session =
            EffectExecutionSession::new(EffectExecutionSessionConfig::uncached(8 * 1024 * 1024));
        session.bind_generation(GENERATION);
        prepared
            .execute_cpu_prefix_uncancelled(&session, GENERATION, input, FRAME_SEED, WORKING_SPACE)
            .expect("execute CPU prefix")
    }

    fn request(
        graph: &CompiledEffectGraph,
        grant: HeterogeneousGpuResourceGrant,
    ) -> HeterogeneousGpuContinuationRequest {
        HeterogeneousGpuContinuationRequest::new(
            HeterogeneousGpuContinuationBinding::new(
                graph.semantic_fingerprint(),
                GENERATION,
                EXTENT,
                FRAME_SEED,
                WORKING_SPACE,
            ),
            grant,
        )
    }

    #[test]
    fn capability_owns_exact_cpu_to_gpu_scene_linear_route() {
        let capability =
            HeterogeneousGpuExecutionCapability::scene_linear_f32().expect("renderer capability");
        let graph_request = capability.request(EXTENT, generous_graph_budget());
        assert_eq!(graph_request.frame_extent(), EXTENT);
        assert_eq!(
            graph_request.input_residency().format(),
            EffectValueFormat::new(
                EffectWorkingPrecision::Float32,
                EffectColorDomain::SceneLinearRgb,
            )
        );
        assert_eq!(
            graph_request.output_residency().format(),
            graph_request.input_residency().format()
        );
        assert_ne!(
            graph_request.input_residency().lane(),
            graph_request.output_residency().lane()
        );
    }

    #[test]
    fn heterogeneous_wait_control_is_cancellable_and_uses_one_deadline() {
        let now = Instant::now();
        assert!(matches!(
            heterogeneous_gpu_poll_timeout(true, now, now + Duration::from_secs(1)),
            Err(HeterogeneousGpuContinuationError::Canceled)
        ));
        assert!(matches!(
            heterogeneous_gpu_poll_timeout(false, now, now),
            Err(HeterogeneousGpuContinuationError::DeadlineExceeded)
        ));
        assert_eq!(
            heterogeneous_gpu_poll_timeout(
                false,
                now,
                now + HETEROGENEOUS_GPU_WAIT_SLICE + Duration::from_secs(1),
            )
            .expect("future deadline"),
            HETEROGENEOUS_GPU_WAIT_SLICE
        );
    }

    #[test]
    fn validation_rejects_stale_binding_and_undersized_grant_before_recording() {
        let graph = tracer_graph();
        let input = test_input();
        let completion = cpu_completion(Arc::clone(&graph), &input);
        let stale = HeterogeneousGpuContinuationRequest::new(
            HeterogeneousGpuContinuationBinding::new(
                graph.semantic_fingerprint(),
                GENERATION + 1,
                EXTENT,
                FRAME_SEED,
                WORKING_SPACE,
            ),
            generous_gpu_grant(),
        );
        assert!(matches!(
            validate_completion(stale, &completion),
            Err(HeterogeneousGpuContinuationError::BindingMismatch { field: "generation" })
        ));
        let wrong_working_space = HeterogeneousGpuContinuationRequest::new(
            HeterogeneousGpuContinuationBinding::new(
                graph.semantic_fingerprint(),
                GENERATION,
                EXTENT,
                FRAME_SEED,
                WorkingColorSpace::LinearRec709,
            ),
            generous_gpu_grant(),
        );
        assert!(matches!(
            validate_completion(wrong_working_space, &completion),
            Err(HeterogeneousGpuContinuationError::BindingMismatch {
                field: "working_color_space"
            })
        ));
        let tiny = request(&graph, HeterogeneousGpuResourceGrant::new(1, u64::MAX, 0));
        assert!(matches!(
            validate_completion(tiny, &completion),
            Err(HeterogeneousGpuContinuationError::ResourceGrantExceeded {
                kind: HeterogeneousGpuResourceKind::UploadBytes,
                ..
            })
        ));
    }

    #[test]
    fn gpu_dag_requires_the_adapters_non_aliasing_recording_residency() {
        let graph = gpu_dag_graph();
        let input = test_input();
        let capability =
            HeterogeneousGpuExecutionCapability::scene_linear_f32().expect("renderer capability");
        let prepared = PreparedHeterogeneousEffectWork::prepare(
            Arc::clone(&graph),
            capability.environment(),
            capability.request(EXTENT, generous_graph_budget()),
        )
        .expect("prepare GPU DAG continuation");
        let mut session =
            EffectExecutionSession::new(EffectExecutionSessionConfig::uncached(8 * 1024 * 1024));
        session.bind_generation(GENERATION);
        let completion = prepared
            .execute_cpu_prefix_uncancelled(&session, GENERATION, &input, FRAME_SEED, WORKING_SPACE)
            .expect("execute DAG CPU prefix");
        let abstract_peak = completion.execution_plan().peak_device_bytes();
        let recording_bytes =
            gpu_recording_device_bytes(completion.execution_plan(), completion.gpu_suffix())
                .expect("recording residency");
        assert!(recording_bytes > abstract_peak);
        let abstract_only =
            HeterogeneousGpuResourceGrant::new(16 * 1024 * 1024, abstract_peak, 16 * 1024 * 1024);
        assert!(matches!(
            validate_completion(request(&graph, abstract_only), &completion),
            Err(HeterogeneousGpuContinuationError::ResourceGrantExceeded {
                kind: HeterogeneousGpuResourceKind::DeviceBytes,
                required,
                limit,
            }) if required == recording_bytes && limit == abstract_peak
        ));
    }

    #[tokio::test]
    async fn real_wgpu_continuation_matches_complete_cpu_graph_and_proves_readback() {
        let Ok(context) = GpuContext::new().await else {
            eprintln!("skipping heterogeneous GPU continuation test: no GPU adapter available");
            return;
        };
        let graph = tracer_graph();
        let input = test_input();
        let expected = apply_compiled_effect_graph_rgba_f32(
            &input,
            EXTENT.width(),
            EXTENT.height(),
            &graph,
            FRAME_SEED,
        )
        .expect("complete CPU graph reference");
        let token_reference = cpu_completion(Arc::clone(&graph), &input);
        let expected_upload_token = token_reference.evidence().pending_gpu_input_token();
        let expected_output_token = token_reference.evidence().pending_output_token();
        let pool = Arc::new(GpuColorFrameWgpuResourcePool::new(
            GpuColorFrameWgpuResourcePoolOptions::default(),
        ));
        let mut runtime =
            HeterogeneousGpuContinuationRuntime::with_resource_pool(context, Arc::clone(&pool))
                .expect("heterogeneous GPU runtime");
        let cancellation = ExecutionCancellationToken::new();

        for attempt in 0..2 {
            let completion = cpu_completion(Arc::clone(&graph), &input);
            let completed = runtime
                .execute_to_cpu(
                    request(&graph, generous_gpu_grant()),
                    completion,
                    &cancellation,
                    gpu_test_deadline(),
                )
                .expect("execute and read back heterogeneous GPU continuation");

            assert_eq!(
                completed.evidence().completed_upload_token(),
                expected_upload_token
            );
            assert_eq!(
                completed.evidence().completed_output_token(),
                expected_output_token
            );
            assert!(completed.evidence().readback_bytes().is_some());
            assert_eq!(completed.frame().rgba_f32().data.len(), expected.len());
            for (actual, expected) in completed.frame().rgba_f32().data.iter().zip(expected.iter())
            {
                for channel in 0..4 {
                    assert!(
                        (actual[channel] - expected[channel]).abs() <= 2.0e-5,
                        "attempt {attempt} channel {channel}: actual={} expected={}",
                        actual[channel],
                        expected[channel]
                    );
                }
            }
        }
        let diagnostics = pool.diagnostics();
        assert!(
            diagnostics.hits >= 2,
            "the second frame must reuse both exact-contract textures: {diagnostics:?}"
        );
        assert!(!runtime.is_poisoned());
        assert!(runtime.table.is_empty());
    }

    #[tokio::test]
    async fn real_wgpu_gpu_dag_fanout_join_matches_scalar_reference() {
        let Ok(context) = GpuContext::new().await else {
            eprintln!("skipping heterogeneous GPU DAG test: no GPU adapter available");
            return;
        };
        let graph = gpu_dag_graph();
        let input = test_input();
        let expected = apply_compiled_effect_graph_rgba_f32(
            &input,
            EXTENT.width(),
            EXTENT.height(),
            &graph,
            FRAME_SEED,
        )
        .expect("complete CPU DAG reference");
        let capability =
            HeterogeneousGpuExecutionCapability::scene_linear_f32().expect("renderer capability");
        let prepared = PreparedHeterogeneousEffectWork::prepare(
            Arc::clone(&graph),
            capability.environment(),
            capability.request(EXTENT, generous_graph_budget()),
        )
        .expect("prepare GPU DAG continuation");
        assert_eq!(prepared.cpu_nodes().len(), 1);
        assert_eq!(prepared.gpu_suffix().node_ids().len(), 3);
        let mut session =
            EffectExecutionSession::new(EffectExecutionSessionConfig::uncached(8 * 1024 * 1024));
        session.bind_generation(GENERATION);
        let completion = prepared
            .execute_cpu_prefix_uncancelled(&session, GENERATION, &input, FRAME_SEED, WORKING_SPACE)
            .expect("execute DAG CPU prefix");
        let pool = Arc::new(GpuColorFrameWgpuResourcePool::new(
            GpuColorFrameWgpuResourcePoolOptions::default(),
        ));
        let mut runtime =
            HeterogeneousGpuContinuationRuntime::with_resource_pool(context, Arc::clone(&pool))
                .expect("heterogeneous GPU runtime");
        let completed = runtime
            .execute_to_cpu(
                request(&graph, generous_gpu_grant()),
                completion,
                &ExecutionCancellationToken::new(),
                gpu_test_deadline(),
            )
            .expect("execute GPU DAG and read back");
        assert!(
            completed.evidence().recorded().recorded_device_bytes()
                > completed.evidence().recorded().peak_device_bytes(),
            "the one-submission Adapter grant must expose its conservative non-aliasing residency"
        );
        for (actual, expected) in completed.frame().rgba_f32().data.iter().zip(expected.iter()) {
            for channel in 0..4 {
                assert!(
                    (actual[channel] - expected[channel]).abs() <= 2.0e-5,
                    "channel {channel}: actual={} expected={}",
                    actual[channel],
                    expected[channel]
                );
            }
        }
        assert!(!runtime.is_poisoned());
        assert!(runtime.table.is_empty());
    }

    #[tokio::test]
    async fn repeated_pre_submit_readback_rejection_leaves_table_empty_and_pool_bounded() {
        let Ok(context) = GpuContext::new().await else {
            eprintln!("skipping heterogeneous GPU rollback test: no GPU adapter available");
            return;
        };
        let pool = Arc::new(GpuColorFrameWgpuResourcePool::new(
            GpuColorFrameWgpuResourcePoolOptions::default(),
        ));
        let mut runtime =
            HeterogeneousGpuContinuationRuntime::with_resource_pool(context, Arc::clone(&pool))
                .expect("heterogeneous GPU runtime");
        let graph = tracer_graph();
        let input = test_input();
        let no_readback = HeterogeneousGpuResourceGrant::new(16 * 1024 * 1024, 16 * 1024 * 1024, 0);
        let cancellation = ExecutionCancellationToken::new();

        let first = match runtime.execute_to_cpu(
            request(&graph, no_readback),
            cpu_completion(Arc::clone(&graph), &input),
            &cancellation,
            gpu_test_deadline(),
        ) {
            Ok(_) => panic!("readback grant must fail before submission"),
            Err(error) => error,
        };
        assert!(matches!(
            first,
            HeterogeneousGpuContinuationError::ResourceGrantExceeded {
                kind: HeterogeneousGpuResourceKind::ReadbackBytes,
                ..
            }
        ));
        assert!(!runtime.is_poisoned());
        assert!(runtime.table.is_empty());
        let after_first = pool.diagnostics();

        let second = match runtime.execute_to_cpu(
            request(&graph, no_readback),
            cpu_completion(Arc::clone(&graph), &input),
            &cancellation,
            gpu_test_deadline(),
        ) {
            Ok(_) => panic!("repeated readback grant must fail before submission"),
            Err(error) => error,
        };
        assert!(matches!(
            second,
            HeterogeneousGpuContinuationError::ResourceGrantExceeded {
                kind: HeterogeneousGpuResourceKind::ReadbackBytes,
                ..
            }
        ));
        assert!(!runtime.is_poisoned());
        assert!(runtime.table.is_empty());
        let after_second = pool.diagnostics();
        assert_eq!(
            after_second.retained_resources,
            after_first.retained_resources
        );
        assert_eq!(after_second.retained_bytes, after_first.retained_bytes);
    }
}
