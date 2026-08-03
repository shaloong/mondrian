//! Exact finite temporal sampling and ROI execution over the one compiled Effect IR.
//!
//! This Module is the budget-aware scalar semantic reference for CPU execution
//! and future SIMD/GPU adapters. It consumes [`crate::CompiledEffectGraph`]
//! directly; it does not introduce a second graph or reinterpret definition
//! contracts.

mod program;
mod schedule;

use crate::adjustment::{
    apply_render_op_f32_region_controlled, render_op_f32_scratch_frames, EffectRasterRegion,
};
use crate::execution::{apply_alpha_mask_f32_region_controlled, blend_rgba_f32_region_controlled};
use crate::execution_session::EffectTemporalCachedOutput;
use crate::mask_raster::PreparedMaskRasterSet;
use crate::{
    CompiledEffectGraph, EffectExecutionDemand, EffectExecutionDemandError, EffectExecutionSession,
    EffectFrameExtent, EffectGraphNodeId, EffectGraphNodeKind, EffectInputRoi, EffectPixelRoi,
    EffectProcessingBackend, EffectResourceLifetime, EffectRoiHalo, EffectStateModel,
    EffectTemporalBoundary, EffectTemporalSpan, EffectWorkingPrecision, PreparedEffectProgram,
};
use mondrian_core::{ExecutionCancellationToken, TimelineTime};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use program::{prepare_temporal_value_program, PreparedTemporalValueProgram, TemporalValueAddress};
use schedule::{plan_temporal_tiles, temporal_scalar_required_bytes};

type TemporalSampleEvaluation = Result<(Arc<CompiledEffectGraph>, i64), String>;
type TemporalSampleEvaluator<'a> = &'a mut dyn FnMut(TimelineTime) -> TemporalSampleEvaluation;

/// Complete immutable identity of the source adapter and every time/pixel
/// mapping it exposes to one Effect program.
///
/// The fingerprint must cover the source revision, physical stream,
/// Clip-to-source or nested-Sequence mapping, color/alpha interpretation,
/// geometry, and frame-seed grid. A path, durable entity ID, or truncated hash
/// is not sufficient.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EffectTemporalSourceIdentity([u8; 32]);

impl EffectTemporalSourceIdentity {
    /// Bind one complete canonical semantic fingerprint.
    pub const fn from_complete_semantic_fingerprint(fingerprint: [u8; 32]) -> Self {
        Self(fingerprint)
    }

    /// Complete provider identity.
    pub const fn semantic_fingerprint(self) -> [u8; 32] {
        self.0
    }
}

/// Whether the request continues ordinary evaluation or follows a scheduler
/// discontinuity.
///
/// This fact is evidence, not a processor-owned rule key. Finite temporal
/// stateless execution is random-access in either case. Stateful contracts
/// remain fail-closed until an ordered Effect continuity Session exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EffectExecutionContinuity {
    /// Ordinary evaluation in the same scheduler run.
    Continuous,
    /// Seek, source switch, graph change, or other discontinuous re-entry.
    Discontinuous,
}

/// One exact temporal/ROI output request.
#[derive(Debug, Clone)]
pub struct EffectTemporalExecutionRequest {
    generation: u64,
    continuity: EffectExecutionContinuity,
    output_time: TimelineTime,
    output_frame_seed: i64,
    frame_extent: EffectFrameExtent,
    output_roi: EffectPixelRoi,
    cancellation: ExecutionCancellationToken,
}

impl EffectTemporalExecutionRequest {
    /// Construct a request bound to one immutable scheduler generation.
    pub fn new(
        generation: u64,
        continuity: EffectExecutionContinuity,
        output_time: TimelineTime,
        frame_extent: EffectFrameExtent,
        output_roi: EffectPixelRoi,
        cancellation: ExecutionCancellationToken,
    ) -> Self {
        Self {
            generation,
            continuity,
            output_time,
            output_frame_seed: frame_seed_for_output(output_time),
            frame_extent,
            output_roi,
            cancellation,
        }
    }

    /// Bind the exact deterministic seed used by the current-time Effect DAG.
    ///
    /// Timeline execution supplies the same seed carried by its ordinary
    /// single-frame render plan. Source tiles cannot infer this value from a
    /// retimed Clip coordinate.
    pub const fn with_output_frame_seed(mut self, output_frame_seed: i64) -> Self {
        self.output_frame_seed = output_frame_seed;
        self
    }

    /// Scheduler generation that owns every provider request and cache entry.
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Continuity evidence supplied by the scheduler.
    pub const fn continuity(&self) -> EffectExecutionContinuity {
        self.continuity
    }

    /// Exact Clip visual-author-domain output time.
    pub const fn output_time(&self) -> TimelineTime {
        self.output_time
    }

    /// Deterministic seed for Effects evaluated at `output_time`.
    pub const fn output_frame_seed(&self) -> i64 {
        self.output_frame_seed
    }

    /// Complete frame coordinate extent.
    pub const fn frame_extent(&self) -> EffectFrameExtent {
        self.frame_extent
    }

    /// Requested half-open output region.
    pub const fn output_roi(&self) -> EffectPixelRoi {
        self.output_roi
    }

    /// Monotonic cancellation authority for this generation.
    pub const fn cancellation(&self) -> &ExecutionCancellationToken {
        &self.cancellation
    }
}

/// One source request emitted by the scalar temporal executor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EffectTemporalFrameRequest {
    generation: u64,
    time: TimelineTime,
    frame_extent: EffectFrameExtent,
    input_roi: EffectInputRoi,
    exact_halo: Option<EffectRoiHalo>,
    precision: EffectWorkingPrecision,
}

/// Immutable, exact source-demand set collected from one compiled Effect graph.
///
/// This is the boundary between low-frequency graph evaluation and concrete
/// media work. Preview may schedule every request asynchronously; Export may
/// resolve the same requests synchronously inside its job. Neither consumer is
/// allowed to ask the scalar executor to decode while it is walking the graph.
#[derive(Debug, Clone)]
pub struct EffectTemporalFrameDemandBatch {
    generation: u64,
    requests: Arc<[EffectTemporalFrameRequest]>,
    coverage_bytes: usize,
}

impl EffectTemporalFrameDemandBatch {
    /// Scheduler generation that owns every request in this frozen batch.
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Exact, de-duplicated requests in deterministic evaluation order.
    pub fn requests(&self) -> &[EffectTemporalFrameRequest] {
        &self.requests
    }

    /// Whether this batch has no pixel dependency.
    pub fn is_empty(&self) -> bool {
        self.requests.is_empty()
    }

    /// Exact Float32 source-coverage bytes that must be frozen before
    /// execution. Callers use this for resource admission before materializing
    /// any tile.
    pub const fn coverage_bytes(&self) -> usize {
        self.coverage_bytes
    }
}

/// Immutable time-expanded temporal execution prepared from one exact Effect
/// stack and all exact-time graph evaluations it reaches.
///
/// This value is the only authority shared by demand collection and pixel
/// execution. Consumers cannot accidentally freeze dependencies from one
/// graph projection and execute another.
#[derive(Debug, Clone)]
pub struct PreparedEffectTemporalExecution {
    graph: Arc<CompiledEffectGraph>,
    request: EffectTemporalExecutionRequest,
    program: Arc<PreparedTemporalValueProgram>,
    demands: EffectTemporalFrameDemandBatch,
}

impl PreparedEffectTemporalExecution {
    /// Root compiled Effect IR at the requested output instant.
    pub const fn graph(&self) -> &Arc<CompiledEffectGraph> {
        &self.graph
    }

    /// Exact scheduler-owned execution request.
    pub const fn request(&self) -> &EffectTemporalExecutionRequest {
        &self.request
    }

    /// Exact raw-source dependencies of the complete expanded execution.
    pub const fn demands(&self) -> &EffectTemporalFrameDemandBatch {
        &self.demands
    }
}

impl EffectTemporalFrameRequest {
    /// Owning scheduler generation.
    pub const fn generation(self) -> u64 {
        self.generation
    }

    /// Exact Clip-domain source instant requested by the graph.
    pub const fn time(self) -> TimelineTime {
        self.time
    }

    /// Complete coordinate extent shared by every tile.
    pub const fn frame_extent(self) -> EffectFrameExtent {
        self.frame_extent
    }

    /// Concrete required source region and strength of its ROI evidence.
    pub const fn input_roi(self) -> EffectInputRoi {
        self.input_roi
    }

    /// Exact finite halo, or `None` for an unknown conservative full-frame
    /// request.
    pub const fn exact_halo(self) -> Option<EffectRoiHalo> {
        self.exact_halo
    }

    /// Exact requested working representation.
    pub const fn precision(self) -> EffectWorkingPrecision {
        self.precision
    }
}

/// Immutable contiguous Float32 coverage or output tile.
#[derive(Debug, Clone)]
pub struct EffectFrameTileF32 {
    time: TimelineTime,
    frame_extent: EffectFrameExtent,
    roi: EffectPixelRoi,
    frame_seed: i64,
    pixels: Arc<Vec<[f32; 4]>>,
}

impl EffectFrameTileF32 {
    /// Validate and retain one exact source tile.
    pub fn new(
        time: TimelineTime,
        frame_extent: EffectFrameExtent,
        roi: EffectPixelRoi,
        frame_seed: i64,
        pixels: Vec<[f32; 4]>,
    ) -> Result<Self, EffectTemporalFrameProviderError> {
        Self::from_shared(time, frame_extent, roi, frame_seed, Arc::new(pixels))
    }

    fn from_shared(
        time: TimelineTime,
        frame_extent: EffectFrameExtent,
        roi: EffectPixelRoi,
        frame_seed: i64,
        pixels: Arc<Vec<[f32; 4]>>,
    ) -> Result<Self, EffectTemporalFrameProviderError> {
        let expected =
            checked_pixel_count(roi).ok_or(EffectTemporalFrameProviderError::InvalidTile {
                reason: "tile pixel count overflowed addressable memory".to_owned(),
            })?;
        if pixels.len() != expected {
            return Err(EffectTemporalFrameProviderError::InvalidTile {
                reason: format!(
                    "tile contains {} pixels but ROI requires {expected}",
                    pixels.len()
                ),
            });
        }
        if roi != clamp_roi(roi, frame_extent) {
            return Err(EffectTemporalFrameProviderError::InvalidTile {
                reason: "tile ROI exceeds the complete frame extent".to_owned(),
            });
        }
        Ok(Self { time, frame_extent, roi, frame_seed, pixels })
    }

    /// Exact source time.
    pub const fn time(&self) -> TimelineTime {
        self.time
    }

    /// Complete source coordinate extent.
    pub const fn frame_extent(&self) -> EffectFrameExtent {
        self.frame_extent
    }

    /// Half-open source region represented by `pixels`.
    pub const fn roi(&self) -> EffectPixelRoi {
        self.roi
    }

    /// Deterministic frame seed for this exact evaluation instant.
    pub const fn frame_seed(&self) -> i64 {
        self.frame_seed
    }

    /// Straight-alpha scene-linear Float32 pixels in row-major order.
    pub fn pixels(&self) -> &[[f32; 4]] {
        self.pixels.as_slice()
    }
}

/// Failure returned by a concrete temporal source Adapter.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EffectTemporalFrameProviderError {
    /// The owning execution generation was canceled.
    #[error("effect temporal frame request was canceled")]
    Canceled,
    /// Source decode, nested evaluation, or another recoverable dependency
    /// could not provide the exact request.
    #[error("effect temporal frame provider failed: {reason}")]
    Unavailable {
        /// Adapter-owned failure reason.
        reason: String,
    },
    /// The Adapter exposed malformed coverage or copied an invalid region.
    #[error("effect temporal frame provider produced invalid coverage: {reason}")]
    InvalidTile {
        /// Exact contract violation.
        reason: String,
    },
}

/// Adapter that copies prepared Clip-domain source or nested-Sequence pixels
/// into an executor-owned destination.
///
/// The executor reserves the exact destination capacity before crossing this
/// seam. Implementations must not perform decode, color conversion, nested
/// evaluation, or hidden allocation here; production uses an immutable
/// [`PreparedTemporalFrameSet`] assembled before pixel execution.
pub trait EffectTemporalFrameProvider {
    /// Complete immutable source/mapping identity used by cache admission.
    fn source_identity(&self) -> EffectTemporalSourceIdentity;

    /// Exact immutable source-coverage bytes retained for this execution.
    fn retained_coverage_bytes(&self) -> usize;

    /// Copy exactly the requested region and Float32 representation.
    ///
    /// `destination` is empty and already has capacity for the exact request.
    /// Success requires appending exactly that many row-major pixels.
    /// Implementations must observe `cancellation` at bounded checkpoints. A
    /// canceled result may not populate a decode or Effect cache.
    fn copy_frame(
        &mut self,
        request: EffectTemporalFrameRequest,
        cancellation: &ExecutionCancellationToken,
        destination: &mut Vec<[f32; 4]>,
    ) -> Result<(), EffectTemporalFrameProviderError>;
}

/// A fully resolved immutable temporal batch.
///
/// Construction proves that each graph demand has exactly one matching
/// coverage tile and that no unrequested tile entered the set. `copy_frame`
/// may copy any exact subregion covered by one frozen tile, enabling
/// budget-driven execution tiling without asking Preview or Export to decode
/// the same source again. It performs no decode, nested evaluation, title
/// rasterization, color conversion, or allocation.
#[derive(Debug, Clone)]
pub struct PreparedTemporalFrameSet {
    source_identity: EffectTemporalSourceIdentity,
    generation: u64,
    requests: Arc<[EffectTemporalFrameRequest]>,
    frames: HashMap<EffectTemporalFrameRequest, EffectFrameTileF32>,
    retained_coverage_bytes: usize,
}

impl PreparedTemporalFrameSet {
    /// Freeze one completely resolved demand batch.
    pub fn prepare(
        source_identity: EffectTemporalSourceIdentity,
        batch: EffectTemporalFrameDemandBatch,
        resolved: impl IntoIterator<Item = (EffectTemporalFrameRequest, EffectFrameTileF32)>,
    ) -> Result<Self, PreparedTemporalFrameSetError> {
        let expected = batch.requests.iter().copied().collect::<HashSet<_>>();
        let mut frames = HashMap::with_capacity(expected.len());
        for (request, tile) in resolved {
            if request.generation != batch.generation {
                return Err(PreparedTemporalFrameSetError::GenerationMismatch {
                    expected: batch.generation,
                    actual: request.generation,
                });
            }
            if !expected.contains(&request) {
                return Err(PreparedTemporalFrameSetError::UnexpectedRequest { request });
            }
            validate_provider_tile(&tile, request).map_err(|error| {
                PreparedTemporalFrameSetError::InvalidTile { request, reason: error.to_string() }
            })?;
            if frames.insert(request, tile).is_some() {
                return Err(PreparedTemporalFrameSetError::DuplicateRequest { request });
            }
        }
        if let Some(request) =
            batch.requests.iter().copied().find(|request| !frames.contains_key(request))
        {
            return Err(PreparedTemporalFrameSetError::MissingRequest { request });
        }
        Ok(Self {
            source_identity,
            generation: batch.generation,
            requests: batch.requests,
            frames,
            retained_coverage_bytes: batch.coverage_bytes,
        })
    }

    /// Scheduler generation bound to this immutable set.
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Exact requests proven complete by construction.
    pub fn requests(&self) -> &[EffectTemporalFrameRequest] {
        &self.requests
    }

    /// Number of retained source tiles.
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// Whether no source tile is retained.
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Exact immutable Float32 coverage retained by this set.
    pub const fn retained_coverage_bytes(&self) -> usize {
        self.retained_coverage_bytes
    }
}

impl EffectTemporalFrameProvider for PreparedTemporalFrameSet {
    fn source_identity(&self) -> EffectTemporalSourceIdentity {
        self.source_identity
    }

    fn retained_coverage_bytes(&self) -> usize {
        self.retained_coverage_bytes()
    }

    fn copy_frame(
        &mut self,
        request: EffectTemporalFrameRequest,
        cancellation: &ExecutionCancellationToken,
        destination: &mut Vec<[f32; 4]>,
    ) -> Result<(), EffectTemporalFrameProviderError> {
        if cancellation.is_canceled() {
            return Err(EffectTemporalFrameProviderError::Canceled);
        }
        if request.generation != self.generation {
            return Err(EffectTemporalFrameProviderError::Unavailable {
                reason: format!(
                    "prepared temporal set belongs to generation {}, not {}",
                    self.generation, request.generation
                ),
            });
        }
        if !destination.is_empty() {
            return Err(EffectTemporalFrameProviderError::InvalidTile {
                reason: "temporal destination was not empty at the provider seam".to_owned(),
            });
        }
        let requested_roi = request.input_roi.region();
        let mut covering_frames = self.frames.iter().filter(|(prepared, tile)| {
            prepared.generation == request.generation
                && prepared.time == request.time
                && prepared.frame_extent == request.frame_extent
                && prepared.precision == request.precision
                && roi_contains(tile.roi, requested_roi)
        });
        let (_, covering) = covering_frames.next().ok_or_else(|| {
            EffectTemporalFrameProviderError::Unavailable {
                reason: format!(
                    "prepared temporal set does not cover request at {} / {} with ROI {:?}",
                    request.time.numerator(),
                    request.time.denominator(),
                    requested_roi
                ),
            }
        })?;
        if covering_frames.next().is_some() {
            return Err(EffectTemporalFrameProviderError::InvalidTile {
                reason: "prepared temporal set contains ambiguous overlapping coverage".to_owned(),
            });
        }
        copy_tile_region(covering, requested_roi, cancellation, destination)
    }
}

/// Invalid or incomplete input while freezing a temporal frame set.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PreparedTemporalFrameSetError {
    /// A resolved request belongs to another scheduler generation.
    #[error("temporal frame generation mismatch: expected {expected}, received {actual}")]
    GenerationMismatch {
        /// Batch generation.
        expected: u64,
        /// Resolved request generation.
        actual: u64,
    },
    /// The adapter returned a request the compiled graph never demanded.
    #[error("temporal frame set contains an unrequested dependency: {request:?}")]
    UnexpectedRequest {
        /// Unexpected exact request.
        request: EffectTemporalFrameRequest,
    },
    /// The adapter returned the same exact request more than once.
    #[error("temporal frame set contains duplicate dependency: {request:?}")]
    DuplicateRequest {
        /// Duplicated exact request.
        request: EffectTemporalFrameRequest,
    },
    /// A graph demand was not resolved.
    #[error("temporal frame set is missing dependency: {request:?}")]
    MissingRequest {
        /// Missing exact request.
        request: EffectTemporalFrameRequest,
    },
    /// A resolved tile did not exactly satisfy its request.
    #[error("invalid tile for temporal request {request:?}: {reason}")]
    InvalidTile {
        /// Request the tile claimed to satisfy.
        request: EffectTemporalFrameRequest,
        /// Exact validation failure.
        reason: String,
    },
}

/// One completed exact ROI execution.
#[derive(Debug, Clone)]
pub struct EffectTemporalExecutionOutput {
    tile: EffectFrameTileF32,
    cache_identity: [u8; 32],
    provider_requests: usize,
    execution_tiles: usize,
    peak_working_bytes: usize,
}

impl EffectTemporalExecutionOutput {
    /// Requested output tile.
    pub const fn tile(&self) -> &EffectFrameTileF32 {
        &self.tile
    }

    /// Complete versioned identity used for Session-local cache lookup.
    pub const fn cache_identity(&self) -> [u8; 32] {
        self.cache_identity
    }

    /// Number of exact provider copies performed (zero on a cache hit).
    pub const fn provider_requests(&self) -> usize {
        self.provider_requests
    }

    /// Number of scalar output tiles actually executed (zero on a cache hit or
    /// empty output).
    pub const fn execution_tiles(&self) -> usize {
        self.execution_tiles
    }

    /// Peak scalar working bytes admitted for this execution.
    pub const fn peak_working_bytes(&self) -> usize {
        self.peak_working_bytes
    }
}

/// Why exact temporal/ROI scalar execution failed closed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EffectTemporalExecutionError {
    /// Demand planning could not represent the exact request.
    #[error(transparent)]
    Demand(#[from] EffectExecutionDemandError),
    /// One definition stage does not admit CPU Float32.
    #[error("effect stage {stage_index} does not admit CPU Float32 temporal execution")]
    ExecutionModeNotAdmitted {
        /// First incompatible definition stage.
        stage_index: usize,
    },
    /// Ordered mutable state requires a future continuity Session.
    #[error("stateful effect execution requires an ordered continuity session")]
    StatefulUnsupported,
    /// Continuity-owned resources cannot enter a random-access execution.
    #[error("effect execution requires continuity-session resource ownership")]
    ContinuityResourceUnsupported,
    /// Unbounded temporal input cannot be materialized by a bounded request.
    #[error("effect execution has unbounded temporal input")]
    UnboundedTemporalInput,
    /// A temporal graph did not match the exact bounded production shape:
    /// Source-fed finite signed taps plus a current-time DAG.
    #[error("temporal graph is outside the admitted production tracer: {reason}")]
    UnsupportedTemporalShape {
        /// Stable fail-closed explanation.
        reason: &'static str,
    },
    /// Evaluating the same prepared Effect stack at an exact sample instant
    /// failed before any pixels were requested.
    #[error("effect graph evaluation at temporal sample {time:?} failed: {reason}")]
    SampleGraphEvaluation {
        /// Exact Clip-domain sample instant.
        time: TimelineTime,
        /// Prepared-program evaluation failure.
        reason: String,
    },
    /// Definition-stage execution contracts changed between exact sample
    /// instants, invalidating stable cross-time stage addressing.
    #[error(
        "effect stage {stage_index} changed its temporal execution contract across sample times"
    )]
    SampledStageContractChanged {
        /// First changed or missing Definition stage.
        stage_index: usize,
    },
    /// The finite time-expanded projection exceeded a hard operational cap.
    #[error("effect temporal program exceeds the {limit} {kind} hard limit")]
    TemporalProgramLimitExceeded {
        /// Stable projection dimension.
        kind: &'static str,
        /// Maximum admitted count.
        limit: usize,
    },
    /// A sampled exact-time graph requires source pixels outside the root
    /// request's already admitted conservative coverage.
    #[error("sampled effect graph at {time:?} requires source coverage outside the root temporal request")]
    SampledSpatialDemandEscaped {
        /// Exact Clip-domain sample instant.
        time: TimelineTime,
    },
    /// Mixed RGB processing domains require a renderer-owned temporal color
    /// Adapter that is not yet provided by this scalar reference.
    #[error("temporal scalar execution requires unresolved color-domain processing")]
    ColorDomainUnsupported,
    /// A graph node has no exact lowering in the bounded scalar executor.
    #[error("temporal scalar execution does not support graph node {node_id:?} ({kind})")]
    UnsupportedGraphNode {
        /// Unsupported compiled node.
        node_id: EffectGraphNodeId,
        /// Stable node-kind label.
        kind: &'static str,
    },
    /// A non-temporal unary operation lacks a Float32 scalar implementation.
    #[error("temporal scalar execution does not support render operation `{op}`")]
    UnsupportedRenderOperation {
        /// Stable operation label.
        op: &'static str,
    },
    /// A finite temporal operation carried invalid runtime values.
    #[error("invalid temporal frame blend: {reason}")]
    InvalidTemporalOperation {
        /// Exact invalid value or arithmetic reason.
        reason: &'static str,
    },
    /// A concrete source or nested Sequence Adapter failed.
    #[error(transparent)]
    Provider(#[from] EffectTemporalFrameProviderError),
    /// Execution was canceled at a cooperative checkpoint.
    #[error("effect temporal execution was canceled")]
    Canceled,
    /// Tile-local scalar staging, live graph values, kernel scratch, or output
    /// publication exceeded the explicit Session budget before allocation.
    #[error(
        "effect temporal scalar working set requires {required_bytes} bytes, exceeding the {budget_bytes}-byte budget"
    )]
    WorkingSetBudgetExceeded {
        /// Bytes required at the rejected checkpoint.
        required_bytes: usize,
        /// Session-owned limit.
        budget_bytes: usize,
    },
    /// The exact frozen source coverage could not be represented in the
    /// process address space.
    #[error("effect temporal source coverage byte size overflowed")]
    SourceCoverageSizeOverflow,
    /// Prepared Mask geometry or region rasterization failed.
    #[error("effect temporal Mask raster failed: {source}")]
    MaskRasterFailed {
        /// Typed Mask preparation/raster reason.
        #[source]
        source: crate::MaskRasterError,
    },
    /// Deterministic tiling would create an operationally unsafe amount of
    /// scheduler metadata and per-tile overhead.
    #[error("effect temporal scalar schedule exceeds the {limit}-tile hard limit")]
    TileScheduleLimitExceeded {
        /// Maximum tiles admitted for one output request.
        limit: usize,
    },
    /// A planned output ROI was not contained by its exact input region.
    #[error("effect temporal ROI projection is invalid: {reason}")]
    InvalidRoiProjection {
        /// Stable internal contract violation.
        reason: &'static str,
    },
    /// The compiled graph did not produce its declared output.
    #[error("effect temporal graph output is missing")]
    MissingGraphOutput,
    /// A scheduled graph input was not live when its consumer executed.
    #[error("effect temporal graph value {node_id:?} is missing")]
    MissingGraphValue {
        /// Missing compiled graph value.
        node_id: EffectGraphNodeId,
    },
    /// The compiled graph's use-count evidence did not match its schedule.
    #[error("effect temporal graph liveness is invalid: {reason}")]
    InvalidGraphLiveness {
        /// Stable internal contract violation.
        reason: &'static str,
    },
    /// The executor's owned-frame ledger diverged from compiled liveness.
    #[error(
        "effect temporal working-set ledger retained {actual_bytes} bytes; expected {expected_bytes}"
    )]
    WorkingSetLedgerMismatch {
        /// Bytes required by the execution state.
        expected_bytes: usize,
        /// Bytes recorded by the ledger.
        actual_bytes: usize,
    },
}

const MAX_TEMPORAL_SCALAR_TILES: usize = 4_096;

/// Collect every source frame required by the bounded finite temporal production
/// tracer without fetching any pixels.
///
/// The admitted graph is deliberately mathematically closed: every
/// `TemporalFrameBlend` reads `Source` directly, while current-time unary
/// branches may fan out and rejoin through Blend or MultiInput nodes. Repeated
/// exact sample times are de-duplicated before the provider sees them. A
/// temporal operation with upstream Effects would require those Effects to be
/// evaluated at each history time, while frame-bound parameters are currently
/// compiled at the output Clip time. Such graphs therefore fail closed instead
/// of silently applying current parameters to historical frames. Current-time
/// Mask nodes are admitted through immutable geometry prepared once for the
/// complete frame extent and rasterized over the exact requested region.
pub fn collect_temporal_frame_demands(
    compiled: &CompiledEffectGraph,
    request: &EffectTemporalExecutionRequest,
) -> Result<EffectTemporalFrameDemandBatch, EffectTemporalExecutionError> {
    let prepared = prepare_temporal_execution_inner(Arc::new(compiled.clone()), request, None)?;
    ensure_temporal_program(&prepared.program)?;
    Ok(prepared.demands)
}

/// Prepare one exact finite temporal execution, reevaluating the same prepared
/// Effect stack whenever a temporal stage samples an effected upstream value.
///
/// This function owns the root and every sampled graph evaluation, so a caller
/// cannot accidentally combine dependencies from different Effect programs.
/// The callback supplies only the deterministic frame seed for each exact
/// Clip-domain sample instant. Stage contracts remain stable authoring
/// obligations; a sampled evaluation that changes them fails closed. Animated
/// parameters and dynamic topology may otherwise produce a distinct compiled
/// graph at every sample time.
pub fn prepare_temporal_frame_execution(
    program: &PreparedEffectProgram,
    request: &EffectTemporalExecutionRequest,
    mut sample_frame_seed: impl FnMut(TimelineTime) -> Result<i64, String>,
) -> Result<PreparedEffectTemporalExecution, EffectTemporalExecutionError> {
    prepare_temporal_frame_execution_with_evaluator(
        request,
        |time| program.evaluate(time).map_err(|error| error.to_string()),
        &mut sample_frame_seed,
    )
}

fn prepare_temporal_frame_execution_with_evaluator(
    request: &EffectTemporalExecutionRequest,
    mut evaluate_graph: impl FnMut(TimelineTime) -> Result<Arc<CompiledEffectGraph>, String>,
    sample_frame_seed: &mut impl FnMut(TimelineTime) -> Result<i64, String>,
) -> Result<PreparedEffectTemporalExecution, EffectTemporalExecutionError> {
    let compiled = evaluate_graph(request.output_time).map_err(|reason| {
        EffectTemporalExecutionError::SampleGraphEvaluation { time: request.output_time, reason }
    })?;
    let mut evaluate_sample = |time| {
        let graph = evaluate_graph(time)?;
        let frame_seed = sample_frame_seed(time)?;
        Ok((graph, frame_seed))
    };
    let prepared = prepare_temporal_execution_inner(compiled, request, Some(&mut evaluate_sample))?;
    ensure_temporal_program(&prepared.program)?;
    Ok(prepared)
}

fn prepare_temporal_execution_inner(
    compiled: Arc<CompiledEffectGraph>,
    request: &EffectTemporalExecutionRequest,
    evaluate_sample: Option<TemporalSampleEvaluator<'_>>,
) -> Result<PreparedEffectTemporalExecution, EffectTemporalExecutionError> {
    if request.cancellation.is_canceled() {
        return Err(EffectTemporalExecutionError::Canceled);
    }
    admit_temporal_scalar(&compiled)?;
    if compiled.domain_plan().requires_conversion() || !compiled.domain_plan().blockers.is_empty() {
        return Err(EffectTemporalExecutionError::ColorDomainUnsupported);
    }
    let program = prepare_temporal_value_program(Arc::clone(&compiled), request, evaluate_sample)?;
    let demand = compiled.plan_execution_demand(
        request.output_time,
        request.frame_extent,
        request.output_roi,
    )?;
    ensure_bounded_temporal_window(demand.temporal_window())?;
    let (earliest, latest) = finite_temporal_bounds(demand.temporal_window())?;
    for context in program.contexts() {
        let sampled = context.graph.plan_execution_demand(
            context.time,
            request.frame_extent,
            request.output_roi,
        )?;
        ensure_bounded_temporal_window(sampled.temporal_window())?;
        if !roi_contains(demand.input_roi().region(), sampled.input_roi().region()) {
            return Err(EffectTemporalExecutionError::SampledSpatialDemandEscaped {
                time: context.time,
            });
        }
    }
    let requests = program
        .source_times()
        .iter()
        .copied()
        .map(|time| {
            if time < earliest || time > latest {
                return Err(EffectTemporalExecutionError::InvalidTemporalOperation {
                    reason: "time-expanded source sample exceeds the declared temporal extent",
                });
            }
            Ok(EffectTemporalFrameRequest {
                generation: request.generation,
                time,
                frame_extent: demand.frame_extent(),
                input_roi: demand.input_roi(),
                exact_halo: demand.exact_halo(),
                precision: EffectWorkingPrecision::Float32,
            })
        })
        .collect::<Result<Vec<_>, EffectTemporalExecutionError>>()?;
    let coverage_bytes = requests.iter().try_fold(0usize, |total, request| {
        checked_pixel_count(request.input_roi.region())
            .and_then(|pixels| pixels.checked_mul(std::mem::size_of::<[f32; 4]>()))
            .and_then(|bytes| total.checked_add(bytes))
            .ok_or(EffectTemporalExecutionError::SourceCoverageSizeOverflow)
    })?;
    let demands = EffectTemporalFrameDemandBatch {
        generation: request.generation,
        requests: requests.into(),
        coverage_bytes,
    };
    Ok(PreparedEffectTemporalExecution {
        graph: compiled,
        request: request.clone(),
        program: Arc::new(program),
        demands,
    })
}

fn ensure_temporal_program(
    program: &PreparedTemporalValueProgram,
) -> Result<(), EffectTemporalExecutionError> {
    if program.temporal_nodes() == 0 {
        Err(EffectTemporalExecutionError::UnsupportedTemporalShape {
            reason: "graph has no finite temporal operation",
        })
    } else {
        Ok(())
    }
}

fn finite_temporal_bounds(
    window: crate::EffectTemporalWindow,
) -> Result<(TimelineTime, TimelineTime), EffectTemporalExecutionError> {
    match (window.earliest(), window.latest()) {
        (EffectTemporalBoundary::Finite(earliest), EffectTemporalBoundary::Finite(latest)) => {
            Ok((earliest, latest))
        }
        _ => Err(EffectTemporalExecutionError::UnboundedTemporalInput),
    }
}

#[derive(Debug, Clone)]
struct PreparedScalarTemporalRequest {
    temporal_program: Arc<PreparedTemporalValueProgram>,
    demand: EffectExecutionDemand,
    cache_identity: [u8; 32],
    retained_coverage_bytes: usize,
    mask_rasters: HashMap<usize, Arc<PreparedMaskRasterSet>>,
}

impl PreparedScalarTemporalRequest {
    fn prepare_mask_rasters(
        mut self,
        request: &EffectTemporalExecutionRequest,
        budget: usize,
    ) -> Result<Self, EffectTemporalExecutionError> {
        if self.mask_rasters.is_empty() {
            let mask_bytes =
                self.temporal_program.contexts().iter().try_fold(0usize, |total, context| {
                    let bytes =
                        PreparedMaskRasterSet::required_retained_bytes(context.graph.graph())
                            .map_err(|error| EffectTemporalExecutionError::MaskRasterFailed {
                                source: error,
                            })?;
                    total.checked_add(bytes).ok_or(
                        EffectTemporalExecutionError::WorkingSetBudgetExceeded {
                            required_bytes: usize::MAX,
                            budget_bytes: budget,
                        },
                    )
                })?;
            let required_bytes = self.retained_coverage_bytes.checked_add(mask_bytes).ok_or(
                EffectTemporalExecutionError::WorkingSetBudgetExceeded {
                    required_bytes: usize::MAX,
                    budget_bytes: budget,
                },
            )?;
            if required_bytes > budget {
                return Err(EffectTemporalExecutionError::WorkingSetBudgetExceeded {
                    required_bytes,
                    budget_bytes: budget,
                });
            }
            let mut retained = 0usize;
            for (index, context) in self.temporal_program.contexts().iter().enumerate() {
                let rasters = PreparedMaskRasterSet::prepare(
                    context.graph.graph(),
                    request.frame_extent,
                    &request.cancellation,
                )
                .map_err(|error| {
                    EffectTemporalExecutionError::MaskRasterFailed { source: error }
                })?;
                retained = retained.checked_add(rasters.retained_bytes()).ok_or(
                    EffectTemporalExecutionError::WorkingSetBudgetExceeded {
                        required_bytes: usize::MAX,
                        budget_bytes: budget,
                    },
                )?;
                if !rasters.is_empty() {
                    self.mask_rasters.insert(index, Arc::new(rasters));
                }
            }
            if retained > mask_bytes {
                return Err(EffectTemporalExecutionError::WorkingSetLedgerMismatch {
                    expected_bytes: mask_bytes,
                    actual_bytes: retained,
                });
            }
        }
        Ok(self)
    }

    fn retained_execution_bytes(&self) -> Result<usize, EffectTemporalExecutionError> {
        self.retained_coverage_bytes
            .checked_add(
                self.mask_rasters
                    .values()
                    .map(|rasters| rasters.retained_bytes())
                    .fold(0usize, usize::saturating_add),
            )
            .ok_or(EffectTemporalExecutionError::WorkingSetBudgetExceeded {
                required_bytes: usize::MAX,
                budget_bytes: usize::MAX,
            })
    }
}

impl EffectExecutionSession {
    /// Prepare one exact finite temporal execution while retaining every
    /// dynamic topology in this owner-scoped Session.
    ///
    /// The Session never becomes graph authority: root and sampled graphs are
    /// still evaluated from the same immutable [`PreparedEffectProgram`]. It
    /// only supplies bounded topology residency shared with ordinary
    /// single-frame evaluation and later pixel execution.
    pub fn prepare_temporal_frame_execution(
        &mut self,
        program: &PreparedEffectProgram,
        request: &EffectTemporalExecutionRequest,
        mut sample_frame_seed: impl FnMut(TimelineTime) -> Result<i64, String>,
    ) -> Result<PreparedEffectTemporalExecution, EffectTemporalExecutionError> {
        self.bind_generation(request.generation());
        prepare_temporal_frame_execution_with_evaluator(
            request,
            |time| program.evaluate_with_session(time, self).map_err(|error| error.to_string()),
            &mut sample_frame_seed,
        )
    }

    /// Execute a finite temporal, stateless CPU Float32 graph under this
    /// Session's hard working-set grant.
    ///
    /// A request that fits executes as one exact ROI. Otherwise the Session
    /// derives a deterministic, non-overlapping tile schedule from the unique
    /// compiled graph, executes one tile at a time, and publishes only the
    /// completely assembled result. The retained output, tile execution, and
    /// final reference-counted publication share the same byte grant.
    pub fn execute_temporal_f32(
        &mut self,
        compiled: &CompiledEffectGraph,
        request: &EffectTemporalExecutionRequest,
        provider: &mut dyn EffectTemporalFrameProvider,
    ) -> Result<EffectTemporalExecutionOutput, EffectTemporalExecutionError> {
        let execution =
            prepare_temporal_execution_inner(Arc::new(compiled.clone()), request, None)?;
        self.execute_prepared_temporal_f32(&execution, provider)
    }

    /// Execute one already prepared time-expanded finite temporal program.
    ///
    /// Demand freezing and execution consume the same immutable projection, so
    /// exact-time graph evaluation cannot drift between the two phases.
    pub fn execute_prepared_temporal_f32(
        &mut self,
        execution: &PreparedEffectTemporalExecution,
        provider: &mut dyn EffectTemporalFrameProvider,
    ) -> Result<EffectTemporalExecutionOutput, EffectTemporalExecutionError> {
        let compiled = execution.graph.as_ref();
        let request = &execution.request;
        let prepared = self.prepare_temporal_scalar_request(
            compiled,
            request,
            Arc::clone(&execution.program),
            provider,
        )?;
        if let Some(output) = self.cached_or_empty_temporal_output(compiled, request, &prepared)? {
            return Ok(output);
        }
        let budget = self.max_working_bytes();
        let prepared = prepared.prepare_mask_rasters(request, budget)?;
        let direct_working_bytes = temporal_scalar_required_bytes(
            &prepared.temporal_program,
            &prepared.demand,
            &prepared.mask_rasters,
        )?;
        let retained_execution_bytes = prepared.retained_execution_bytes()?;
        let direct_required = retained_execution_bytes.checked_add(direct_working_bytes).ok_or(
            EffectTemporalExecutionError::WorkingSetBudgetExceeded {
                required_bytes: usize::MAX,
                budget_bytes: budget,
            },
        )?;
        if direct_required <= budget {
            let mut output = self.execute_prepared_temporal_tile_f32(
                compiled,
                request,
                provider,
                prepared,
                budget - retained_execution_bytes,
                true,
            )?;
            output.peak_working_bytes =
                output.peak_working_bytes.saturating_add(retained_execution_bytes);
            return Ok(output);
        }

        self.execute_tiled_temporal_f32(compiled, request, provider, prepared, budget)
    }

    /// Execute exactly one finite temporal Float32 ROI without scheduling
    /// additional tiles. This is the scalar tile primitive used by the
    /// adaptive Session executor and its semantic-reference tests.
    ///
    /// The scalar reference retains only the exact input ROI. Coordinate-aware
    /// operations still use complete-frame positions, finite-kernel operations
    /// consume their admitted halo, and full-frame-only operations reject a
    /// partial region. Kernel scratch and every resident graph value are
    /// admitted before allocation. Compiled use counts move last-use values in
    /// place, clone only live fan-out inputs, and release joins immediately.
    #[cfg(test)]
    fn execute_temporal_roi_f32(
        &mut self,
        compiled: &CompiledEffectGraph,
        request: &EffectTemporalExecutionRequest,
        provider: &mut dyn EffectTemporalFrameProvider,
    ) -> Result<EffectTemporalExecutionOutput, EffectTemporalExecutionError> {
        let budget = self.max_working_bytes();
        let execution =
            prepare_temporal_execution_inner(Arc::new(compiled.clone()), request, None)?;
        let prepared = self.prepare_temporal_scalar_request(
            compiled,
            request,
            Arc::clone(&execution.program),
            provider,
        )?;
        if let Some(output) = self.cached_or_empty_temporal_output(compiled, request, &prepared)? {
            return Ok(output);
        }
        let prepared = prepared.prepare_mask_rasters(request, budget)?;
        let retained_execution_bytes = prepared.retained_execution_bytes()?;
        let available = budget.checked_sub(retained_execution_bytes).ok_or(
            EffectTemporalExecutionError::WorkingSetBudgetExceeded {
                required_bytes: retained_execution_bytes,
                budget_bytes: budget,
            },
        )?;
        let mut output = self.execute_prepared_temporal_tile_f32(
            compiled, request, provider, prepared, available, true,
        )?;
        output.peak_working_bytes =
            output.peak_working_bytes.saturating_add(retained_execution_bytes);
        Ok(output)
    }

    fn prepare_temporal_scalar_request(
        &mut self,
        compiled: &CompiledEffectGraph,
        request: &EffectTemporalExecutionRequest,
        temporal_program: Arc<PreparedTemporalValueProgram>,
        provider: &dyn EffectTemporalFrameProvider,
    ) -> Result<PreparedScalarTemporalRequest, EffectTemporalExecutionError> {
        if request.cancellation.is_canceled() {
            return Err(EffectTemporalExecutionError::Canceled);
        }
        admit_temporal_scalar(compiled)?;
        if compiled.domain_plan().requires_conversion()
            || !compiled.domain_plan().blockers.is_empty()
        {
            return Err(EffectTemporalExecutionError::ColorDomainUnsupported);
        }
        self.bind_generation(request.generation);
        let demand = compiled.plan_execution_demand(
            request.output_time,
            request.frame_extent,
            request.output_roi,
        )?;
        ensure_bounded_temporal_window(demand.temporal_window())?;
        let source_identity = provider.source_identity();
        let retained_coverage_bytes = provider.retained_coverage_bytes();
        let cache_identity = temporal_cache_identity(
            compiled,
            request,
            demand.input_roi(),
            source_identity,
            temporal_program.fingerprint(),
        );
        Ok(PreparedScalarTemporalRequest {
            temporal_program,
            demand,
            cache_identity,
            retained_coverage_bytes,
            mask_rasters: HashMap::new(),
        })
    }

    fn cached_or_empty_temporal_output(
        &mut self,
        compiled: &CompiledEffectGraph,
        request: &EffectTemporalExecutionRequest,
        prepared: &PreparedScalarTemporalRequest,
    ) -> Result<Option<EffectTemporalExecutionOutput>, EffectTemporalExecutionError> {
        if compiled.output_cache_enabled() {
            if let Some(cached) = self.get_temporal_output(&prepared.cache_identity) {
                let tile = EffectFrameTileF32::from_shared(
                    request.output_time,
                    prepared.demand.frame_extent(),
                    prepared.demand.output_roi(),
                    cached.frame_seed,
                    cached.pixels,
                )?;
                return Ok(Some(EffectTemporalExecutionOutput {
                    tile,
                    cache_identity: prepared.cache_identity,
                    provider_requests: 0,
                    execution_tiles: 0,
                    peak_working_bytes: 0,
                }));
            }
        }
        if prepared.demand.output_roi().is_empty() || prepared.demand.frame_extent().is_empty() {
            let tile = EffectFrameTileF32::new(
                request.output_time,
                prepared.demand.frame_extent(),
                prepared.demand.output_roi(),
                request.output_frame_seed,
                Vec::new(),
            )?;
            return Ok(Some(EffectTemporalExecutionOutput {
                tile,
                cache_identity: prepared.cache_identity,
                provider_requests: 0,
                execution_tiles: 0,
                peak_working_bytes: 0,
            }));
        }
        Ok(None)
    }

    fn execute_prepared_temporal_tile_f32(
        &mut self,
        compiled: &CompiledEffectGraph,
        request: &EffectTemporalExecutionRequest,
        provider: &mut dyn EffectTemporalFrameProvider,
        prepared: PreparedScalarTemporalRequest,
        working_budget: usize,
        publish_output_cache: bool,
    ) -> Result<EffectTemporalExecutionOutput, EffectTemporalExecutionError> {
        let mask_rasters = prepared.mask_rasters.clone();
        let mut evaluator = ScalarTemporalEvaluator::new(
            request,
            prepared.demand.input_roi(),
            prepared.demand.exact_halo(),
            Arc::clone(&prepared.temporal_program),
            mask_rasters,
            provider,
            working_budget,
        )?;
        let output = evaluator.execute()?;
        if request.cancellation.is_canceled() {
            return Err(EffectTemporalExecutionError::Canceled);
        }
        let output_pixel_bytes = checked_pixel_count(prepared.demand.output_roi())
            .and_then(|pixels| pixels.checked_mul(std::mem::size_of::<[f32; 4]>()))
            .ok_or(EffectTemporalExecutionError::WorkingSetBudgetExceeded {
                required_bytes: usize::MAX,
                budget_bytes: evaluator.working.budget,
            })?;
        // The crop Vec enters `Arc<Vec<_>>` ownership without copying its pixel
        // allocation, so publication adds exactly one output-sized buffer.
        evaluator.working.ensure_transient(output_pixel_bytes)?;
        let output_pixels = Arc::new(crop_tile(
            &output,
            prepared.demand.input_roi().region(),
            prepared.demand.output_roi(),
            &request.cancellation,
        )?);
        let output_seed = request.output_frame_seed;
        let provider_requests = evaluator.provider_requests;
        let peak_working_bytes = evaluator.working.peak;
        drop(output);
        drop(evaluator);
        if publish_output_cache
            && compiled.output_cache_enabled()
            && !request.cancellation.is_canceled()
        {
            self.put_temporal_output(
                prepared.cache_identity,
                EffectTemporalCachedOutput {
                    pixels: Arc::clone(&output_pixels),
                    frame_seed: output_seed,
                },
            );
        }
        let tile = EffectFrameTileF32::from_shared(
            request.output_time,
            prepared.demand.frame_extent(),
            prepared.demand.output_roi(),
            output_seed,
            output_pixels,
        )?;
        Ok(EffectTemporalExecutionOutput {
            tile,
            cache_identity: prepared.cache_identity,
            provider_requests,
            execution_tiles: 1,
            peak_working_bytes,
        })
    }

    fn execute_tiled_temporal_f32(
        &mut self,
        compiled: &CompiledEffectGraph,
        request: &EffectTemporalExecutionRequest,
        provider: &mut dyn EffectTemporalFrameProvider,
        prepared: PreparedScalarTemporalRequest,
        budget: usize,
    ) -> Result<EffectTemporalExecutionOutput, EffectTemporalExecutionError> {
        temporal_cancellation_checkpoint(&request.cancellation)?;
        let output_roi = prepared.demand.output_roi();
        let output_pixels = checked_pixel_count(output_roi).ok_or(
            EffectTemporalExecutionError::WorkingSetBudgetExceeded {
                required_bytes: usize::MAX,
                budget_bytes: budget,
            },
        )?;
        let output_bytes = output_pixels.checked_mul(std::mem::size_of::<[f32; 4]>()).ok_or(
            EffectTemporalExecutionError::WorkingSetBudgetExceeded {
                required_bytes: usize::MAX,
                budget_bytes: budget,
            },
        )?;
        let mask_rasters = prepared.mask_rasters.clone();
        if mask_rasters.values().any(|rasters| rasters.extent() != request.frame_extent) {
            return Err(EffectTemporalExecutionError::InvalidGraphLiveness {
                reason: "prepared Mask raster extent changed during tiled execution",
            });
        }
        let base_resident_bytes = prepared
            .retained_execution_bytes()?
            .checked_add(output_bytes)
            .ok_or(EffectTemporalExecutionError::WorkingSetBudgetExceeded {
                required_bytes: usize::MAX,
                budget_bytes: budget,
            })?;
        if base_resident_bytes > budget {
            return Err(EffectTemporalExecutionError::WorkingSetBudgetExceeded {
                required_bytes: base_resident_bytes,
                budget_bytes: budget,
            });
        }
        let tile_budget = budget - base_resident_bytes;
        let tiles = plan_temporal_tiles(
            compiled,
            request,
            &prepared.temporal_program,
            output_roi,
            base_resident_bytes,
            tile_budget,
            budget,
            &mask_rasters,
        )?;
        let mut assembled = vec![[0.0_f32; 4]; output_pixels];
        let mut provider_requests = 0usize;
        let mut peak_working_bytes = base_resident_bytes;
        let execution_tiles = tiles.len();

        for tile_roi in tiles {
            temporal_cancellation_checkpoint(&request.cancellation)?;
            let tile_request = EffectTemporalExecutionRequest {
                generation: request.generation,
                continuity: request.continuity,
                output_time: request.output_time,
                output_frame_seed: request.output_frame_seed,
                frame_extent: request.frame_extent,
                output_roi: tile_roi,
                cancellation: request.cancellation.clone(),
            };
            let mut tile_prepared = self.prepare_temporal_scalar_request(
                compiled,
                &tile_request,
                Arc::clone(&prepared.temporal_program),
                provider,
            )?;
            if tile_prepared.retained_coverage_bytes != prepared.retained_coverage_bytes {
                return Err(EffectTemporalExecutionError::InvalidGraphLiveness {
                    reason: "temporal provider coverage changed during one tiled execution",
                });
            }
            tile_prepared.mask_rasters = mask_rasters.clone();
            // Internal tiles are one attempt-local implementation detail. They
            // never read or publish output-cache entries: a later cancellation
            // must not leave a partially completed frame represented in the
            // Session cache.
            let tile_output = self.execute_prepared_temporal_tile_f32(
                compiled,
                &tile_request,
                provider,
                tile_prepared,
                tile_budget,
                false,
            )?;
            let required_with_output = base_resident_bytes
                .checked_add(tile_output.peak_working_bytes)
                .ok_or(EffectTemporalExecutionError::WorkingSetBudgetExceeded {
                    required_bytes: usize::MAX,
                    budget_bytes: budget,
                })?;
            if required_with_output > budget {
                return Err(EffectTemporalExecutionError::WorkingSetLedgerMismatch {
                    expected_bytes: budget,
                    actual_bytes: required_with_output,
                });
            }
            peak_working_bytes = peak_working_bytes.max(required_with_output);
            provider_requests = provider_requests.saturating_add(tile_output.provider_requests);
            stitch_temporal_output_tile(
                &mut assembled,
                output_roi,
                tile_output.tile(),
                &request.cancellation,
            )?;
        }

        temporal_cancellation_checkpoint(&request.cancellation)?;
        let output_pixels = Arc::new(assembled);
        if compiled.output_cache_enabled() && !request.cancellation.is_canceled() {
            self.put_temporal_output(
                prepared.cache_identity,
                EffectTemporalCachedOutput {
                    pixels: Arc::clone(&output_pixels),
                    frame_seed: request.output_frame_seed,
                },
            );
        }
        let tile = EffectFrameTileF32::from_shared(
            request.output_time,
            prepared.demand.frame_extent(),
            output_roi,
            request.output_frame_seed,
            output_pixels,
        )?;
        Ok(EffectTemporalExecutionOutput {
            tile,
            cache_identity: prepared.cache_identity,
            provider_requests,
            execution_tiles,
            peak_working_bytes,
        })
    }
}

fn admit_temporal_scalar(
    compiled: &CompiledEffectGraph,
) -> Result<(), EffectTemporalExecutionError> {
    for (stage_index, contract) in compiled.execution_envelope().stages().iter().enumerate() {
        if !contract.execution_modes.contains(
            EffectProcessingBackend::Cpu,
            EffectWorkingPrecision::Float32,
        ) {
            return Err(EffectTemporalExecutionError::ExecutionModeNotAdmitted { stage_index });
        }
    }
    let aggregate = compiled.execution_envelope().aggregate();
    if aggregate.state_model != EffectStateModel::Stateless {
        return Err(EffectTemporalExecutionError::StatefulUnsupported);
    }
    if aggregate.resource_lifetime == EffectResourceLifetime::ContinuitySession {
        return Err(EffectTemporalExecutionError::ContinuityResourceUnsupported);
    }
    if matches!(aggregate.temporal_input.past, EffectTemporalSpan::Unbounded)
        || matches!(
            aggregate.temporal_input.future,
            EffectTemporalSpan::Unbounded
        )
    {
        return Err(EffectTemporalExecutionError::UnboundedTemporalInput);
    }
    Ok(())
}

fn validate_temporal_blend(
    _sample_offset: TimelineTime,
    mix: f32,
) -> Result<(), EffectTemporalExecutionError> {
    if !mix.is_finite() || !(0.0..=1.0).contains(&mix) {
        return Err(EffectTemporalExecutionError::InvalidTemporalOperation {
            reason: "mix must be finite and within [0, 1]",
        });
    }
    Ok(())
}

fn temporal_sample_time(
    time: TimelineTime,
    sample_offset: TimelineTime,
) -> Result<TimelineTime, EffectTemporalExecutionError> {
    time.checked_add(sample_offset).map_err(|_| {
        EffectTemporalExecutionError::InvalidTemporalOperation {
            reason: "temporal sample arithmetic overflowed",
        }
    })
}

fn ensure_bounded_temporal_window(
    window: crate::EffectTemporalWindow,
) -> Result<(), EffectTemporalExecutionError> {
    if matches!(window.earliest(), EffectTemporalBoundary::Unbounded)
        || matches!(window.latest(), EffectTemporalBoundary::Unbounded)
    {
        return Err(EffectTemporalExecutionError::UnboundedTemporalInput);
    }
    Ok(())
}

fn stitch_temporal_output_tile(
    assembled: &mut [[f32; 4]],
    output_roi: EffectPixelRoi,
    tile: &EffectFrameTileF32,
    cancellation: &ExecutionCancellationToken,
) -> Result<(), EffectTemporalExecutionError> {
    if !roi_contains(output_roi, tile.roi())
        || checked_pixel_count(tile.roi()) != Some(tile.pixels().len())
        || checked_pixel_count(output_roi) != Some(assembled.len())
    {
        return Err(EffectTemporalExecutionError::InvalidRoiProjection {
            reason: "scheduled temporal tile does not match the assembled output region",
        });
    }
    let output_width = output_roi.width() as usize;
    let local_x = (tile.roi().x() - output_roi.x()) as usize;
    let local_y = (tile.roi().y() - output_roi.y()) as usize;
    let tile_width = tile.roi().width() as usize;
    for row in 0..tile.roi().height() as usize {
        temporal_cancellation_checkpoint(cancellation)?;
        let output_start = (local_y + row) * output_width + local_x;
        let tile_start = row * tile_width;
        assembled[output_start..output_start + tile_width]
            .copy_from_slice(&tile.pixels()[tile_start..tile_start + tile_width]);
    }
    temporal_cancellation_checkpoint(cancellation)?;
    Ok(())
}

struct ScalarTemporalEvaluator<'a> {
    request: &'a EffectTemporalExecutionRequest,
    input_roi: EffectInputRoi,
    raster_region: EffectRasterRegion,
    exact_halo: Option<EffectRoiHalo>,
    temporal_program: Arc<PreparedTemporalValueProgram>,
    mask_rasters: HashMap<usize, Arc<PreparedMaskRasterSet>>,
    provider: &'a mut dyn EffectTemporalFrameProvider,
    outputs: HashMap<TemporalValueAddress, Vec<[f32; 4]>>,
    remaining_uses: HashMap<TemporalValueAddress, usize>,
    provider_requests: usize,
    working: ScalarWorkingSet,
}

impl<'a> ScalarTemporalEvaluator<'a> {
    fn new(
        request: &'a EffectTemporalExecutionRequest,
        input_roi: EffectInputRoi,
        exact_halo: Option<EffectRoiHalo>,
        temporal_program: Arc<PreparedTemporalValueProgram>,
        mask_rasters: HashMap<usize, Arc<PreparedMaskRasterSet>>,
        provider: &'a mut dyn EffectTemporalFrameProvider,
        working_budget: usize,
    ) -> Result<Self, EffectTemporalExecutionError> {
        let input_region = input_roi.region();
        let frame_bytes = checked_pixel_count(input_region)
            .and_then(|pixels| pixels.checked_mul(std::mem::size_of::<[f32; 4]>()))
            .ok_or(EffectTemporalExecutionError::WorkingSetBudgetExceeded {
                required_bytes: usize::MAX,
                budget_bytes: working_budget,
            })?;
        if frame_bytes > working_budget {
            return Err(EffectTemporalExecutionError::WorkingSetBudgetExceeded {
                required_bytes: frame_bytes,
                budget_bytes: working_budget,
            });
        }
        let remaining_uses = temporal_program.use_counts().clone();
        let output_capacity = temporal_program.ordered_values().len();
        Ok(Self {
            request,
            input_roi,
            raster_region: EffectRasterRegion::new(
                request.frame_extent.width(),
                request.frame_extent.height(),
                input_region.x(),
                input_region.y(),
                input_region.width(),
                input_region.height(),
            ),
            exact_halo,
            temporal_program,
            mask_rasters,
            provider,
            outputs: HashMap::with_capacity(output_capacity),
            remaining_uses,
            provider_requests: 0,
            working: ScalarWorkingSet::new(working_budget, frame_bytes),
        })
    }

    fn execute(&mut self) -> Result<Vec<[f32; 4]>, EffectTemporalExecutionError> {
        let schedule = self.temporal_program.ordered_values().to_vec();
        for address in schedule {
            temporal_cancellation_checkpoint(&self.request.cancellation)?;
            let output = match address {
                TemporalValueAddress::Source(time) => self.fetch_source(time)?,
                TemporalValueAddress::Graph { context, node_id } => {
                    let context_ref = self.temporal_program.context(context)?;
                    let frame_seed = context_ref.frame_seed;
                    let node = context_ref.graph.graph().node(node_id).cloned().ok_or(
                        EffectTemporalExecutionError::UnsupportedGraphNode {
                            node_id,
                            kind: "missing",
                        },
                    )?;
                    match node.kind {
                        EffectGraphNodeKind::Source => {
                            return Err(EffectTemporalExecutionError::InvalidGraphLiveness {
                                reason:
                                    "source node was not normalized in the time-expanded schedule",
                            });
                        }
                        EffectGraphNodeKind::UnaryEffect { input, op }
                        | EffectGraphNodeKind::DomainEffect { input, op, .. } => match op {
                            crate::EffectRenderOp::TemporalFrameBlend { sample_offset, mix } => {
                                validate_temporal_blend(sample_offset, mix)?;
                                let input =
                                    self.temporal_program.address_for_input(context, input)?;
                                let sample = self.temporal_program.temporal_sample(address).ok_or(
                                    EffectTemporalExecutionError::InvalidGraphLiveness {
                                        reason: "temporal graph value has no expanded sample edge",
                                    },
                                )?;
                                self.temporal_blend(input, sample, mix)?
                            }
                            op => {
                                let input =
                                    self.temporal_program.address_for_input(context, input)?;
                                let mut output = self.take_value(input)?;
                                let scratch_bytes = render_op_f32_scratch_frames(&op)
                                    .checked_mul(self.working.frame_bytes)
                                    .ok_or(
                                        EffectTemporalExecutionError::WorkingSetBudgetExceeded {
                                            required_bytes: usize::MAX,
                                            budget_bytes: self.working.budget,
                                        },
                                    )?;
                                self.working.ensure_transient(scratch_bytes)?;
                                let cancellation = self.request.cancellation.clone();
                                let execution = apply_render_op_f32_region_controlled(
                                    &mut output,
                                    self.raster_region,
                                    &op,
                                    frame_seed,
                                    &mut || temporal_cancellation_checkpoint(&cancellation),
                                );
                                match execution {
                                    Ok(true) => {}
                                    Ok(false) => {
                                        return Err(
                                    EffectTemporalExecutionError::UnsupportedRenderOperation {
                                        op: render_op_name(&op),
                                    },
                                );
                                    }
                                    Err(error) => return Err(error),
                                }
                                output
                            }
                        },
                        EffectGraphNodeKind::Blend { base, overlay, blend_mode, opacity } => {
                            let base = self.temporal_program.address_for_input(context, base)?;
                            let overlay =
                                self.temporal_program.address_for_input(context, overlay)?;
                            let mut base = self.take_value(base)?;
                            let overlay = self.take_value(overlay)?;
                            let cancellation = self.request.cancellation.clone();
                            let blended = blend_rgba_f32_region_controlled(
                                &mut base,
                                &overlay,
                                self.raster_region,
                                opacity,
                                blend_mode,
                                frame_seed,
                                &mut || temporal_cancellation_checkpoint(&cancellation),
                            )?;
                            if !blended {
                                return Err(EffectTemporalExecutionError::InvalidRoiProjection {
                                    reason: "blend inputs do not match the admitted raster region",
                                });
                            }
                            self.working.release_frame()?;
                            base
                        }
                        EffectGraphNodeKind::Mask { input, mask, invert, mask_op } => {
                            let input = self.temporal_program.address_for_input(context, input)?;
                            let mask = self.temporal_program.address_for_input(context, mask)?;
                            let mut output = self.take_value(input)?;
                            let mask_pixels = self.take_value(mask)?;
                            if output.len() != mask_pixels.len() {
                                return Err(EffectTemporalExecutionError::InvalidRoiProjection {
                                    reason: "Mask input and raster do not cover the same region",
                                });
                            }
                            let cancellation = self.request.cancellation.clone();
                            apply_alpha_mask_f32_region_controlled(
                                &mut output,
                                &mask_pixels,
                                invert,
                                mask_op,
                                &mut || temporal_cancellation_checkpoint(&cancellation),
                            )?;
                            self.working.release_frame()?;
                            output
                        }
                        EffectGraphNodeKind::MaskSource { .. } => {
                            let raster = self
                                .mask_rasters
                                .get(&context)
                                .map(Arc::as_ref)
                                .and_then(|rasters| rasters.get(node_id))
                                .ok_or(EffectTemporalExecutionError::InvalidGraphLiveness {
                                    reason: "prepared Mask raster is missing for a MaskSource node",
                                })?;
                            if raster.extent() != self.request.frame_extent {
                                return Err(EffectTemporalExecutionError::InvalidGraphLiveness {
                                    reason:
                                        "prepared Mask raster extent does not match the request",
                                });
                            }
                            self.working.reserve_frame()?;
                            self.working.ensure_transient(raster.max_scratch_bytes())?;
                            raster
                                .rasterize_rgba_f32(
                                    self.input_roi.region(),
                                    &self.request.cancellation,
                                )
                                .map_err(|error| EffectTemporalExecutionError::MaskRasterFailed {
                                    source: error,
                                })?
                        }
                        EffectGraphNodeKind::MultiInput { inputs, blend_mode, opacity } => {
                            let Some(first) = inputs.first().copied() else {
                                return Err(EffectTemporalExecutionError::InvalidGraphLiveness {
                                    reason: "multi-input node has no inputs",
                                });
                            };
                            let first = self.temporal_program.address_for_input(context, first)?;
                            let mut output = self.take_value(first)?;
                            for overlay_id in &inputs[1..] {
                                let overlay = self
                                    .temporal_program
                                    .address_for_input(context, *overlay_id)?;
                                let overlay = self.take_value(overlay)?;
                                let cancellation = self.request.cancellation.clone();
                                let blended = blend_rgba_f32_region_controlled(
                                    &mut output,
                                    &overlay,
                                    self.raster_region,
                                    opacity,
                                    blend_mode,
                                    frame_seed,
                                    &mut || temporal_cancellation_checkpoint(&cancellation),
                                )?;
                                if !blended {
                                    return Err(EffectTemporalExecutionError::InvalidRoiProjection {
                                reason:
                                    "multi-input values do not match the admitted raster region",
                            });
                                }
                                self.working.release_frame()?;
                            }
                            output
                        }
                    }
                }
            };
            if self.outputs.insert(address, output).is_some() {
                return Err(EffectTemporalExecutionError::InvalidGraphLiveness {
                    reason: "time-expanded schedule produced one value more than once",
                });
            }
        }

        let output_id = self.temporal_program.root();
        let output = self
            .outputs
            .remove(&output_id)
            .ok_or(EffectTemporalExecutionError::MissingGraphOutput)?;
        if !self.outputs.is_empty() {
            return Err(EffectTemporalExecutionError::InvalidGraphLiveness {
                reason: "compiled use counts left non-output values resident",
            });
        }
        if self.remaining_uses.values().any(|remaining| *remaining != 0) {
            return Err(EffectTemporalExecutionError::InvalidGraphLiveness {
                reason: "time-expanded use counts retained unconsumed edges",
            });
        }
        if self.working.resident_bytes != self.working.frame_bytes {
            return Err(EffectTemporalExecutionError::WorkingSetLedgerMismatch {
                expected_bytes: self.working.frame_bytes,
                actual_bytes: self.working.resident_bytes,
            });
        }
        Ok(output)
    }

    fn fetch_source(
        &mut self,
        time: TimelineTime,
    ) -> Result<Vec<[f32; 4]>, EffectTemporalExecutionError> {
        if self.request.cancellation.is_canceled() {
            return Err(EffectTemporalExecutionError::Canceled);
        }
        let provider_request = EffectTemporalFrameRequest {
            generation: self.request.generation,
            time,
            frame_extent: self.request.frame_extent,
            input_roi: self.input_roi,
            exact_halo: self.exact_halo,
            precision: EffectWorkingPrecision::Float32,
        };
        let pixel_count = checked_pixel_count(provider_request.input_roi.region()).ok_or(
            EffectTemporalExecutionError::WorkingSetBudgetExceeded {
                required_bytes: usize::MAX,
                budget_bytes: self.working.budget,
            },
        )?;
        self.working.reserve_frame()?;
        let mut pixels = Vec::with_capacity(pixel_count);
        self.provider
            .copy_frame(provider_request, &self.request.cancellation, &mut pixels)?;
        self.provider_requests = self.provider_requests.saturating_add(1);
        if self.request.cancellation.is_canceled() {
            return Err(EffectTemporalExecutionError::Canceled);
        }
        if pixels.len() != pixel_count {
            return Err(EffectTemporalFrameProviderError::InvalidTile {
                reason: format!(
                    "provider copied {} pixels but ROI requires {pixel_count}",
                    pixels.len()
                ),
            }
            .into());
        }
        Ok(pixels)
    }

    fn temporal_blend(
        &mut self,
        input: TemporalValueAddress,
        sample: TemporalValueAddress,
        mix: f32,
    ) -> Result<Vec<[f32; 4]>, EffectTemporalExecutionError> {
        let mut current = self.take_value(input)?;
        let sample = self.take_value(sample)?;
        if current.len() != sample.len() {
            return Err(EffectTemporalExecutionError::InvalidRoiProjection {
                reason: "temporal input frames do not have equal pixel counts",
            });
        }
        mix_temporal_frames_in_place_controlled(&mut current, &sample, mix, &mut || {
            temporal_cancellation_checkpoint(&self.request.cancellation)
        })?;
        self.working.release_frame()?;
        Ok(current)
    }

    fn take_value(
        &mut self,
        address: TemporalValueAddress,
    ) -> Result<Vec<[f32; 4]>, EffectTemporalExecutionError> {
        if !self.outputs.contains_key(&address) {
            return Err(missing_temporal_value(address));
        }
        let remaining = self.remaining_uses.get_mut(&address).ok_or(
            EffectTemporalExecutionError::InvalidGraphLiveness {
                reason: "compiled graph value has no use-count evidence",
            },
        )?;
        if *remaining == 0 {
            return Err(EffectTemporalExecutionError::InvalidGraphLiveness {
                reason: "compiled graph value was consumed more often than declared",
            });
        }
        *remaining -= 1;
        if *remaining == 0 {
            return self.outputs.remove(&address).ok_or_else(|| missing_temporal_value(address));
        }

        self.working.reserve_frame()?;
        self.outputs
            .get(&address)
            .cloned()
            .ok_or_else(|| missing_temporal_value(address))
    }
}

fn missing_temporal_value(address: TemporalValueAddress) -> EffectTemporalExecutionError {
    match address {
        TemporalValueAddress::Graph { node_id, .. } => {
            EffectTemporalExecutionError::MissingGraphValue { node_id }
        }
        TemporalValueAddress::Source(_) => EffectTemporalExecutionError::InvalidGraphLiveness {
            reason: "time-expanded source value is missing",
        },
    }
}

struct ScalarWorkingSet {
    budget: usize,
    frame_bytes: usize,
    resident_bytes: usize,
    peak: usize,
}

impl ScalarWorkingSet {
    const fn new(budget: usize, frame_bytes: usize) -> Self {
        Self { budget, frame_bytes, resident_bytes: 0, peak: 0 }
    }

    fn reserve_frame(&mut self) -> Result<(), EffectTemporalExecutionError> {
        let required = self.resident_bytes.checked_add(self.frame_bytes).ok_or(
            EffectTemporalExecutionError::WorkingSetBudgetExceeded {
                required_bytes: usize::MAX,
                budget_bytes: self.budget,
            },
        )?;
        if required > self.budget {
            return Err(EffectTemporalExecutionError::WorkingSetBudgetExceeded {
                required_bytes: required,
                budget_bytes: self.budget,
            });
        }
        self.resident_bytes = required;
        self.peak = self.peak.max(required);
        Ok(())
    }

    fn release_frame(&mut self) -> Result<(), EffectTemporalExecutionError> {
        self.resident_bytes = self.resident_bytes.checked_sub(self.frame_bytes).ok_or(
            EffectTemporalExecutionError::InvalidGraphLiveness {
                reason: "working-set ledger released a non-resident frame",
            },
        )?;
        Ok(())
    }

    fn ensure_transient(&mut self, bytes: usize) -> Result<(), EffectTemporalExecutionError> {
        let required = self.resident_bytes.checked_add(bytes).ok_or(
            EffectTemporalExecutionError::WorkingSetBudgetExceeded {
                required_bytes: usize::MAX,
                budget_bytes: self.budget,
            },
        )?;
        if required > self.budget {
            return Err(EffectTemporalExecutionError::WorkingSetBudgetExceeded {
                required_bytes: required,
                budget_bytes: self.budget,
            });
        }
        self.peak = self.peak.max(required);
        Ok(())
    }
}

fn validate_provider_tile(
    tile: &EffectFrameTileF32,
    request: EffectTemporalFrameRequest,
) -> Result<(), EffectTemporalFrameProviderError> {
    if tile.time != request.time {
        return Err(EffectTemporalFrameProviderError::InvalidTile {
            reason: "tile time does not match the exact request".to_owned(),
        });
    }
    if tile.frame_extent != request.frame_extent {
        return Err(EffectTemporalFrameProviderError::InvalidTile {
            reason: "tile frame extent does not match the exact request".to_owned(),
        });
    }
    if tile.roi != request.input_roi.region() {
        return Err(EffectTemporalFrameProviderError::InvalidTile {
            reason: "tile ROI does not match the exact request".to_owned(),
        });
    }
    Ok(())
}

fn temporal_cache_identity(
    compiled: &CompiledEffectGraph,
    request: &EffectTemporalExecutionRequest,
    input_roi: EffectInputRoi,
    source_identity: EffectTemporalSourceIdentity,
    temporal_program_fingerprint: [u8; 32],
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"mondrian.effect-temporal-roi-execution.v3");
    hasher.update(compiled.semantic_fingerprint());
    hasher.update(temporal_program_fingerprint);
    hasher.update(source_identity.semantic_fingerprint());
    hasher.update(request.generation.to_le_bytes());
    hash_time(&mut hasher, request.output_time);
    hasher.update(request.output_frame_seed.to_le_bytes());
    hasher.update(request.frame_extent.width().to_le_bytes());
    hasher.update(request.frame_extent.height().to_le_bytes());
    hash_roi(&mut hasher, request.output_roi);
    match input_roi {
        EffectInputRoi::Exact(roi) => {
            hasher.update([0]);
            hash_roi(&mut hasher, roi);
        }
        EffectInputRoi::ExactFullFrame(roi) => {
            hasher.update([1]);
            hash_roi(&mut hasher, roi);
        }
        EffectInputRoi::UnknownConservativeFullFrame(roi) => {
            hasher.update([2]);
            hash_roi(&mut hasher, roi);
        }
    }
    hasher.update([match request.continuity {
        EffectExecutionContinuity::Continuous => 0,
        EffectExecutionContinuity::Discontinuous => 1,
    }]);
    hasher.finalize().into()
}

fn hash_time(hasher: &mut Sha256, time: TimelineTime) {
    hasher.update(time.numerator().to_le_bytes());
    hasher.update(time.denominator().to_le_bytes());
}

fn hash_roi(hasher: &mut Sha256, roi: EffectPixelRoi) {
    hasher.update(roi.x().to_le_bytes());
    hasher.update(roi.y().to_le_bytes());
    hasher.update(roi.width().to_le_bytes());
    hasher.update(roi.height().to_le_bytes());
}

fn checked_pixel_count(extent: impl PixelExtent + Copy) -> Option<usize> {
    usize::try_from(extent.width())
        .ok()?
        .checked_mul(usize::try_from(extent.height()).ok()?)
}

trait PixelExtent {
    fn width(self) -> u32;
    fn height(self) -> u32;
}

impl PixelExtent for EffectFrameExtent {
    fn width(self) -> u32 {
        self.width()
    }

    fn height(self) -> u32 {
        self.height()
    }
}

impl PixelExtent for EffectPixelRoi {
    fn width(self) -> u32 {
        self.width()
    }

    fn height(self) -> u32 {
        self.height()
    }
}

fn clamp_roi(roi: EffectPixelRoi, extent: EffectFrameExtent) -> EffectPixelRoi {
    let x = roi.x().min(extent.width());
    let y = roi.y().min(extent.height());
    let right = (u64::from(roi.x()) + u64::from(roi.width())).min(u64::from(extent.width()));
    let bottom = (u64::from(roi.y()) + u64::from(roi.height())).min(u64::from(extent.height()));
    EffectPixelRoi::new(
        x,
        y,
        u32::try_from(right.saturating_sub(u64::from(x))).unwrap_or(u32::MAX),
        u32::try_from(bottom.saturating_sub(u64::from(y))).unwrap_or(u32::MAX),
    )
}

fn roi_contains(outer: EffectPixelRoi, inner: EffectPixelRoi) -> bool {
    let outer_right = u64::from(outer.x()) + u64::from(outer.width());
    let outer_bottom = u64::from(outer.y()) + u64::from(outer.height());
    let inner_right = u64::from(inner.x()) + u64::from(inner.width());
    let inner_bottom = u64::from(inner.y()) + u64::from(inner.height());
    inner.x() >= outer.x()
        && inner.y() >= outer.y()
        && inner_right <= outer_right
        && inner_bottom <= outer_bottom
}

fn copy_tile_region(
    tile: &EffectFrameTileF32,
    requested_roi: EffectPixelRoi,
    cancellation: &ExecutionCancellationToken,
    destination: &mut Vec<[f32; 4]>,
) -> Result<(), EffectTemporalFrameProviderError> {
    if !roi_contains(tile.roi, requested_roi) {
        return Err(EffectTemporalFrameProviderError::InvalidTile {
            reason: "prepared source tile does not contain the requested ROI".to_owned(),
        });
    }
    let tile_width = tile.roi.width() as usize;
    let local_x = (requested_roi.x() - tile.roi.x()) as usize;
    let local_y = (requested_roi.y() - tile.roi.y()) as usize;
    for row in 0..requested_roi.height() as usize {
        let start = (local_y + row) * tile_width + local_x;
        let source_row = tile
            .pixels
            .get(start..start + requested_roi.width() as usize)
            .ok_or_else(|| EffectTemporalFrameProviderError::InvalidTile {
                reason: "prepared source tile storage does not cover its declared ROI".to_owned(),
            })?;
        for chunk in source_row.chunks(4_096) {
            provider_cancellation_checkpoint(cancellation)?;
            destination.extend_from_slice(chunk);
        }
    }
    provider_cancellation_checkpoint(cancellation)?;
    Ok(())
}

fn provider_cancellation_checkpoint(
    cancellation: &ExecutionCancellationToken,
) -> Result<(), EffectTemporalFrameProviderError> {
    if cancellation.is_canceled() {
        Err(EffectTemporalFrameProviderError::Canceled)
    } else {
        Ok(())
    }
}

fn crop_tile(
    frame: &[[f32; 4]],
    input_roi: EffectPixelRoi,
    output_roi: EffectPixelRoi,
    cancellation: &ExecutionCancellationToken,
) -> Result<Vec<[f32; 4]>, EffectTemporalExecutionError> {
    let input_right = u64::from(input_roi.x()) + u64::from(input_roi.width());
    let input_bottom = u64::from(input_roi.y()) + u64::from(input_roi.height());
    let output_right = u64::from(output_roi.x()) + u64::from(output_roi.width());
    let output_bottom = u64::from(output_roi.y()) + u64::from(output_roi.height());
    if output_roi.x() < input_roi.x()
        || output_roi.y() < input_roi.y()
        || output_right > input_right
        || output_bottom > input_bottom
    {
        return Err(EffectTemporalExecutionError::InvalidRoiProjection {
            reason: "output region is not contained by its planned input region",
        });
    }
    if checked_pixel_count(input_roi) != Some(frame.len()) {
        return Err(EffectTemporalExecutionError::InvalidRoiProjection {
            reason: "input pixel buffer does not match its planned region",
        });
    }
    let mut output = Vec::with_capacity(checked_pixel_count(output_roi).unwrap_or(0));
    let input_width = input_roi.width() as usize;
    let local_x = (output_roi.x() - input_roi.x()) as usize;
    let local_y = (output_roi.y() - input_roi.y()) as usize;
    for row in 0..output_roi.height() as usize {
        temporal_cancellation_checkpoint(cancellation)?;
        let start = (local_y + row) * input_width + local_x;
        output.extend_from_slice(&frame[start..start + output_roi.width() as usize]);
    }
    temporal_cancellation_checkpoint(cancellation)?;
    Ok(output)
}

#[cfg(test)]
fn crop_frame(
    frame: &[[f32; 4]],
    extent: EffectFrameExtent,
    roi: EffectPixelRoi,
    cancellation: &ExecutionCancellationToken,
) -> Result<Vec<[f32; 4]>, EffectTemporalExecutionError> {
    crop_tile(frame, extent.full_frame_roi(), roi, cancellation)
}

fn temporal_cancellation_checkpoint(
    cancellation: &ExecutionCancellationToken,
) -> Result<(), EffectTemporalExecutionError> {
    if cancellation.is_canceled() {
        Err(EffectTemporalExecutionError::Canceled)
    } else {
        Ok(())
    }
}

fn mix_straight_rgba(current: [f32; 4], past: [f32; 4], mix: f32) -> [f32; 4] {
    let current_alpha = current[3].clamp(0.0, 1.0);
    let past_alpha = past[3].clamp(0.0, 1.0);
    let inverse = 1.0 - mix;
    let alpha = current_alpha.mul_add(inverse, past_alpha * mix);
    if alpha <= f32::EPSILON {
        return [0.0, 0.0, 0.0, 0.0];
    }
    [
        (current[0] * current_alpha * inverse + past[0] * past_alpha * mix) / alpha,
        (current[1] * current_alpha * inverse + past[1] * past_alpha * mix) / alpha,
        (current[2] * current_alpha * inverse + past[2] * past_alpha * mix) / alpha,
        alpha,
    ]
}

fn mix_temporal_frames_in_place_controlled<E>(
    current: &mut [[f32; 4]],
    past: &[[f32; 4]],
    mix: f32,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), E> {
    for (current_chunk, past_chunk) in current.chunks_mut(4_096).zip(past.chunks(4_096)) {
        checkpoint()?;
        for (current, past) in current_chunk.iter_mut().zip(past_chunk) {
            *current = mix_straight_rgba(*current, *past, mix);
        }
    }
    checkpoint()?;
    Ok(())
}

#[cfg(test)]
fn mix_temporal_frames_controlled<E>(
    current: &[[f32; 4]],
    past: &[[f32; 4]],
    mix: f32,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<Vec<[f32; 4]>, E> {
    let mut output = current.to_vec();
    mix_temporal_frames_in_place_controlled(&mut output, past, mix, checkpoint)?;
    Ok(output)
}

fn render_op_name(op: &crate::EffectRenderOp) -> &'static str {
    match op {
        crate::EffectRenderOp::ColorAdjust { .. } => "color_adjust",
        crate::EffectRenderOp::GaussianBlur { .. } => "gaussian_blur",
        crate::EffectRenderOp::Sharpen { .. } => "sharpen",
        crate::EffectRenderOp::Vignette { .. } => "vignette",
        crate::EffectRenderOp::ChromaticAberration { .. } => "chromatic_aberration",
        crate::EffectRenderOp::Grain { .. } => "grain",
        crate::EffectRenderOp::TemporalFrameBlend { .. } => "temporal_frame_blend",
        crate::EffectRenderOp::Lut3D { .. } => "lut3d",
        crate::EffectRenderOp::Custom { .. } => "custom",
    }
}

fn frame_seed_for_output(time: TimelineTime) -> i64 {
    let mut hasher = Sha256::new();
    hasher.update(b"mondrian.effect-frame-seed-fallback.v1");
    hash_time(&mut hasher, time);
    let digest = hasher.finalize();
    i64::from_le_bytes([
        digest[0], digest[1], digest[2], digest[3], digest[4], digest[5], digest[6], digest[7],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        apply_compiled_effect_graph_rgba_f32, prepare_effect_graph_topology, EffectDeterminism,
        EffectExecutionContract, EffectExecutionEnvelope, EffectExecutionModes,
        EffectExecutionSessionConfig, EffectGraphBuilderState, EffectGraphNode,
        EffectGraphTopology, EffectRenderGraph, EffectResourceLifetime, EffectRoiPropagation,
        EffectTemporalInputExtent,
    };
    use mondrian_core::BlendMode;

    #[test]
    fn temporal_blend_checks_cancellation_at_fixed_pixel_chunks() {
        let current = vec![[1.0, 0.0, 0.0, 1.0]; 8_193];
        let past = vec![[0.0, 0.0, 1.0, 1.0]; 8_193];
        let mut checkpoints = 0_u32;
        let result = mix_temporal_frames_controlled(&current, &past, 0.5, &mut || {
            checkpoints = checkpoints.saturating_add(1);
            if checkpoints == 2 {
                Err(EffectTemporalExecutionError::Canceled)
            } else {
                Ok(())
            }
        });
        assert_eq!(result, Err(EffectTemporalExecutionError::Canceled));
        assert_eq!(
            checkpoints, 2,
            "8,193 pixels must cross a 4,096-pixel checkpoint boundary"
        );
    }

    struct GradientProvider {
        identity: EffectTemporalSourceIdentity,
        extent: EffectFrameExtent,
        requests: Vec<EffectTemporalFrameRequest>,
    }

    impl GradientProvider {
        fn new(extent: EffectFrameExtent) -> Self {
            Self {
                identity: EffectTemporalSourceIdentity::from_complete_semantic_fingerprint([7; 32]),
                extent,
                requests: Vec::new(),
            }
        }

        fn pixel(time: TimelineTime, x: u32, y: u32) -> [f32; 4] {
            [
                time.to_f64() as f32 + x as f32 * 0.1,
                y as f32 * 0.2,
                (x + y) as f32 * 0.05,
                1.0,
            ]
        }
    }

    impl EffectTemporalFrameProvider for GradientProvider {
        fn source_identity(&self) -> EffectTemporalSourceIdentity {
            self.identity
        }

        fn retained_coverage_bytes(&self) -> usize {
            0
        }

        fn copy_frame(
            &mut self,
            request: EffectTemporalFrameRequest,
            cancellation: &ExecutionCancellationToken,
            destination: &mut Vec<[f32; 4]>,
        ) -> Result<(), EffectTemporalFrameProviderError> {
            if cancellation.is_canceled() {
                return Err(EffectTemporalFrameProviderError::Canceled);
            }
            assert_eq!(request.frame_extent(), self.extent);
            assert!(destination.is_empty());
            let roi = request.input_roi().region();
            for y in roi.y()..roi.y() + roi.height() {
                for x in roi.x()..roi.x() + roi.width() {
                    destination.push(Self::pixel(request.time(), x, y));
                }
                provider_cancellation_checkpoint(cancellation)?;
            }
            self.requests.push(request);
            Ok(())
        }
    }

    fn bind_linear(
        ops: impl IntoIterator<Item = crate::EffectRenderOp>,
        contract: EffectExecutionContract,
    ) -> Arc<CompiledEffectGraph> {
        let mut builder = EffectGraphBuilderState::new();
        for op in ops {
            builder.append_unary(op);
        }
        let graph = builder.finish();
        bind_graph(graph, contract)
    }

    fn bind_graph(
        graph: EffectRenderGraph,
        contract: EffectExecutionContract,
    ) -> Arc<CompiledEffectGraph> {
        let topology = prepare_effect_graph_topology(&graph).expect("valid topology");
        let source = EffectGraphNodeId(0);
        let output = graph.output.expect("graph output");
        let emitted_nodes = graph
            .nodes
            .iter()
            .filter_map(|node| (node.id != source).then_some(node.id))
            .collect::<Vec<_>>();
        topology
            .bind_with_execution_bindings(
                graph,
                EffectExecutionEnvelope::new(contract, Arc::from([contract])),
                Arc::from([crate::graph::CompiledEffectStageBinding::new(
                    0,
                    contract,
                    source,
                    output,
                    emitted_nodes,
                )]),
            )
            .expect("bound graph")
    }

    fn bind_effected_temporal_graph(
        exposure: f32,
        offset: TimelineTime,
    ) -> Arc<CompiledEffectGraph> {
        let current = EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            determinism: EffectDeterminism::Deterministic,
            state_model: EffectStateModel::Stateless,
            temporal_input: EffectTemporalInputExtent::CURRENT_FRAME,
            roi_propagation: EffectRoiPropagation::PixelLocal,
            resource_lifetime: EffectResourceLifetime::Frame,
            topology: EffectGraphTopology::LinearChain,
        };
        let temporal = temporal_contract(offset, EffectRoiPropagation::PixelLocal);
        let aggregate = current.compose(temporal).expect("composed contracts");
        let graph = EffectRenderGraph {
            nodes: vec![
                EffectGraphNode {
                    id: EffectGraphNodeId(0),
                    kind: EffectGraphNodeKind::Source,
                },
                EffectGraphNode {
                    id: EffectGraphNodeId(1),
                    kind: EffectGraphNodeKind::UnaryEffect {
                        input: EffectGraphNodeId(0),
                        op: crate::EffectRenderOp::ColorAdjust {
                            exposure,
                            contrast: 1.0,
                            saturation: 1.0,
                            working_color_space: mondrian_core::WorkingColorSpace::LinearRec709,
                        },
                    },
                },
                EffectGraphNode {
                    id: EffectGraphNodeId(2),
                    kind: EffectGraphNodeKind::UnaryEffect {
                        input: EffectGraphNodeId(1),
                        op: crate::EffectRenderOp::TemporalFrameBlend {
                            sample_offset: past_offset(offset),
                            mix: 0.5,
                        },
                    },
                },
            ],
            output: Some(EffectGraphNodeId(2)),
        };
        prepare_effect_graph_topology(&graph)
            .expect("valid topology")
            .bind_with_execution_bindings(
                graph,
                EffectExecutionEnvelope::new(aggregate, Arc::from([current, temporal])),
                Arc::from([
                    crate::graph::CompiledEffectStageBinding::new(
                        0,
                        current,
                        EffectGraphNodeId(0),
                        EffectGraphNodeId(1),
                        Arc::from([EffectGraphNodeId(1)]),
                    ),
                    crate::graph::CompiledEffectStageBinding::new(
                        1,
                        temporal,
                        EffectGraphNodeId(1),
                        EffectGraphNodeId(2),
                        Arc::from([EffectGraphNodeId(2)]),
                    ),
                ]),
            )
            .expect("bound effected temporal graph")
    }

    fn temporal_contract(
        past: TimelineTime,
        roi_propagation: EffectRoiPropagation,
    ) -> EffectExecutionContract {
        EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            determinism: EffectDeterminism::Deterministic,
            state_model: EffectStateModel::Stateless,
            temporal_input: EffectTemporalInputExtent {
                past: EffectTemporalSpan::Finite(past),
                future: EffectTemporalSpan::None,
            },
            roi_propagation,
            resource_lifetime: EffectResourceLifetime::Frame,
            topology: EffectGraphTopology::LinearChain,
        }
    }

    fn past_offset(duration: TimelineTime) -> TimelineTime {
        TimelineTime::ZERO.checked_sub(duration).expect("past offset")
    }

    fn dag_contract(past: EffectTemporalSpan) -> EffectExecutionContract {
        dag_contract_with_extent(EffectTemporalInputExtent {
            past,
            future: EffectTemporalSpan::None,
        })
    }

    fn dag_contract_with_extent(
        temporal_input: EffectTemporalInputExtent,
    ) -> EffectExecutionContract {
        EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            determinism: EffectDeterminism::Deterministic,
            state_model: EffectStateModel::Stateless,
            temporal_input,
            roi_propagation: EffectRoiPropagation::PixelLocal,
            resource_lifetime: EffectResourceLifetime::Frame,
            topology: EffectGraphTopology::GeneralDag,
        }
    }

    #[test]
    fn finite_past_processor_fetches_exact_signed_owner_domain_times() {
        let offset = TimelineTime::new(1, 2).expect("offset");
        let graph = bind_linear(
            [crate::EffectRenderOp::TemporalFrameBlend {
                sample_offset: past_offset(offset),
                mix: 0.25,
            }],
            temporal_contract(offset, EffectRoiPropagation::PixelLocal),
        );
        let extent = EffectFrameExtent::new(4, 2);
        let mut provider = GradientProvider::new(extent);
        let mut session = EffectExecutionSession::new(EffectExecutionSessionConfig {
            max_cache_entries: 4,
            max_cache_bytes: 1024 * 1024,
            max_working_bytes: 1024 * 1024,
            max_gpu_plan_entries: 0,
            max_gpu_plan_bytes: 0,
        });
        let request = EffectTemporalExecutionRequest::new(
            9,
            EffectExecutionContinuity::Discontinuous,
            TimelineTime::new(1, 4).expect("time"),
            extent,
            EffectPixelRoi::new(1, 0, 2, 1),
            ExecutionCancellationToken::new(),
        );
        let output = session
            .execute_temporal_roi_f32(&graph, &request, &mut provider)
            .expect("temporal output");
        assert_eq!(provider.requests.len(), 2);
        assert_eq!(
            provider.requests[0].time(),
            TimelineTime::new(1, 4).expect("time")
        );
        assert_eq!(
            provider.requests[1].time(),
            TimelineTime::new(-1, 4).expect("signed past time")
        );
        let current = GradientProvider::pixel(request.output_time(), 1, 0);
        let past =
            GradientProvider::pixel(TimelineTime::new(-1, 4).expect("signed past time"), 1, 0);
        assert_eq!(
            output.tile().pixels()[0],
            mix_straight_rgba(current, past, 0.25)
        );
    }

    #[test]
    fn finite_future_processor_fetches_exact_signed_owner_domain_times() {
        let offset = TimelineTime::new(1, 3).expect("offset");
        let graph = bind_linear(
            [crate::EffectRenderOp::TemporalFrameBlend { sample_offset: offset, mix: 0.25 }],
            EffectExecutionContract {
                temporal_input: EffectTemporalInputExtent {
                    past: EffectTemporalSpan::None,
                    future: EffectTemporalSpan::Finite(offset),
                },
                ..temporal_contract(TimelineTime::ZERO, EffectRoiPropagation::PixelLocal)
            },
        );
        let extent = EffectFrameExtent::new(4, 2);
        let mut provider = GradientProvider::new(extent);
        let mut session = EffectExecutionSession::default();
        let request = EffectTemporalExecutionRequest::new(
            10,
            EffectExecutionContinuity::Discontinuous,
            TimelineTime::new(1, 4).expect("time"),
            extent,
            EffectPixelRoi::new(1, 0, 2, 1),
            ExecutionCancellationToken::new(),
        );
        let demands = collect_temporal_frame_demands(&graph, &request).expect("future demands");
        assert_eq!(
            demands.requests().iter().map(|demand| demand.time()).collect::<Vec<_>>(),
            vec![
                TimelineTime::new(1, 4).expect("time"),
                TimelineTime::new(7, 12).expect("future time"),
            ]
        );

        let output = session
            .execute_temporal_roi_f32(&graph, &request, &mut provider)
            .expect("future temporal output");
        assert_eq!(output.provider_requests(), 2);
        assert_eq!(provider.requests.len(), 2);
        let current = GradientProvider::pixel(request.output_time(), 1, 0);
        let future = GradientProvider::pixel(TimelineTime::new(7, 12).expect("future time"), 1, 0);
        assert_eq!(
            output.tile().pixels()[0],
            mix_straight_rgba(current, future, 0.25)
        );
    }

    #[test]
    fn signed_multi_tap_program_deduplicates_samples_and_matches_tiled_execution() {
        let past = TimelineTime::new(1, 2).expect("past");
        let future = TimelineTime::new(1, 4).expect("future");
        let graph = bind_graph(
            EffectRenderGraph {
                nodes: vec![
                    EffectGraphNode {
                        id: EffectGraphNodeId(0),
                        kind: EffectGraphNodeKind::Source,
                    },
                    EffectGraphNode {
                        id: EffectGraphNodeId(1),
                        kind: EffectGraphNodeKind::UnaryEffect {
                            input: EffectGraphNodeId(0),
                            op: crate::EffectRenderOp::TemporalFrameBlend {
                                sample_offset: past_offset(past),
                                mix: 0.25,
                            },
                        },
                    },
                    EffectGraphNode {
                        id: EffectGraphNodeId(2),
                        kind: EffectGraphNodeKind::UnaryEffect {
                            input: EffectGraphNodeId(0),
                            op: crate::EffectRenderOp::TemporalFrameBlend {
                                sample_offset: future,
                                mix: 0.5,
                            },
                        },
                    },
                    EffectGraphNode {
                        id: EffectGraphNodeId(3),
                        kind: EffectGraphNodeKind::UnaryEffect {
                            input: EffectGraphNodeId(0),
                            op: crate::EffectRenderOp::TemporalFrameBlend {
                                sample_offset: future,
                                mix: 0.75,
                            },
                        },
                    },
                    EffectGraphNode {
                        id: EffectGraphNodeId(4),
                        kind: EffectGraphNodeKind::MultiInput {
                            inputs: vec![
                                EffectGraphNodeId(1),
                                EffectGraphNodeId(2),
                                EffectGraphNodeId(3),
                            ],
                            blend_mode: BlendMode::Normal,
                            opacity: 0.25,
                        },
                    },
                ],
                output: Some(EffectGraphNodeId(4)),
            },
            dag_contract_with_extent(EffectTemporalInputExtent {
                past: EffectTemporalSpan::Finite(past),
                future: EffectTemporalSpan::Finite(future),
            }),
        );
        let extent = EffectFrameExtent::new(17, 11);
        let output_time = TimelineTime::new(3, 2).expect("output time");
        let request = EffectTemporalExecutionRequest::new(
            11,
            EffectExecutionContinuity::Continuous,
            output_time,
            extent,
            extent.full_frame_roi(),
            ExecutionCancellationToken::new(),
        );
        let demands = collect_temporal_frame_demands(&graph, &request).expect("multi-tap demands");
        assert_eq!(
            demands.requests().iter().map(|demand| demand.time()).collect::<Vec<_>>(),
            vec![
                output_time,
                TimelineTime::ONE,
                TimelineTime::new(7, 4).expect("future time"),
            ],
            "the provider contract preserves schedule order and de-duplicates exact times"
        );

        let mut direct_session = EffectExecutionSession::default();
        let mut direct_provider = GradientProvider::new(extent);
        let direct = direct_session
            .execute_temporal_f32(&graph, &request, &mut direct_provider)
            .expect("direct multi-tap output");
        assert_eq!(direct.provider_requests(), 3);
        assert_eq!(direct_provider.requests.len(), 3);

        let current = GradientProvider::pixel(output_time, 0, 0);
        let past_pixel = GradientProvider::pixel(TimelineTime::ONE, 0, 0);
        let future_pixel =
            GradientProvider::pixel(TimelineTime::new(7, 4).expect("future time"), 0, 0);
        let past_value = mix_straight_rgba(current, past_pixel, 0.25);
        let future_half = mix_straight_rgba(current, future_pixel, 0.5);
        let future_three_quarters = mix_straight_rgba(current, future_pixel, 0.75);
        let expected = crate::adjustment::blend_rgba_f32_pixel(
            crate::adjustment::blend_rgba_f32_pixel(
                past_value,
                future_half,
                0.25,
                BlendMode::Normal,
            ),
            future_three_quarters,
            0.25,
            BlendMode::Normal,
        );
        assert_eq!(direct.tile().pixels()[0], expected);

        let output_bytes = std::mem::size_of_val(direct.tile().pixels());
        let budget = output_bytes + 512;
        let mut tiled_session =
            EffectExecutionSession::new(EffectExecutionSessionConfig::uncached(budget));
        let mut tiled_provider = GradientProvider::new(extent);
        let tiled = tiled_session
            .execute_temporal_f32(&graph, &request, &mut tiled_provider)
            .expect("tiled multi-tap output");
        assert!(tiled.execution_tiles() > 1);
        assert!(tiled.peak_working_bytes() <= budget);
        assert_eq!(tiled.tile().pixels(), direct.tile().pixels());
        assert_eq!(tiled.provider_requests(), tiled.execution_tiles() * 3);
    }

    #[test]
    fn frozen_batch_collects_once_and_is_the_only_execution_provider() {
        let offset = TimelineTime::new(1, 2).expect("offset");
        let graph = bind_linear(
            [
                crate::EffectRenderOp::TemporalFrameBlend {
                    sample_offset: past_offset(offset),
                    mix: 0.25,
                },
                crate::EffectRenderOp::ColorAdjust {
                    exposure: 0.0,
                    contrast: 1.0,
                    saturation: 1.0,
                    working_color_space: mondrian_core::WorkingColorSpace::LinearRec709,
                },
            ],
            temporal_contract(offset, EffectRoiPropagation::PixelLocal),
        );
        let extent = EffectFrameExtent::new(4, 2);
        let request = EffectTemporalExecutionRequest::new(
            19,
            EffectExecutionContinuity::Discontinuous,
            TimelineTime::new(3, 2).expect("time"),
            extent,
            extent.full_frame_roi(),
            ExecutionCancellationToken::new(),
        )
        .with_output_frame_seed(1234);
        let batch = collect_temporal_frame_demands(&graph, &request).expect("collect demands");
        assert_eq!(batch.generation(), 19);
        assert_eq!(
            batch.requests().iter().map(|request| request.time()).collect::<Vec<_>>(),
            vec![TimelineTime::new(3, 2).expect("time"), TimelineTime::ONE]
        );

        let identity = EffectTemporalSourceIdentity::from_complete_semantic_fingerprint([11; 32]);
        let resolved = batch
            .requests()
            .iter()
            .copied()
            .map(|demand| {
                let roi = demand.input_roi().region();
                let pixels = (0..checked_pixel_count(roi).expect("pixel count"))
                    .map(|_| [demand.time().to_f64() as f32, 0.0, 0.0, 1.0])
                    .collect::<Vec<_>>();
                let tile = EffectFrameTileF32::new(
                    demand.time(),
                    demand.frame_extent(),
                    roi,
                    demand.time().numerator(),
                    pixels,
                )
                .expect("tile");
                (demand, tile)
            })
            .collect::<Vec<_>>();
        let mut frozen =
            PreparedTemporalFrameSet::prepare(identity, batch, resolved).expect("freeze batch");
        let mut session = EffectExecutionSession::default();
        let output = session
            .execute_temporal_roi_f32(&graph, &request, &mut frozen)
            .expect("execute only from frozen frames");
        assert_eq!(output.provider_requests(), 2);
        assert_eq!(frozen.len(), 2);
        assert_eq!(output.tile().frame_seed(), 1234);
        assert_eq!(output.tile().pixels()[0], [1.375, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn demand_collection_rejects_effects_before_temporal_blend() {
        let offset = TimelineTime::new(1, 2).expect("offset");
        let graph = bind_linear(
            [
                crate::EffectRenderOp::ColorAdjust {
                    exposure: 0.0,
                    contrast: 1.0,
                    saturation: 1.0,
                    working_color_space: mondrian_core::WorkingColorSpace::LinearRec709,
                },
                crate::EffectRenderOp::TemporalFrameBlend {
                    sample_offset: past_offset(offset),
                    mix: 0.5,
                },
            ],
            temporal_contract(offset, EffectRoiPropagation::PixelLocal),
        );
        let extent = EffectFrameExtent::new(2, 2);
        let request = EffectTemporalExecutionRequest::new(
            1,
            EffectExecutionContinuity::Continuous,
            TimelineTime::ONE,
            extent,
            extent.full_frame_roi(),
            ExecutionCancellationToken::new(),
        );
        assert!(matches!(
            collect_temporal_frame_demands(&graph, &request),
            Err(EffectTemporalExecutionError::UnsupportedTemporalShape { .. })
        ));
    }

    #[test]
    fn prepared_temporal_execution_reevaluates_upstream_effects_at_sample_time() {
        let offset = TimelineTime::new(1, 2).expect("offset");
        let root = bind_effected_temporal_graph(1.0, offset);
        let sampled = bind_effected_temporal_graph(-1.0, offset);
        let extent = EffectFrameExtent::new(1, 1);
        let request = EffectTemporalExecutionRequest::new(
            77,
            EffectExecutionContinuity::Discontinuous,
            TimelineTime::ONE,
            extent,
            extent.full_frame_roi(),
            ExecutionCancellationToken::new(),
        )
        .with_output_frame_seed(24);
        let mut evaluate_sample = |time| {
            assert_eq!(time, TimelineTime::new(1, 2).expect("sample time"));
            Ok((Arc::clone(&sampled), 12))
        };
        let prepared = prepare_temporal_execution_inner(
            Arc::clone(&root),
            &request,
            Some(&mut evaluate_sample),
        )
        .expect("time-expanded effected execution");
        ensure_temporal_program(&prepared.program).expect("finite temporal program");
        assert_eq!(
            prepared
                .demands()
                .requests()
                .iter()
                .map(|request| request.time())
                .collect::<Vec<_>>(),
            vec![
                TimelineTime::ONE,
                TimelineTime::new(1, 2).expect("sample time")
            ]
        );
        let identity = EffectTemporalSourceIdentity::from_complete_semantic_fingerprint([31; 32]);
        let resolved = prepared
            .demands()
            .requests()
            .iter()
            .copied()
            .map(|demand| {
                let tile = EffectFrameTileF32::new(
                    demand.time(),
                    demand.frame_extent(),
                    demand.input_roi().region(),
                    0,
                    vec![[demand.time().to_f64() as f32, 0.0, 0.0, 1.0]],
                )
                .expect("resolved source");
                (demand, tile)
            })
            .collect::<Vec<_>>();
        let mut frozen =
            PreparedTemporalFrameSet::prepare(identity, prepared.demands().clone(), resolved)
                .expect("frozen sources");
        let mut session = EffectExecutionSession::default();
        let output = session
            .execute_prepared_temporal_f32(&prepared, &mut frozen)
            .expect("effected temporal output");
        assert_eq!(output.tile().pixels(), &[[1.125, 0.0, 0.0, 1.0]]);
        assert_eq!(output.tile().frame_seed(), 24);
    }

    #[test]
    fn temporal_mask_dag_matches_full_frame_reference_in_direct_and_tiled_execution() {
        let offset = TimelineTime::new(1, 2).expect("offset");
        let mask_shape = crate::mask::MaskShape::Path {
            points: vec![
                crate::mask::BezierPoint::new(glam::Vec2::new(0.12, 0.15)),
                crate::mask::BezierPoint::new(glam::Vec2::new(0.86, 0.2)),
                crate::mask::BezierPoint::new(glam::Vec2::new(0.72, 0.84)),
                crate::mask::BezierPoint::new(glam::Vec2::new(0.18, 0.76)),
            ],
            closed: true,
        };
        let graph = bind_graph(
            EffectRenderGraph {
                nodes: vec![
                    EffectGraphNode {
                        id: EffectGraphNodeId(0),
                        kind: EffectGraphNodeKind::Source,
                    },
                    EffectGraphNode {
                        id: EffectGraphNodeId(1),
                        kind: EffectGraphNodeKind::UnaryEffect {
                            input: EffectGraphNodeId(0),
                            op: crate::EffectRenderOp::TemporalFrameBlend {
                                sample_offset: past_offset(offset),
                                mix: 0.5,
                            },
                        },
                    },
                    EffectGraphNode {
                        id: EffectGraphNodeId(2),
                        kind: EffectGraphNodeKind::MaskSource {
                            shape: mask_shape.clone(),
                            feather: 2.5,
                            expansion: 1.0,
                            opacity: 0.8,
                        },
                    },
                    EffectGraphNode {
                        id: EffectGraphNodeId(3),
                        kind: EffectGraphNodeKind::Mask {
                            input: EffectGraphNodeId(1),
                            mask: EffectGraphNodeId(2),
                            invert: false,
                            mask_op: crate::mask::MaskOp::Add,
                        },
                    },
                ],
                output: Some(EffectGraphNodeId(3)),
            },
            dag_contract(EffectTemporalSpan::Finite(offset)),
        );
        let extent = EffectFrameExtent::new(8, 4);
        let output_time = TimelineTime::ONE;
        let partial_roi = EffectPixelRoi::new(2, 1, 3, 2);
        let request = EffectTemporalExecutionRequest::new(
            1,
            EffectExecutionContinuity::Continuous,
            output_time,
            extent,
            partial_roi,
            ExecutionCancellationToken::new(),
        );
        let demands = collect_temporal_frame_demands(&graph, &request).expect("Mask demands");
        assert_eq!(demands.requests()[0].input_roi().region(), partial_roi);

        let reference_graph = bind_graph(
            EffectRenderGraph {
                nodes: vec![
                    EffectGraphNode {
                        id: EffectGraphNodeId(0),
                        kind: EffectGraphNodeKind::Source,
                    },
                    EffectGraphNode {
                        id: EffectGraphNodeId(1),
                        kind: EffectGraphNodeKind::MaskSource {
                            shape: mask_shape,
                            feather: 2.5,
                            expansion: 1.0,
                            opacity: 0.8,
                        },
                    },
                    EffectGraphNode {
                        id: EffectGraphNodeId(2),
                        kind: EffectGraphNodeKind::Mask {
                            input: EffectGraphNodeId(0),
                            mask: EffectGraphNodeId(1),
                            invert: false,
                            mask_op: crate::mask::MaskOp::Add,
                        },
                    },
                ],
                output: Some(EffectGraphNodeId(2)),
            },
            dag_contract(EffectTemporalSpan::None),
        );
        let past_time = output_time.checked_sub(offset).expect("past time");
        let mixed = (0..extent.height())
            .flat_map(|y| {
                (0..extent.width()).map(move |x| {
                    let current = GradientProvider::pixel(output_time, x, y);
                    let past = GradientProvider::pixel(past_time, x, y);
                    [
                        (current[0] + past[0]) * 0.5,
                        (current[1] + past[1]) * 0.5,
                        (current[2] + past[2]) * 0.5,
                        1.0,
                    ]
                })
            })
            .collect::<Vec<_>>();
        let reference = apply_compiled_effect_graph_rgba_f32(
            &mixed,
            extent.width(),
            extent.height(),
            &reference_graph,
            0,
        )
        .expect("full-frame Mask reference");

        let mut direct_session = EffectExecutionSession::default();
        let mut direct_provider = GradientProvider::new(extent);
        let direct = direct_session
            .execute_temporal_roi_f32(&graph, &request, &mut direct_provider)
            .expect("direct Mask tile");
        assert_eq!(
            direct.tile().pixels(),
            crop_frame(
                &reference,
                extent,
                partial_roi,
                &ExecutionCancellationToken::new(),
            )
            .expect("reference crop")
        );

        let full_request = EffectTemporalExecutionRequest::new(
            2,
            EffectExecutionContinuity::Continuous,
            output_time,
            extent,
            extent.full_frame_roi(),
            ExecutionCancellationToken::new(),
        );
        let mask_rasters =
            PreparedMaskRasterSet::prepare(graph.graph(), extent, &full_request.cancellation)
                .expect("prepared Mask geometry");
        let insufficient_budget = mask_rasters.retained_bytes() - 1;
        let mut rejected_session = EffectExecutionSession::new(
            EffectExecutionSessionConfig::uncached(insufficient_budget),
        );
        let mut rejected_provider = GradientProvider::new(extent);
        assert!(matches!(
            rejected_session.execute_temporal_f32(
                &graph,
                &full_request,
                &mut rejected_provider,
            ),
            Err(EffectTemporalExecutionError::WorkingSetBudgetExceeded {
                required_bytes,
                budget_bytes,
            }) if required_bytes == mask_rasters.retained_bytes()
                && budget_bytes == insufficient_budget
        ));
        let output_bytes = reference.len() * std::mem::size_of::<[f32; 4]>();
        let budget = mask_rasters.retained_bytes() + output_bytes + 256;
        let mut tiled_session =
            EffectExecutionSession::new(EffectExecutionSessionConfig::uncached(budget));
        let mut tiled_provider = GradientProvider::new(extent);
        let tiled = tiled_session
            .execute_temporal_f32(&graph, &full_request, &mut tiled_provider)
            .expect("tiled Mask execution");
        assert!(tiled.execution_tiles() > 1);
        assert!(tiled.peak_working_bytes() <= budget);
        assert_eq!(tiled.tile().pixels(), reference);
    }

    #[test]
    fn current_time_dag_after_history_matches_reference_and_exact_live_set() {
        let offset = TimelineTime::new(1, 2).expect("offset");
        let color = |input, exposure| EffectGraphNodeKind::UnaryEffect {
            input,
            op: crate::EffectRenderOp::ColorAdjust {
                exposure,
                contrast: 1.0,
                saturation: 1.0,
                working_color_space: mondrian_core::WorkingColorSpace::LinearRec709,
            },
        };
        let vignette = |input| EffectGraphNodeKind::UnaryEffect {
            input,
            op: crate::EffectRenderOp::Vignette { intensity: 0.63, feather: 0.37 },
        };
        let temporal_graph = bind_graph(
            EffectRenderGraph {
                nodes: vec![
                    EffectGraphNode {
                        id: EffectGraphNodeId(0),
                        kind: EffectGraphNodeKind::Source,
                    },
                    EffectGraphNode {
                        id: EffectGraphNodeId(1),
                        kind: EffectGraphNodeKind::UnaryEffect {
                            input: EffectGraphNodeId(0),
                            op: crate::EffectRenderOp::TemporalFrameBlend {
                                sample_offset: past_offset(offset),
                                mix: 0.35,
                            },
                        },
                    },
                    EffectGraphNode {
                        id: EffectGraphNodeId(2),
                        kind: color(EffectGraphNodeId(1), 0.4),
                    },
                    EffectGraphNode {
                        id: EffectGraphNodeId(3),
                        kind: vignette(EffectGraphNodeId(1)),
                    },
                    EffectGraphNode {
                        id: EffectGraphNodeId(4),
                        kind: EffectGraphNodeKind::Blend {
                            base: EffectGraphNodeId(2),
                            overlay: EffectGraphNodeId(3),
                            blend_mode: BlendMode::Dissolve,
                            opacity: 0.45,
                        },
                    },
                    EffectGraphNode {
                        id: EffectGraphNodeId(5),
                        kind: color(EffectGraphNodeId(1), -0.2),
                    },
                    EffectGraphNode {
                        id: EffectGraphNodeId(6),
                        kind: EffectGraphNodeKind::MultiInput {
                            inputs: vec![EffectGraphNodeId(4), EffectGraphNodeId(5)],
                            blend_mode: BlendMode::Normal,
                            opacity: 0.25,
                        },
                    },
                ],
                output: Some(EffectGraphNodeId(6)),
            },
            dag_contract(EffectTemporalSpan::Finite(offset)),
        );
        let reference_graph = bind_graph(
            EffectRenderGraph {
                nodes: vec![
                    EffectGraphNode {
                        id: EffectGraphNodeId(0),
                        kind: EffectGraphNodeKind::Source,
                    },
                    EffectGraphNode {
                        id: EffectGraphNodeId(1),
                        kind: color(EffectGraphNodeId(0), 0.4),
                    },
                    EffectGraphNode {
                        id: EffectGraphNodeId(2),
                        kind: vignette(EffectGraphNodeId(0)),
                    },
                    EffectGraphNode {
                        id: EffectGraphNodeId(3),
                        kind: EffectGraphNodeKind::Blend {
                            base: EffectGraphNodeId(1),
                            overlay: EffectGraphNodeId(2),
                            blend_mode: BlendMode::Dissolve,
                            opacity: 0.45,
                        },
                    },
                    EffectGraphNode {
                        id: EffectGraphNodeId(4),
                        kind: color(EffectGraphNodeId(0), -0.2),
                    },
                    EffectGraphNode {
                        id: EffectGraphNodeId(5),
                        kind: EffectGraphNodeKind::MultiInput {
                            inputs: vec![EffectGraphNodeId(3), EffectGraphNodeId(4)],
                            blend_mode: BlendMode::Normal,
                            opacity: 0.25,
                        },
                    },
                ],
                output: Some(EffectGraphNodeId(5)),
            },
            dag_contract(EffectTemporalSpan::None),
        );

        let extent = EffectFrameExtent::new(17, 11);
        let output_roi = EffectPixelRoi::new(7, 4, 5, 3);
        let output_time = TimelineTime::new(5, 4).expect("output time");
        let past_time = output_time.checked_sub(offset).expect("past time");
        let frame_seed = 0x51a7_i64;
        let mixed = (0..extent.height())
            .flat_map(|y| {
                (0..extent.width()).map(move |x| {
                    mix_straight_rgba(
                        GradientProvider::pixel(output_time, x, y),
                        GradientProvider::pixel(past_time, x, y),
                        0.35,
                    )
                })
            })
            .collect::<Vec<_>>();
        let reference = apply_compiled_effect_graph_rgba_f32(
            &mixed,
            extent.width(),
            extent.height(),
            &reference_graph,
            frame_seed,
        )
        .expect("current-frame DAG reference");
        let expected = crop_frame(
            &reference,
            extent,
            output_roi,
            &ExecutionCancellationToken::new(),
        )
        .expect("reference crop");

        let tile_bytes =
            checked_pixel_count(output_roi).expect("tile pixels") * std::mem::size_of::<[f32; 4]>();
        let exact_peak = tile_bytes * 3;
        let request = EffectTemporalExecutionRequest::new(
            31,
            EffectExecutionContinuity::Continuous,
            output_time,
            extent,
            output_roi,
            ExecutionCancellationToken::new(),
        )
        .with_output_frame_seed(frame_seed);
        let mut session = EffectExecutionSession::new(EffectExecutionSessionConfig {
            max_cache_entries: 0,
            max_cache_bytes: 0,
            max_working_bytes: exact_peak,
            max_gpu_plan_entries: 0,
            max_gpu_plan_bytes: 0,
        });
        let mut provider = GradientProvider::new(extent);
        let output = session
            .execute_temporal_roi_f32(&temporal_graph, &request, &mut provider)
            .expect("temporal DAG");
        assert_eq!(output.tile().pixels(), expected);
        assert_eq!(output.provider_requests(), 2);
        assert_eq!(output.peak_working_bytes(), exact_peak);

        let mut rejected = EffectExecutionSession::new(EffectExecutionSessionConfig {
            max_cache_entries: 0,
            max_cache_bytes: 0,
            max_working_bytes: exact_peak - 1,
            max_gpu_plan_entries: 0,
            max_gpu_plan_bytes: 0,
        });
        let mut rejected_provider = GradientProvider::new(extent);
        assert_eq!(
            rejected
                .execute_temporal_roi_f32(&temporal_graph, &request, &mut rejected_provider)
                .expect_err("one byte below exact live-set peak must fail"),
            EffectTemporalExecutionError::WorkingSetBudgetExceeded {
                required_bytes: exact_peak,
                budget_bytes: exact_peak - 1,
            }
        );

        let full_request = EffectTemporalExecutionRequest::new(
            32,
            EffectExecutionContinuity::Continuous,
            output_time,
            extent,
            extent.full_frame_roi(),
            ExecutionCancellationToken::new(),
        )
        .with_output_frame_seed(frame_seed);
        let full_demands =
            collect_temporal_frame_demands(&temporal_graph, &full_request).expect("full demands");
        let source_coverage_bytes = full_demands.coverage_bytes();
        let resolved = full_demands
            .requests()
            .iter()
            .copied()
            .map(|demand| {
                let roi = demand.input_roi().region();
                let pixels = (roi.y()..roi.y() + roi.height())
                    .flat_map(|y| {
                        (roi.x()..roi.x() + roi.width())
                            .map(move |x| GradientProvider::pixel(demand.time(), x, y))
                    })
                    .collect::<Vec<_>>();
                let tile = EffectFrameTileF32::new(
                    demand.time(),
                    demand.frame_extent(),
                    roi,
                    demand.time().numerator(),
                    pixels,
                )
                .expect("full source tile");
                (demand, tile)
            })
            .collect::<Vec<_>>();
        let frozen = PreparedTemporalFrameSet::prepare(
            EffectTemporalSourceIdentity::from_complete_semantic_fingerprint([23; 32]),
            full_demands,
            resolved,
        )
        .expect("full frozen source coverage");
        let full_output_bytes = reference.len() * std::mem::size_of::<[f32; 4]>();
        let tiled_budget = source_coverage_bytes + full_output_bytes + 3_504;
        assert!(tiled_budget < source_coverage_bytes + full_output_bytes * 3);
        let mut tiled_session = EffectExecutionSession::new(EffectExecutionSessionConfig {
            max_cache_entries: 0,
            max_cache_bytes: 0,
            max_working_bytes: tiled_budget,
            max_gpu_plan_entries: 0,
            max_gpu_plan_bytes: 0,
        });
        let mut tiled_provider = frozen.clone();
        let tiled = tiled_session
            .execute_temporal_f32(&temporal_graph, &full_request, &mut tiled_provider)
            .expect("budget-driven temporal tiles");
        assert_eq!(tiled.tile().pixels(), reference);
        assert_eq!(tiled.execution_tiles(), 4);
        assert_eq!(tiled.provider_requests(), tiled.execution_tiles() * 2);
        let largest_tile_peak = 9 * 6 * std::mem::size_of::<[f32; 4]>() * 3;
        assert_eq!(
            tiled.peak_working_bytes(),
            source_coverage_bytes + full_output_bytes + largest_tile_peak
        );

        let mut cached_session = EffectExecutionSession::new(EffectExecutionSessionConfig {
            max_cache_entries: 16,
            max_cache_bytes: 1024 * 1024,
            max_working_bytes: tiled_budget,
            max_gpu_plan_entries: 0,
            max_gpu_plan_bytes: 0,
        });
        let mut cached_provider = frozen.clone();
        let first_cached = cached_session
            .execute_temporal_f32(&temporal_graph, &full_request, &mut cached_provider)
            .expect("first tiled cache population");
        assert_eq!(first_cached.execution_tiles(), 4);
        let second_cached = cached_session
            .execute_temporal_f32(&temporal_graph, &full_request, &mut cached_provider)
            .expect("assembled output cache hit");
        assert_eq!(second_cached.tile().pixels(), reference);
        assert_eq!(second_cached.execution_tiles(), 0);
        assert_eq!(second_cached.provider_requests(), 0);
        assert_eq!(second_cached.peak_working_bytes(), 0);

        let publication_budget = source_coverage_bytes + full_output_bytes - 1;
        let mut publication_rejected = EffectExecutionSession::new(EffectExecutionSessionConfig {
            max_cache_entries: 0,
            max_cache_bytes: 0,
            max_working_bytes: publication_budget,
            max_gpu_plan_entries: 0,
            max_gpu_plan_bytes: 0,
        });
        let mut rejected_frozen = frozen;
        assert_eq!(
            publication_rejected
                .execute_temporal_f32(&temporal_graph, &full_request, &mut rejected_frozen,)
                .expect_err("full output publication must fit the shared grant"),
            EffectTemporalExecutionError::WorkingSetBudgetExceeded {
                required_bytes: source_coverage_bytes + full_output_bytes,
                budget_bytes: publication_budget,
            }
        );
    }

    #[test]
    fn tiled_temporal_cancellation_never_publishes_a_partial_output() {
        struct CancelingProvider {
            inner: GradientProvider,
            cancellation: ExecutionCancellationToken,
            cancel_after: usize,
        }

        impl EffectTemporalFrameProvider for CancelingProvider {
            fn source_identity(&self) -> EffectTemporalSourceIdentity {
                self.inner.source_identity()
            }

            fn retained_coverage_bytes(&self) -> usize {
                self.inner.retained_coverage_bytes()
            }

            fn copy_frame(
                &mut self,
                request: EffectTemporalFrameRequest,
                cancellation: &ExecutionCancellationToken,
                destination: &mut Vec<[f32; 4]>,
            ) -> Result<(), EffectTemporalFrameProviderError> {
                self.inner.copy_frame(request, cancellation, destination)?;
                if self.inner.requests.len() == self.cancel_after {
                    self.cancellation.cancel();
                }
                Ok(())
            }
        }

        let offset = TimelineTime::new(1, 2).expect("offset");
        let graph = bind_linear(
            [crate::EffectRenderOp::TemporalFrameBlend {
                sample_offset: past_offset(offset),
                mix: 0.5,
            }],
            temporal_contract(offset, EffectRoiPropagation::PixelLocal),
        );
        let extent = EffectFrameExtent::new(17, 11);
        let frame_bytes = 17 * 11 * std::mem::size_of::<[f32; 4]>();
        let budget = frame_bytes + 512;
        let cancellation = ExecutionCancellationToken::new();
        let request = EffectTemporalExecutionRequest::new(
            41,
            EffectExecutionContinuity::Continuous,
            TimelineTime::ONE,
            extent,
            extent.full_frame_roi(),
            cancellation.clone(),
        );
        let mut provider = CancelingProvider {
            inner: GradientProvider::new(extent),
            cancellation,
            cancel_after: 3,
        };
        let mut session = EffectExecutionSession::new(EffectExecutionSessionConfig {
            max_cache_entries: 16,
            max_cache_bytes: 4 * 1024 * 1024,
            max_working_bytes: budget,
            max_gpu_plan_entries: 0,
            max_gpu_plan_bytes: 0,
        });

        assert_eq!(
            session
                .execute_temporal_f32(&graph, &request, &mut provider)
                .expect_err("third source copy cancels the multi-tile attempt"),
            EffectTemporalExecutionError::Canceled
        );
        assert_eq!(provider.inner.requests.len(), 3);
        assert_eq!(session.diagnostics().cache_entries, 0);
    }

    #[test]
    fn excessive_temporal_tile_schedule_fails_before_source_copy() {
        let offset = TimelineTime::new(1, 2).expect("offset");
        let graph = bind_linear(
            [crate::EffectRenderOp::TemporalFrameBlend {
                sample_offset: past_offset(offset),
                mix: 0.5,
            }],
            temporal_contract(offset, EffectRoiPropagation::PixelLocal),
        );
        let extent = EffectFrameExtent::new(128, 128);
        let output_bytes = 128 * 128 * std::mem::size_of::<[f32; 4]>();
        let request = EffectTemporalExecutionRequest::new(
            42,
            EffectExecutionContinuity::Continuous,
            TimelineTime::ONE,
            extent,
            extent.full_frame_roi(),
            ExecutionCancellationToken::new(),
        );
        let mut provider = GradientProvider::new(extent);
        let mut session = EffectExecutionSession::new(EffectExecutionSessionConfig {
            max_cache_entries: 0,
            max_cache_bytes: 0,
            max_working_bytes: output_bytes + 2 * std::mem::size_of::<[f32; 4]>(),
            max_gpu_plan_entries: 0,
            max_gpu_plan_bytes: 0,
        });

        assert_eq!(
            session
                .execute_temporal_f32(&graph, &request, &mut provider)
                .expect_err("unsafe tile fan-out must fail during planning"),
            EffectTemporalExecutionError::TileScheduleLimitExceeded {
                limit: MAX_TEMPORAL_SCALAR_TILES,
            }
        );
        assert!(provider.requests.is_empty());
        assert_eq!(session.diagnostics().cache_entries, 0);
    }

    #[test]
    fn uhd_two_frame_temporal_plan_fits_the_standard_384_mib_grant() {
        let offset = TimelineTime::new(1, 24).expect("offset");
        let graph = bind_linear(
            [crate::EffectRenderOp::TemporalFrameBlend {
                sample_offset: past_offset(offset),
                mix: 0.5,
            }],
            temporal_contract(offset, EffectRoiPropagation::PixelLocal),
        );
        let extent = EffectFrameExtent::new(3_840, 2_160);
        let output_roi = extent.full_frame_roi();
        let request = EffectTemporalExecutionRequest::new(
            43,
            EffectExecutionContinuity::Continuous,
            TimelineTime::ONE,
            extent,
            output_roi,
            ExecutionCancellationToken::new(),
        );
        let prepared = prepare_temporal_execution_inner(Arc::clone(&graph), &request, None)
            .expect("admitted program");
        let temporal_program = Arc::clone(&prepared.program);
        let demand = graph
            .plan_execution_demand(request.output_time, extent, output_roi)
            .expect("full-frame demand");
        let mask_rasters = HashMap::new();
        let source_coverage_bytes = collect_temporal_frame_demands(&graph, &request)
            .expect("demands")
            .coverage_bytes();
        let output_bytes = 3_840 * 2_160 * std::mem::size_of::<[f32; 4]>();
        let base_resident_bytes = source_coverage_bytes + output_bytes;
        let budget = 384 * 1024 * 1024;
        let direct_working =
            temporal_scalar_required_bytes(&temporal_program, &demand, &mask_rasters)
                .expect("direct proof");
        assert!(base_resident_bytes <= budget);
        assert!(source_coverage_bytes + direct_working > budget);

        let tiles = plan_temporal_tiles(
            &graph,
            &request,
            &temporal_program,
            output_roi,
            base_resident_bytes,
            budget - base_resident_bytes,
            budget,
            &mask_rasters,
        )
        .expect("bounded UHD tile schedule");
        assert_eq!(tiles.len(), 64);
        assert!(tiles.iter().all(|tile| tile.width() == 480 && tile.height() == 270));
        assert_eq!(
            tiles
                .iter()
                .map(|tile| u64::from(tile.width()) * u64::from(tile.height()))
                .sum::<u64>(),
            u64::from(extent.width()) * u64::from(extent.height())
        );
        let tile_peak = 480 * 270 * std::mem::size_of::<[f32; 4]>() * 2;
        assert_eq!(base_resident_bytes + tile_peak, 402_278_400);
        assert!(base_resident_bytes + tile_peak <= budget);
    }

    #[test]
    fn frozen_batch_rejects_missing_and_stale_generation_tiles() {
        let offset = TimelineTime::new(1, 2).expect("offset");
        let graph = bind_linear(
            [crate::EffectRenderOp::TemporalFrameBlend {
                sample_offset: past_offset(offset),
                mix: 0.5,
            }],
            temporal_contract(offset, EffectRoiPropagation::PixelLocal),
        );
        let extent = EffectFrameExtent::new(2, 2);
        let request = EffectTemporalExecutionRequest::new(
            3,
            EffectExecutionContinuity::Continuous,
            TimelineTime::ONE,
            extent,
            extent.full_frame_roi(),
            ExecutionCancellationToken::new(),
        );
        let batch = collect_temporal_frame_demands(&graph, &request).expect("batch");
        assert_eq!(
            batch.coverage_bytes(),
            2 * 2 * 2 * std::mem::size_of::<[f32; 4]>()
        );
        assert!(matches!(
            PreparedTemporalFrameSet::prepare(
                EffectTemporalSourceIdentity::from_complete_semantic_fingerprint([4; 32]),
                batch.clone(),
                [],
            ),
            Err(PreparedTemporalFrameSetError::MissingRequest { .. })
        ));

        let mut stale = batch.requests()[0];
        stale.generation = 2;
        let roi = stale.input_roi().region();
        let tile = EffectFrameTileF32::new(
            stale.time(),
            stale.frame_extent(),
            roi,
            0,
            vec![[0.0; 4]; checked_pixel_count(roi).expect("pixel count")],
        )
        .expect("tile");
        assert!(matches!(
            PreparedTemporalFrameSet::prepare(
                EffectTemporalSourceIdentity::from_complete_semantic_fingerprint([4; 32]),
                batch,
                [(stale, tile)],
            ),
            Err(PreparedTemporalFrameSetError::GenerationMismatch { expected: 3, actual: 2 })
        ));
    }

    #[test]
    fn expanded_roi_scalar_reference_matches_full_frame_blur_crop() {
        let radius = 2.0;
        let contract = temporal_contract(
            TimelineTime::ZERO,
            EffectRoiPropagation::Expand { horizontal_pixels: 3, vertical_pixels: 3 },
        );
        let graph = bind_linear([crate::EffectRenderOp::GaussianBlur { radius }], contract);
        let extent = EffectFrameExtent::new(16, 12);
        let time = TimelineTime::new(3, 2).expect("time");
        let mut tiled_provider = GradientProvider::new(extent);
        let mut full_provider = GradientProvider::new(extent);
        let mut tiled_session = EffectExecutionSession::new(EffectExecutionSessionConfig {
            max_cache_entries: 0,
            max_cache_bytes: 0,
            max_working_bytes: 8 * 1024 * 1024,
            max_gpu_plan_entries: 0,
            max_gpu_plan_bytes: 0,
        });
        let mut full_session = EffectExecutionSession::new(EffectExecutionSessionConfig {
            max_cache_entries: 0,
            max_cache_bytes: 0,
            max_working_bytes: 8 * 1024 * 1024,
            max_gpu_plan_entries: 0,
            max_gpu_plan_bytes: 0,
        });
        let tile_roi = EffectPixelRoi::new(6, 4, 3, 3);
        let tiled = tiled_session
            .execute_temporal_roi_f32(
                &graph,
                &EffectTemporalExecutionRequest::new(
                    1,
                    EffectExecutionContinuity::Continuous,
                    time,
                    extent,
                    tile_roi,
                    ExecutionCancellationToken::new(),
                ),
                &mut tiled_provider,
            )
            .expect("tile");
        let full = full_session
            .execute_temporal_roi_f32(
                &graph,
                &EffectTemporalExecutionRequest::new(
                    1,
                    EffectExecutionContinuity::Continuous,
                    time,
                    extent,
                    extent.full_frame_roi(),
                    ExecutionCancellationToken::new(),
                ),
                &mut full_provider,
            )
            .expect("full");
        assert_eq!(
            tiled.tile().pixels(),
            crop_frame(
                full.tile().pixels(),
                extent,
                tile_roi,
                &ExecutionCancellationToken::new(),
            )
            .expect("reference crop")
        );
        let halo = tiled_provider.requests[0]
            .exact_halo()
            .expect("Gaussian blur must expose its exact finite halo");
        assert_eq!(halo.left(), 3);
        assert_eq!(halo.top(), 3);
        assert_eq!(halo.right(), 3);
        assert_eq!(halo.bottom(), 3);
    }

    #[test]
    fn pixel_local_coordinate_effects_match_full_frame_crop() {
        let mut contract = temporal_contract(TimelineTime::ZERO, EffectRoiPropagation::PixelLocal);
        contract.determinism = EffectDeterminism::FrameSeeded;
        let graph = bind_linear(
            [
                crate::EffectRenderOp::Vignette { intensity: 0.72, feather: 0.41 },
                crate::EffectRenderOp::Grain { amount: 0.37 },
            ],
            contract,
        );
        let extent = EffectFrameExtent::new(31, 19);
        let tile_roi = EffectPixelRoi::new(17, 9, 5, 4);
        let time = TimelineTime::new(11, 24).expect("time");
        let request = |roi| {
            EffectTemporalExecutionRequest::new(
                7,
                EffectExecutionContinuity::Continuous,
                time,
                extent,
                roi,
                ExecutionCancellationToken::new(),
            )
            .with_output_frame_seed(0x5a17)
        };
        let mut tile_provider = GradientProvider::new(extent);
        let mut full_provider = GradientProvider::new(extent);
        let mut tile_session = EffectExecutionSession::default();
        let mut full_session = EffectExecutionSession::default();

        let tile = tile_session
            .execute_temporal_roi_f32(&graph, &request(tile_roi), &mut tile_provider)
            .expect("coordinate-aware tile");
        let full = full_session
            .execute_temporal_roi_f32(
                &graph,
                &request(extent.full_frame_roi()),
                &mut full_provider,
            )
            .expect("full-frame reference");

        assert_eq!(
            tile.tile().pixels(),
            crop_frame(
                full.tile().pixels(),
                extent,
                tile_roi,
                &ExecutionCancellationToken::new(),
            )
            .expect("reference crop")
        );
    }

    #[test]
    fn exact_roi_working_set_scales_with_the_input_tile_and_accounts_for_kernel_scratch() {
        let radius = 2.0;
        let halo = crate::adjustment::gaussian_blur_input_halo(radius).expect("finite halo");
        let graph = bind_linear(
            [crate::EffectRenderOp::GaussianBlur { radius }],
            temporal_contract(
                TimelineTime::ZERO,
                EffectRoiPropagation::Expand { horizontal_pixels: halo, vertical_pixels: halo },
            ),
        );
        let extent = EffectFrameExtent::new(3_840, 2_160);
        let output_roi = EffectPixelRoi::new(1_000, 700, 8, 8);
        let input_width = output_roi.width() + halo * 2;
        let input_height = output_roi.height() + halo * 2;
        let tile_bytes =
            input_width as usize * input_height as usize * std::mem::size_of::<[f32; 4]>();
        // The source buffer becomes the unary output in place. Peak residency
        // is one retained tile plus either the provider tile during cloning or
        // Gaussian's same-sized scratch image.
        let exact_peak = tile_bytes * 2;
        let request = EffectTemporalExecutionRequest::new(
            23,
            EffectExecutionContinuity::Continuous,
            TimelineTime::ZERO,
            extent,
            output_roi,
            ExecutionCancellationToken::new(),
        );

        let mut admitted = EffectExecutionSession::new(EffectExecutionSessionConfig {
            max_cache_entries: 0,
            max_cache_bytes: 0,
            max_working_bytes: exact_peak,
            max_gpu_plan_entries: 0,
            max_gpu_plan_bytes: 0,
        });
        let mut provider = GradientProvider::new(extent);
        let output = admitted
            .execute_temporal_roi_f32(&graph, &request, &mut provider)
            .expect("tile-sized working budget");
        assert_eq!(output.peak_working_bytes(), exact_peak);
        assert!(
            output.peak_working_bytes()
                < extent.width() as usize
                    * extent.height() as usize
                    * std::mem::size_of::<[f32; 4]>(),
            "a small ROI must not materialize even one complete 4K working frame"
        );

        let mut rejected = EffectExecutionSession::new(EffectExecutionSessionConfig {
            max_cache_entries: 0,
            max_cache_bytes: 0,
            max_working_bytes: exact_peak - 1,
            max_gpu_plan_entries: 0,
            max_gpu_plan_bytes: 0,
        });
        let mut rejected_provider = GradientProvider::new(extent);
        assert_eq!(
            rejected
                .execute_temporal_roi_f32(&graph, &request, &mut rejected_provider)
                .expect_err("kernel scratch above the exact budget must be rejected"),
            EffectTemporalExecutionError::WorkingSetBudgetExceeded {
                required_bytes: exact_peak,
                budget_bytes: exact_peak - 1,
            }
        );
    }

    #[test]
    fn output_publication_is_part_of_the_working_set_budget() {
        let graph = bind_linear(
            [crate::EffectRenderOp::TemporalFrameBlend {
                sample_offset: TimelineTime::ZERO,
                mix: 0.5,
            }],
            temporal_contract(TimelineTime::ZERO, EffectRoiPropagation::PixelLocal),
        );
        let extent = EffectFrameExtent::new(4, 2);
        let frame_bytes = 8 * std::mem::size_of::<[f32; 4]>();
        let exact_peak = frame_bytes * 2;
        let request = EffectTemporalExecutionRequest::new(
            29,
            EffectExecutionContinuity::Continuous,
            TimelineTime::ONE,
            extent,
            extent.full_frame_roi(),
            ExecutionCancellationToken::new(),
        );
        let config = |max_working_bytes| EffectExecutionSessionConfig {
            max_cache_entries: 0,
            max_cache_bytes: 0,
            max_working_bytes,
            max_gpu_plan_entries: 0,
            max_gpu_plan_bytes: 0,
        };

        let mut admitted = EffectExecutionSession::new(config(exact_peak));
        let mut provider = GradientProvider::new(extent);
        let output = admitted
            .execute_temporal_roi_f32(&graph, &request, &mut provider)
            .expect("publication budget");
        assert_eq!(output.peak_working_bytes(), exact_peak);
        assert_eq!(output.provider_requests(), 1);

        let mut rejected = EffectExecutionSession::new(config(exact_peak - 1));
        let mut rejected_provider = GradientProvider::new(extent);
        assert_eq!(
            rejected
                .execute_temporal_roi_f32(&graph, &request, &mut rejected_provider)
                .expect_err("Arc publication must not bypass the byte grant"),
            EffectTemporalExecutionError::WorkingSetBudgetExceeded {
                required_bytes: exact_peak,
                budget_bytes: exact_peak - 1,
            }
        );
    }

    #[test]
    fn empty_output_preserves_the_bound_frame_seed_without_fetching() {
        let graph = bind_linear(
            [crate::EffectRenderOp::TemporalFrameBlend {
                sample_offset: TimelineTime::ZERO,
                mix: 0.5,
            }],
            temporal_contract(TimelineTime::ZERO, EffectRoiPropagation::PixelLocal),
        );
        let extent = EffectFrameExtent::new(4, 2);
        let request = EffectTemporalExecutionRequest::new(
            37,
            EffectExecutionContinuity::Continuous,
            TimelineTime::ONE,
            extent,
            EffectPixelRoi::new(0, 0, 0, 0),
            ExecutionCancellationToken::new(),
        )
        .with_output_frame_seed(0x5eed);
        let mut session = EffectExecutionSession::default();
        let mut provider = GradientProvider::new(extent);

        let output = session
            .execute_temporal_roi_f32(&graph, &request, &mut provider)
            .expect("empty output");

        assert_eq!(output.tile().frame_seed(), 0x5eed);
        assert!(output.tile().pixels().is_empty());
        assert_eq!(output.provider_requests(), 0);
        assert!(provider.requests.is_empty());
    }

    #[test]
    fn cancellation_and_generation_rotation_never_reuse_old_output() {
        let graph = bind_linear(
            [crate::EffectRenderOp::GaussianBlur { radius: 2.0 }],
            temporal_contract(
                TimelineTime::ZERO,
                EffectRoiPropagation::Expand { horizontal_pixels: 3, vertical_pixels: 3 },
            ),
        );
        let extent = EffectFrameExtent::new(2, 2);
        let mut provider = GradientProvider::new(extent);
        let mut session = EffectExecutionSession::default();
        let canceled = ExecutionCancellationToken::new();
        canceled.cancel();
        let canceled_request = EffectTemporalExecutionRequest::new(
            1,
            EffectExecutionContinuity::Continuous,
            TimelineTime::ZERO,
            extent,
            extent.full_frame_roi(),
            canceled,
        );
        assert!(matches!(
            session.execute_temporal_roi_f32(&graph, &canceled_request, &mut provider),
            Err(EffectTemporalExecutionError::Canceled)
        ));
        assert_eq!(session.diagnostics().generation, None);
        assert!(provider.requests.is_empty());

        let first = EffectTemporalExecutionRequest::new(
            1,
            EffectExecutionContinuity::Continuous,
            TimelineTime::ZERO,
            extent,
            extent.full_frame_roi(),
            ExecutionCancellationToken::new(),
        );
        session
            .execute_temporal_roi_f32(&graph, &first, &mut provider)
            .expect("first generation");
        let first_request_count = provider.requests.len();
        session
            .execute_temporal_roi_f32(&graph, &first, &mut provider)
            .expect("same-generation cache");
        assert_eq!(provider.requests.len(), first_request_count);

        let second = EffectTemporalExecutionRequest::new(
            2,
            EffectExecutionContinuity::Discontinuous,
            TimelineTime::ZERO,
            extent,
            extent.full_frame_roi(),
            ExecutionCancellationToken::new(),
        );
        session
            .execute_temporal_roi_f32(&graph, &second, &mut provider)
            .expect("new generation");
        assert!(provider.requests.len() > first_request_count);
    }
}
