//! Exact finite-history and ROI execution over the one compiled Effect IR.
//!
//! This Module is the scalar semantic reference for future tiled CPU/SIMD/GPU
//! adapters. It consumes [`crate::CompiledEffectGraph`] directly; it does not
//! introduce a second graph or reinterpret definition contracts.

use crate::adjustment::{
    apply_render_op_f32_region_controlled, render_op_f32_scratch_frames, EffectRasterRegion,
};
use crate::execution::blend_rgba_f32_region_controlled;
use crate::execution_session::EffectTemporalCachedOutput;
use crate::{
    CompiledEffectGraph, EffectExecutionDemandError, EffectExecutionSession, EffectFrameExtent,
    EffectGraphNodeId, EffectGraphNodeKind, EffectInputRoi, EffectPixelRoi,
    EffectProcessingBackend, EffectResourceLifetime, EffectRoiHalo, EffectStateModel,
    EffectTemporalBoundary, EffectTemporalSpan, EffectWorkingPrecision,
};
use mondrian_core::{ExecutionCancellationToken, TimelineTime};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

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
/// This fact is evidence, not a processor-owned rule key. Finite-history
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

/// Immutable Float32 tile returned by an Effect temporal source Adapter.
#[derive(Debug, Clone)]
pub struct EffectFrameTileF32 {
    time: TimelineTime,
    frame_extent: EffectFrameExtent,
    roi: EffectPixelRoi,
    frame_seed: i64,
    pixels: Arc<[[f32; 4]]>,
}

impl EffectFrameTileF32 {
    /// Validate and retain one exact source tile.
    pub fn new(
        time: TimelineTime,
        frame_extent: EffectFrameExtent,
        roi: EffectPixelRoi,
        frame_seed: i64,
        pixels: impl Into<Arc<[[f32; 4]]>>,
    ) -> Result<Self, EffectTemporalFrameProviderError> {
        let pixels = pixels.into();
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
        &self.pixels
    }

    fn byte_len(&self) -> usize {
        self.pixels.len().saturating_mul(std::mem::size_of::<[f32; 4]>())
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
    /// The Adapter returned a malformed or mismatched tile.
    #[error("effect temporal frame provider returned an invalid tile: {reason}")]
    InvalidTile {
        /// Exact contract violation.
        reason: String,
    },
}

/// Adapter that resolves Clip-domain frame requests to exact source or nested
/// Sequence pixels.
pub trait EffectTemporalFrameProvider {
    /// Complete immutable source/mapping identity used by cache admission.
    fn source_identity(&self) -> EffectTemporalSourceIdentity;

    /// Fetch exactly the requested region and Float32 representation.
    ///
    /// Implementations must observe `cancellation` before publishing success.
    /// A canceled result may not populate a decode or Effect cache.
    fn fetch_frame(
        &mut self,
        request: EffectTemporalFrameRequest,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<EffectFrameTileF32, EffectTemporalFrameProviderError>;
}

/// A fully resolved immutable temporal batch.
///
/// Construction proves that each graph demand has exactly one matching tile
/// and that no unrequested tile entered the set. `fetch_frame` is consequently
/// a bounded lookup only: it performs no decode, nested evaluation, title
/// rasterization, color conversion, or other hidden work.
#[derive(Debug, Clone)]
pub struct PreparedTemporalFrameSet {
    source_identity: EffectTemporalSourceIdentity,
    generation: u64,
    requests: Arc<[EffectTemporalFrameRequest]>,
    frames: HashMap<EffectTemporalFrameRequest, EffectFrameTileF32>,
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
}

impl EffectTemporalFrameProvider for PreparedTemporalFrameSet {
    fn source_identity(&self) -> EffectTemporalSourceIdentity {
        self.source_identity
    }

    fn fetch_frame(
        &mut self,
        request: EffectTemporalFrameRequest,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<EffectFrameTileF32, EffectTemporalFrameProviderError> {
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
        self.frames.get(&request).cloned().ok_or_else(|| {
            EffectTemporalFrameProviderError::Unavailable {
                reason: format!(
                    "prepared temporal set does not contain exact request at {} / {}",
                    request.time.numerator(),
                    request.time.denominator()
                ),
            }
        })
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

    /// Number of exact provider fetches performed (zero on a cache hit).
    pub const fn provider_requests(&self) -> usize {
        self.provider_requests
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
    /// The bounded production tracer admits past-only history. Future input
    /// needs scheduler look-ahead and is deliberately not approximated.
    #[error("effect execution requires future temporal input")]
    FutureTemporalInputUnsupported,
    /// A temporal graph did not match the exact bounded production shape:
    /// one Source-fed finite-history mixer plus a current-time DAG.
    #[error("temporal graph is outside the admitted production tracer: {reason}")]
    UnsupportedTemporalShape {
        /// Stable fail-closed explanation.
        reason: &'static str,
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
    /// A finite-history operation carried invalid runtime values.
    #[error("invalid temporal frame mix: {reason}")]
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

/// Collect every source frame required by the bounded finite-history production
/// tracer without fetching any pixels.
///
/// The admitted graph is deliberately mathematically closed: exactly one
/// `TemporalFrameMix` reads `Source` directly, while current-time unary
/// branches may fan out and rejoin through Blend or MultiInput nodes. A
/// temporal operation with upstream Effects would require those Effects to be
/// evaluated at each history time, while frame-bound parameters are currently
/// compiled at the output Clip time. Such graphs therefore fail closed instead
/// of silently applying current parameters to historical frames. Mask nodes
/// remain blocked until their rasterizer owns exact ROI coordinates and
/// cooperative cancellation.
pub fn collect_temporal_frame_demands(
    compiled: &CompiledEffectGraph,
    request: &EffectTemporalExecutionRequest,
) -> Result<EffectTemporalFrameDemandBatch, EffectTemporalExecutionError> {
    if request.cancellation.is_canceled() {
        return Err(EffectTemporalExecutionError::Canceled);
    }
    admit_temporal_scalar(compiled)?;
    if compiled.domain_plan().requires_conversion() || !compiled.domain_plan().blockers.is_empty() {
        return Err(EffectTemporalExecutionError::ColorDomainUnsupported);
    }
    let shape = admitted_temporal_shape(compiled)?.ok_or(
        EffectTemporalExecutionError::UnsupportedTemporalShape {
            reason: "graph has no finite-history operation",
        },
    )?;
    let demand = compiled.plan_execution_demand(
        request.output_time,
        request.frame_extent,
        request.output_roi,
    )?;
    ensure_bounded_temporal_window(demand.temporal_window())?;
    let past_time = temporal_past_time(request.output_time, shape.past_offset)?;
    let mut times = Vec::with_capacity(2);
    times.push(request.output_time);
    if past_time != request.output_time {
        times.push(past_time);
    }
    let requests = times
        .into_iter()
        .map(|time| EffectTemporalFrameRequest {
            generation: request.generation,
            time,
            frame_extent: demand.frame_extent(),
            input_roi: demand.input_roi(),
            exact_halo: demand.exact_halo(),
            precision: EffectWorkingPrecision::Float32,
        })
        .collect::<Vec<_>>();
    Ok(EffectTemporalFrameDemandBatch {
        generation: request.generation,
        requests: requests.into(),
    })
}

#[derive(Debug, Clone, Copy)]
struct AdmittedTemporalShape {
    node_id: EffectGraphNodeId,
    source_id: EffectGraphNodeId,
    past_offset: TimelineTime,
}

impl EffectExecutionSession {
    /// Execute a finite-history, stateless CPU Float32 graph for one exact ROI.
    ///
    /// The scalar reference retains only the exact input ROI. Coordinate-aware
    /// operations still use complete-frame positions, finite-kernel operations
    /// consume their admitted halo, and full-frame-only operations reject a
    /// partial region. Kernel scratch and every resident graph value are
    /// admitted before allocation. Compiled use counts move last-use values in
    /// place, clone only live fan-out inputs, and release joins immediately.
    pub fn execute_temporal_roi_f32(
        &mut self,
        compiled: &CompiledEffectGraph,
        request: &EffectTemporalExecutionRequest,
        provider: &mut dyn EffectTemporalFrameProvider,
    ) -> Result<EffectTemporalExecutionOutput, EffectTemporalExecutionError> {
        if request.cancellation.is_canceled() {
            return Err(EffectTemporalExecutionError::Canceled);
        }
        admit_temporal_scalar(compiled)?;
        if compiled.domain_plan().requires_conversion()
            || !compiled.domain_plan().blockers.is_empty()
        {
            return Err(EffectTemporalExecutionError::ColorDomainUnsupported);
        }
        let temporal_shape = admitted_temporal_shape(compiled)?;
        self.bind_generation(request.generation);
        let demand = compiled.plan_execution_demand(
            request.output_time,
            request.frame_extent,
            request.output_roi,
        )?;
        ensure_bounded_temporal_window(demand.temporal_window())?;
        let source_identity = provider.source_identity();
        let cache_identity =
            temporal_cache_identity(compiled, request, demand.input_roi(), source_identity);
        if compiled.output_cache_enabled() {
            if let Some(cached) = self.get_temporal_output(&cache_identity) {
                let tile = EffectFrameTileF32::new(
                    request.output_time,
                    demand.frame_extent(),
                    demand.output_roi(),
                    cached.frame_seed,
                    cached.pixels,
                )?;
                return Ok(EffectTemporalExecutionOutput {
                    tile,
                    cache_identity,
                    provider_requests: 0,
                    peak_working_bytes: 0,
                });
            }
        }
        if demand.output_roi().is_empty() || demand.frame_extent().is_empty() {
            let tile = EffectFrameTileF32::new(
                request.output_time,
                demand.frame_extent(),
                demand.output_roi(),
                request.output_frame_seed,
                Arc::<[[f32; 4]]>::from([]),
            )?;
            return Ok(EffectTemporalExecutionOutput {
                tile,
                cache_identity,
                provider_requests: 0,
                peak_working_bytes: 0,
            });
        }

        let mut evaluator = ScalarTemporalEvaluator::new(
            compiled,
            request,
            demand.input_roi(),
            demand.exact_halo(),
            temporal_shape,
            provider,
            self.max_working_bytes(),
        )?;
        let output = evaluator.execute()?;
        if request.cancellation.is_canceled() {
            return Err(EffectTemporalExecutionError::Canceled);
        }
        let output_pixel_bytes = checked_pixel_count(demand.output_roi())
            .and_then(|pixels| pixels.checked_mul(std::mem::size_of::<[f32; 4]>()))
            .ok_or(EffectTemporalExecutionError::WorkingSetBudgetExceeded {
                required_bytes: usize::MAX,
                budget_bytes: evaluator.working.budget,
            })?;
        // `crop_tile` owns one Vec while `Arc<[T]>::from(Vec<T>)` may allocate
        // its reference-counted target before releasing that Vec. Admit both
        // transient output-sized allocations conservatively.
        let publication_bytes = output_pixel_bytes.checked_mul(2).ok_or(
            EffectTemporalExecutionError::WorkingSetBudgetExceeded {
                required_bytes: usize::MAX,
                budget_bytes: evaluator.working.budget,
            },
        )?;
        evaluator.working.ensure_transient(publication_bytes)?;
        let output_pixels = Arc::<[[f32; 4]]>::from(crop_tile(
            &output,
            demand.input_roi().region(),
            demand.output_roi(),
            &request.cancellation,
        )?);
        let output_seed = request.output_frame_seed;
        let provider_requests = evaluator.provider_requests;
        let peak_working_bytes = evaluator.working.peak;
        drop(output);
        drop(evaluator);
        if compiled.output_cache_enabled() && !request.cancellation.is_canceled() {
            self.put_temporal_output(
                cache_identity,
                EffectTemporalCachedOutput {
                    pixels: Arc::clone(&output_pixels),
                    frame_seed: output_seed,
                },
            );
        }
        let tile = EffectFrameTileF32::new(
            request.output_time,
            demand.frame_extent(),
            demand.output_roi(),
            output_seed,
            output_pixels,
        )?;
        Ok(EffectTemporalExecutionOutput {
            tile,
            cache_identity,
            provider_requests,
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
    if !matches!(aggregate.temporal_input.future, EffectTemporalSpan::None) {
        return Err(EffectTemporalExecutionError::FutureTemporalInputUnsupported);
    }
    Ok(())
}

fn admitted_temporal_shape(
    compiled: &CompiledEffectGraph,
) -> Result<Option<AdmittedTemporalShape>, EffectTemporalExecutionError> {
    compiled
        .graph()
        .output
        .ok_or(EffectTemporalExecutionError::MissingGraphOutput)?;
    let mut temporal = None;
    let mut source = None;
    for node_id in &compiled.schedule().ordered_nodes {
        let node = compiled.graph().node(*node_id).ok_or(
            EffectTemporalExecutionError::UnsupportedGraphNode {
                node_id: *node_id,
                kind: "missing",
            },
        )?;
        match &node.kind {
            EffectGraphNodeKind::Source => {
                if source.replace(node.id).is_some() {
                    return Err(EffectTemporalExecutionError::UnsupportedTemporalShape {
                        reason: "more than one source value is reachable",
                    });
                }
            }
            EffectGraphNodeKind::UnaryEffect { input, op }
            | EffectGraphNodeKind::DomainEffect { input, op, .. } => {
                if let crate::EffectRenderOp::TemporalFrameMix { past_offset, mix } = op {
                    if temporal.is_some() {
                        return Err(EffectTemporalExecutionError::UnsupportedTemporalShape {
                            reason: "more than one temporal operation is reachable",
                        });
                    }
                    validate_temporal_mix(*past_offset, *mix)?;
                    let input_node = compiled.graph().node(*input).ok_or(
                        EffectTemporalExecutionError::UnsupportedGraphNode {
                            node_id: *input,
                            kind: "missing",
                        },
                    )?;
                    if !matches!(&input_node.kind, EffectGraphNodeKind::Source) {
                        return Err(EffectTemporalExecutionError::UnsupportedTemporalShape {
                            reason:
                                "temporal input has upstream Effects or another derived graph value",
                        });
                    }
                    temporal = Some(AdmittedTemporalShape {
                        node_id: node.id,
                        source_id: *input,
                        past_offset: *past_offset,
                    });
                }
            }
            EffectGraphNodeKind::Blend { .. } | EffectGraphNodeKind::MultiInput { .. } => {}
            EffectGraphNodeKind::Mask { .. } | EffectGraphNodeKind::MaskSource { .. } => {
                return Err(EffectTemporalExecutionError::UnsupportedTemporalShape {
                    reason:
                        "temporal ROI mask execution requires a coordinate-aware cancellable rasterizer",
                });
            }
        }
    }
    if source.is_none() {
        return Err(EffectTemporalExecutionError::UnsupportedTemporalShape {
            reason: "graph has no reachable source value",
        });
    }
    Ok(temporal)
}

fn validate_temporal_mix(
    past_offset: TimelineTime,
    mix: f32,
) -> Result<(), EffectTemporalExecutionError> {
    if past_offset.is_negative() {
        return Err(EffectTemporalExecutionError::InvalidTemporalOperation {
            reason: "past offset is negative",
        });
    }
    if !mix.is_finite() || !(0.0..=1.0).contains(&mix) {
        return Err(EffectTemporalExecutionError::InvalidTemporalOperation {
            reason: "mix must be finite and within [0, 1]",
        });
    }
    Ok(())
}

fn temporal_past_time(
    time: TimelineTime,
    past_offset: TimelineTime,
) -> Result<TimelineTime, EffectTemporalExecutionError> {
    validate_temporal_mix(past_offset, 0.0)?;
    time.checked_sub(past_offset).map_err(|_| {
        EffectTemporalExecutionError::InvalidTemporalOperation {
            reason: "past sample arithmetic overflowed",
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

struct ScalarTemporalEvaluator<'a> {
    compiled: &'a CompiledEffectGraph,
    request: &'a EffectTemporalExecutionRequest,
    input_roi: EffectInputRoi,
    raster_region: EffectRasterRegion,
    exact_halo: Option<EffectRoiHalo>,
    temporal_shape: Option<AdmittedTemporalShape>,
    provider: &'a mut dyn EffectTemporalFrameProvider,
    outputs: HashMap<EffectGraphNodeId, Vec<[f32; 4]>>,
    remaining_uses: HashMap<EffectGraphNodeId, usize>,
    provider_requests: usize,
    working: ScalarWorkingSet,
}

impl<'a> ScalarTemporalEvaluator<'a> {
    fn new(
        compiled: &'a CompiledEffectGraph,
        request: &'a EffectTemporalExecutionRequest,
        input_roi: EffectInputRoi,
        exact_halo: Option<EffectRoiHalo>,
        temporal_shape: Option<AdmittedTemporalShape>,
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
        Ok(Self {
            compiled,
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
            temporal_shape,
            provider,
            outputs: HashMap::with_capacity(compiled.graph().nodes.len()),
            remaining_uses: compiled.node_use_counts().clone(),
            provider_requests: 0,
            working: ScalarWorkingSet::new(working_budget, frame_bytes),
        })
    }

    fn execute(&mut self) -> Result<Vec<[f32; 4]>, EffectTemporalExecutionError> {
        let schedule = self.compiled.schedule().ordered_nodes.clone();
        for node_id in schedule {
            temporal_cancellation_checkpoint(&self.request.cancellation)?;
            let node = self
                .compiled
                .graph()
                .node(node_id)
                .ok_or(EffectTemporalExecutionError::UnsupportedGraphNode {
                    node_id,
                    kind: "missing",
                })?
                .clone();
            let output = match node.kind {
                EffectGraphNodeKind::Source => self.fetch_source(self.request.output_time)?,
                EffectGraphNodeKind::UnaryEffect { input, op }
                | EffectGraphNodeKind::DomainEffect { input, op, .. } => match op {
                    crate::EffectRenderOp::TemporalFrameMix { past_offset, mix } => {
                        let shape = self.temporal_shape.ok_or(
                            EffectTemporalExecutionError::UnsupportedTemporalShape {
                                reason: "temporal operation has no admitted execution shape",
                            },
                        )?;
                        if shape.node_id != node_id || shape.source_id != input {
                            return Err(EffectTemporalExecutionError::InvalidGraphLiveness {
                                reason: "scheduled temporal node differs from admitted shape",
                            });
                        }
                        self.temporal_mix(input, past_offset, mix)?
                    }
                    op => {
                        let mut output = self.take_graph_input(input)?;
                        let scratch_bytes = render_op_f32_scratch_frames(&op)
                            .checked_mul(self.working.frame_bytes)
                            .ok_or(EffectTemporalExecutionError::WorkingSetBudgetExceeded {
                                required_bytes: usize::MAX,
                                budget_bytes: self.working.budget,
                            })?;
                        self.working.ensure_transient(scratch_bytes)?;
                        let cancellation = self.request.cancellation.clone();
                        let execution = apply_render_op_f32_region_controlled(
                            &mut output,
                            self.raster_region,
                            &op,
                            self.request.output_frame_seed,
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
                    let mut base = self.take_graph_input(base)?;
                    let overlay = self.take_graph_input(overlay)?;
                    let cancellation = self.request.cancellation.clone();
                    let blended = blend_rgba_f32_region_controlled(
                        &mut base,
                        &overlay,
                        self.raster_region,
                        opacity,
                        blend_mode,
                        self.request.output_frame_seed,
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
                EffectGraphNodeKind::Mask { .. } => {
                    return Err(EffectTemporalExecutionError::UnsupportedGraphNode {
                        node_id,
                        kind: "mask",
                    });
                }
                EffectGraphNodeKind::MaskSource { .. } => {
                    return Err(EffectTemporalExecutionError::UnsupportedGraphNode {
                        node_id,
                        kind: "mask_source",
                    });
                }
                EffectGraphNodeKind::MultiInput { inputs, blend_mode, opacity } => {
                    let Some(first) = inputs.first().copied() else {
                        return Err(EffectTemporalExecutionError::InvalidGraphLiveness {
                            reason: "multi-input node has no inputs",
                        });
                    };
                    let mut output = self.take_graph_input(first)?;
                    for overlay_id in &inputs[1..] {
                        let overlay = self.take_graph_input(*overlay_id)?;
                        let cancellation = self.request.cancellation.clone();
                        let blended = blend_rgba_f32_region_controlled(
                            &mut output,
                            &overlay,
                            self.raster_region,
                            opacity,
                            blend_mode,
                            self.request.output_frame_seed,
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
            };
            if self.outputs.insert(node_id, output).is_some() {
                return Err(EffectTemporalExecutionError::InvalidGraphLiveness {
                    reason: "compiled schedule produced one graph value more than once",
                });
            }
        }

        let output_id = self
            .compiled
            .graph()
            .output
            .ok_or(EffectTemporalExecutionError::MissingGraphOutput)?;
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
                reason: "compiled use counts retained unconsumed graph edges",
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
        let tile = self.provider.fetch_frame(provider_request, &self.request.cancellation)?;
        self.provider_requests = self.provider_requests.saturating_add(1);
        if self.request.cancellation.is_canceled() {
            return Err(EffectTemporalExecutionError::Canceled);
        }
        validate_provider_tile(&tile, provider_request)?;
        self.working.reserve_frame_with_transient(tile.byte_len())?;
        clone_tile_pixels(&tile, &self.request.cancellation)
    }

    fn temporal_mix(
        &mut self,
        input_id: EffectGraphNodeId,
        past_offset: TimelineTime,
        mix: f32,
    ) -> Result<Vec<[f32; 4]>, EffectTemporalExecutionError> {
        validate_temporal_mix(past_offset, mix)?;
        let past_time = temporal_past_time(self.request.output_time, past_offset)?;
        let mut current = self.take_graph_input(input_id)?;
        if past_time == self.request.output_time {
            temporal_cancellation_checkpoint(&self.request.cancellation)?;
            return Ok(current);
        }
        let past = self.fetch_source(past_time)?;
        if current.len() != past.len() {
            return Err(EffectTemporalExecutionError::InvalidRoiProjection {
                reason: "temporal input frames do not have equal pixel counts",
            });
        }
        mix_temporal_frames_in_place_controlled(&mut current, &past, mix, &mut || {
            temporal_cancellation_checkpoint(&self.request.cancellation)
        })?;
        self.working.release_frame()?;
        Ok(current)
    }

    fn take_graph_input(
        &mut self,
        node_id: EffectGraphNodeId,
    ) -> Result<Vec<[f32; 4]>, EffectTemporalExecutionError> {
        if !self.outputs.contains_key(&node_id) {
            return Err(EffectTemporalExecutionError::MissingGraphValue { node_id });
        }
        let remaining = self.remaining_uses.get_mut(&node_id).ok_or(
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
            return self
                .outputs
                .remove(&node_id)
                .ok_or(EffectTemporalExecutionError::MissingGraphValue { node_id });
        }

        self.working.reserve_frame()?;
        self.outputs
            .get(&node_id)
            .cloned()
            .ok_or(EffectTemporalExecutionError::MissingGraphValue { node_id })
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

    fn reserve_frame_with_transient(
        &mut self,
        transient_bytes: usize,
    ) -> Result<(), EffectTemporalExecutionError> {
        let required = self
            .resident_bytes
            .checked_add(transient_bytes)
            .and_then(|bytes| bytes.checked_add(self.frame_bytes))
            .ok_or(EffectTemporalExecutionError::WorkingSetBudgetExceeded {
                required_bytes: usize::MAX,
                budget_bytes: self.budget,
            })?;
        if required > self.budget {
            return Err(EffectTemporalExecutionError::WorkingSetBudgetExceeded {
                required_bytes: required,
                budget_bytes: self.budget,
            });
        }
        self.resident_bytes = self.resident_bytes.saturating_add(self.frame_bytes);
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
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"mondrian.effect-temporal-roi-execution.v2");
    hasher.update(compiled.semantic_fingerprint());
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

fn clone_tile_pixels(
    tile: &EffectFrameTileF32,
    cancellation: &ExecutionCancellationToken,
) -> Result<Vec<[f32; 4]>, EffectTemporalExecutionError> {
    let mut pixels = Vec::with_capacity(tile.pixels.len());
    for chunk in tile.pixels.chunks(4_096) {
        temporal_cancellation_checkpoint(cancellation)?;
        pixels.extend_from_slice(chunk);
    }
    temporal_cancellation_checkpoint(cancellation)?;
    Ok(pixels)
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
        crate::EffectRenderOp::TemporalFrameMix { .. } => "temporal_frame_mix",
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
    fn temporal_mix_checks_cancellation_at_fixed_pixel_chunks() {
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

        fn fetch_frame(
            &mut self,
            request: EffectTemporalFrameRequest,
            cancellation: &ExecutionCancellationToken,
        ) -> Result<EffectFrameTileF32, EffectTemporalFrameProviderError> {
            if cancellation.is_canceled() {
                return Err(EffectTemporalFrameProviderError::Canceled);
            }
            assert_eq!(request.frame_extent(), self.extent);
            let roi = request.input_roi().region();
            let mut pixels = Vec::new();
            for y in roi.y()..roi.y() + roi.height() {
                for x in roi.x()..roi.x() + roi.width() {
                    pixels.push(Self::pixel(request.time(), x, y));
                }
            }
            self.requests.push(request);
            EffectFrameTileF32::new(
                request.time(),
                request.frame_extent(),
                roi,
                request.time().numerator(),
                Arc::<[[f32; 4]]>::from(pixels),
            )
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

    fn dag_contract(past: EffectTemporalSpan) -> EffectExecutionContract {
        EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            determinism: EffectDeterminism::Deterministic,
            state_model: EffectStateModel::Stateless,
            temporal_input: EffectTemporalInputExtent { past, future: EffectTemporalSpan::None },
            roi_propagation: EffectRoiPropagation::PixelLocal,
            resource_lifetime: EffectResourceLifetime::Frame,
            topology: EffectGraphTopology::GeneralDag,
        }
    }

    #[test]
    fn finite_history_processor_fetches_exact_signed_owner_domain_times() {
        let offset = TimelineTime::new(1, 2).expect("offset");
        let graph = bind_linear(
            [crate::EffectRenderOp::TemporalFrameMix { past_offset: offset, mix: 0.25 }],
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
    fn frozen_batch_collects_once_and_is_the_only_execution_provider() {
        let offset = TimelineTime::new(1, 2).expect("offset");
        let graph = bind_linear(
            [
                crate::EffectRenderOp::TemporalFrameMix { past_offset: offset, mix: 0.25 },
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
                    Arc::<[[f32; 4]]>::from(pixels),
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
    fn demand_collection_rejects_effects_before_temporal_mix() {
        let offset = TimelineTime::new(1, 2).expect("offset");
        let graph = bind_linear(
            [
                crate::EffectRenderOp::ColorAdjust {
                    exposure: 0.0,
                    contrast: 1.0,
                    saturation: 1.0,
                    working_color_space: mondrian_core::WorkingColorSpace::LinearRec709,
                },
                crate::EffectRenderOp::TemporalFrameMix { past_offset: offset, mix: 0.5 },
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
    fn temporal_mask_dag_stays_blocked_until_raster_contract_is_tile_safe() {
        let offset = TimelineTime::new(1, 2).expect("offset");
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
                            op: crate::EffectRenderOp::TemporalFrameMix {
                                past_offset: offset,
                                mix: 0.5,
                            },
                        },
                    },
                    EffectGraphNode {
                        id: EffectGraphNodeId(2),
                        kind: EffectGraphNodeKind::MaskSource {
                            shape: crate::mask::MaskShape::Rectangle {
                                x: 0.0,
                                y: 0.0,
                                width: 1.0,
                                height: 1.0,
                                corner_radius: 0.0,
                            },
                            feather: 0.0,
                            expansion: 0.0,
                            opacity: 1.0,
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
        let request = EffectTemporalExecutionRequest::new(
            1,
            EffectExecutionContinuity::Continuous,
            TimelineTime::ONE,
            extent,
            EffectPixelRoi::new(2, 1, 3, 2),
            ExecutionCancellationToken::new(),
        );

        assert!(matches!(
            collect_temporal_frame_demands(&graph, &request),
            Err(EffectTemporalExecutionError::UnsupportedTemporalShape {
                reason:
                    "temporal ROI mask execution requires a coordinate-aware cancellable rasterizer"
            })
        ));
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
                            op: crate::EffectRenderOp::TemporalFrameMix {
                                past_offset: offset,
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
    }

    #[test]
    fn frozen_batch_rejects_missing_and_stale_generation_tiles() {
        let offset = TimelineTime::new(1, 2).expect("offset");
        let graph = bind_linear(
            [crate::EffectRenderOp::TemporalFrameMix { past_offset: offset, mix: 0.5 }],
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
        let graph =
            bind_linear(
                [crate::EffectRenderOp::TemporalFrameMix {
                    past_offset: TimelineTime::ZERO,
                    mix: 0.5,
                }],
                temporal_contract(TimelineTime::ZERO, EffectRoiPropagation::PixelLocal),
            );
        let extent = EffectFrameExtent::new(4, 2);
        let frame_bytes = 8 * std::mem::size_of::<[f32; 4]>();
        let exact_peak = frame_bytes * 3;
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
        let graph =
            bind_linear(
                [crate::EffectRenderOp::TemporalFrameMix {
                    past_offset: TimelineTime::ZERO,
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
