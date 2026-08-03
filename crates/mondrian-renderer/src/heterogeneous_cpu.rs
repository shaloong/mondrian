//! Renderer-owned atomic CPU-prefix preparation for heterogeneous Effects.
//!
//! Scheduling, freshness, deadlines, and presentation remain outside this
//! Module. A caller first prepares the exact CPU-F32 → GPU-F32 graph route,
//! then binds typed working frames under an immutable resource grant. The
//! executor performs no route selection: it executes an all-or-nothing batch
//! and returns move-only completions for
//! [`crate::record_heterogeneous_gpu_continuation`].

use std::collections::HashSet;
use std::sync::Arc;

use mondrian_core::WorkingColorSpace;
use mondrian_effects::{
    CompiledEffectGraph, EffectExecutionSessionConfig, EffectFrameExtent,
    EffectGraphExecutionBudget, HeterogeneousCpuExecutionStopReason,
    PreparedHeterogeneousCpuCompletion, PreparedHeterogeneousEffectWork,
    PreparedHeterogeneousEffectWorkError,
};

use crate::{
    ColorFrameAlpha, ColorFrameDomain, ColorFrameEncoding, ColorFrameResidency, ColorFrameSpace,
    CpuColorFrame, HeterogeneousGpuExecutionCapability, TimelineCompositeScratch,
};

/// Exact immutable CPU-F32 → GPU-F32 route prepared before source pixels are
/// materialized.
///
/// The route owns the graph-value plan and lowered GPU suffix. Binding pixels
/// later cannot change the selected lanes, transfers, precision, or extent.
#[derive(Debug, Clone)]
pub struct PreparedHeterogeneousEffectRoute {
    graph: Arc<CompiledEffectGraph>,
    work: Arc<PreparedHeterogeneousEffectWork>,
    graph_fingerprint: [u8; 32],
    budget: EffectGraphExecutionBudget,
}

impl PreparedHeterogeneousEffectRoute {
    /// Prepare the renderer's production heterogeneous route for one exact
    /// graph, extent, and graph-planning budget.
    pub fn prepare(
        graph: Arc<CompiledEffectGraph>,
        extent: EffectFrameExtent,
        budget: EffectGraphExecutionBudget,
    ) -> Result<Self, PreparedHeterogeneousEffectWorkError> {
        let graph_fingerprint = graph.semantic_fingerprint();
        let retained_graph = Arc::clone(&graph);
        let capability = HeterogeneousGpuExecutionCapability::scene_linear_f32().map_err(|_| {
            PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                reason: "renderer_heterogeneous_capability_unavailable",
            }
        })?;
        let work = PreparedHeterogeneousEffectWork::prepare(
            graph,
            capability.environment(),
            capability.request(extent, budget),
        )?;
        Ok(Self {
            graph: retained_graph,
            work: Arc::new(work),
            graph_fingerprint,
            budget,
        })
    }

    /// Complete semantic fingerprint of the compiled graph.
    pub fn graph_fingerprint(&self) -> [u8; 32] {
        self.graph_fingerprint
    }

    /// Exact frame extent used by graph-value planning.
    pub fn frame_extent(&self) -> EffectFrameExtent {
        self.work.plan().frame_extent()
    }

    /// Exact compiled graph whose route was prepared.
    pub fn graph(&self) -> &Arc<CompiledEffectGraph> {
        &self.graph
    }

    /// Effect-owned prepared graph-value work used by CPU and GPU executors.
    pub fn prepared_work(&self) -> &PreparedHeterogeneousEffectWork {
        &self.work
    }

    /// Exact graph-planning authority used during preparation.
    pub const fn graph_budget(&self) -> EffectGraphExecutionBudget {
        self.budget
    }

    fn work(&self) -> &PreparedHeterogeneousEffectWork {
        self.prepared_work()
    }

    fn budget(&self) -> EffectGraphExecutionBudget {
        self.graph_budget()
    }
}

/// One addressable working-linear input to an atomic heterogeneous CPU batch.
#[derive(Debug, Clone)]
pub struct HeterogeneousCpuPrefixBatchItem {
    address: u32,
    route: PreparedHeterogeneousEffectRoute,
    input: CpuColorFrame,
    working_color_space: WorkingColorSpace,
    frame_seed: i64,
}

impl HeterogeneousCpuPrefixBatchItem {
    /// Bind one compiled graph to an exact working frame and deterministic
    /// frame seed.
    pub fn new(
        address: u32,
        route: PreparedHeterogeneousEffectRoute,
        input: CpuColorFrame,
        working_color_space: WorkingColorSpace,
        frame_seed: i64,
    ) -> Self {
        Self {
            address,
            route,
            input,
            working_color_space,
            frame_seed,
        }
    }

    /// Caller-owned address retained in the completion.
    pub const fn address(&self) -> u32 {
        self.address
    }

    /// Complete compiled-graph semantic fingerprint.
    pub fn graph_fingerprint(&self) -> [u8; 32] {
        self.route.graph_fingerprint()
    }

    /// Declared working-space identity.
    pub const fn working_color_space(&self) -> WorkingColorSpace {
        self.working_color_space
    }

    /// Deterministic frame seed.
    pub const fn frame_seed(&self) -> i64 {
        self.frame_seed
    }

    /// Typed CPU frame descriptor.
    pub fn descriptor(&self) -> crate::ColorFrameDescriptor {
        self.input.descriptor()
    }
}

/// Frozen renderer authority for one atomic CPU-prefix batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeterogeneousCpuPrefixBatchGrant {
    effect_session: EffectExecutionSessionConfig,
    graph_execution: EffectGraphExecutionBudget,
    max_batch_items: usize,
    max_batch_pixel_bytes: u64,
}

impl HeterogeneousCpuPrefixBatchGrant {
    /// Construct exact Session, graph-planning, cardinality, and retained-pixel
    /// limits.
    pub const fn new(
        effect_session: EffectExecutionSessionConfig,
        graph_execution: EffectGraphExecutionBudget,
        max_batch_items: usize,
        max_batch_pixel_bytes: u64,
    ) -> Self {
        Self {
            effect_session,
            graph_execution,
            max_batch_items,
            max_batch_pixel_bytes,
        }
    }

    /// Effect Session cache and transient working-set authority.
    pub const fn effect_session(self) -> EffectExecutionSessionConfig {
        self.effect_session
    }

    /// Frame-local graph-value planning authority.
    pub const fn graph_execution(self) -> EffectGraphExecutionBudget {
        self.graph_execution
    }

    /// Maximum number of addressed items.
    pub const fn max_batch_items(self) -> usize {
        self.max_batch_items
    }

    /// Maximum aggregate logical bytes retained by immutable inputs plus
    /// successful CPU-prefix outputs.
    pub const fn max_batch_pixel_bytes(self) -> u64 {
        self.max_batch_pixel_bytes
    }
}

/// Immutable, all-or-nothing heterogeneous CPU-prefix request.
#[derive(Debug, Clone)]
pub struct HeterogeneousCpuPrefixBatchRequest {
    grant: HeterogeneousCpuPrefixBatchGrant,
    items: Box<[HeterogeneousCpuPrefixBatchItem]>,
}

impl HeterogeneousCpuPrefixBatchRequest {
    /// Construct an atomic batch. Call [`Self::validate`] before scheduling to
    /// reject malformed work without consuming queue capacity.
    pub fn new(
        grant: HeterogeneousCpuPrefixBatchGrant,
        items: impl Into<Box<[HeterogeneousCpuPrefixBatchItem]>>,
    ) -> Self {
        Self { grant, items: items.into() }
    }

    /// Frozen resource authority.
    pub const fn grant(&self) -> HeterogeneousCpuPrefixBatchGrant {
        self.grant
    }

    /// Inputs in deterministic recording order.
    pub fn items(&self) -> &[HeterogeneousCpuPrefixBatchItem] {
        &self.items
    }

    /// Validate frame contracts, unique addresses, and retained-pixel
    /// authority without preparing or executing an Effect graph.
    pub fn validate(&self) -> Result<(), HeterogeneousCpuPrefixBatchError> {
        validate_batch_request(self)
    }
}

/// One addressed CPU completion awaiting its exact renderer GPU suffix.
#[derive(Debug)]
pub struct HeterogeneousCpuPrefixBatchCompletion {
    address: u32,
    completion: PreparedHeterogeneousCpuCompletion,
}

impl HeterogeneousCpuPrefixBatchCompletion {
    /// Caller-owned address.
    pub const fn address(&self) -> u32 {
        self.address
    }

    /// Exact CPU completion and pending graph-value token chain.
    pub const fn completion(&self) -> &PreparedHeterogeneousCpuCompletion {
        &self.completion
    }

    /// Consume the addressed handoff.
    pub fn into_parts(self) -> (u32, PreparedHeterogeneousCpuCompletion) {
        (self.address, self.completion)
    }
}

/// Successful atomic CPU-prefix batch in input order.
#[derive(Debug)]
pub struct HeterogeneousCpuPrefixBatchOutput {
    completions: Box<[HeterogeneousCpuPrefixBatchCompletion]>,
}

impl HeterogeneousCpuPrefixBatchOutput {
    /// Addressed completions in deterministic request order.
    pub fn completions(&self) -> &[HeterogeneousCpuPrefixBatchCompletion] {
        &self.completions
    }

    /// Consume all addressed completions.
    pub fn into_completions(self) -> Box<[HeterogeneousCpuPrefixBatchCompletion]> {
        self.completions
    }
}

/// Why a typed frame cannot enter the working-linear CPU-prefix seam.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HeterogeneousCpuPrefixFrameContractViolation {
    /// The frame is not a working-domain value.
    #[error("frame domain is {actual:?}, expected Working")]
    Domain {
        /// Rejected domain.
        actual: ColorFrameDomain,
    },
    /// The frame is not linear floating point.
    #[error("frame encoding is {actual:?}, expected LinearFloat")]
    Encoding {
        /// Rejected encoding.
        actual: ColorFrameEncoding,
    },
    /// The frame is not CPU resident.
    #[error("frame residency is {actual:?}, expected Cpu")]
    Residency {
        /// Rejected residency.
        actual: ColorFrameResidency,
    },
    /// The descriptor does not carry the declared working identity.
    #[error("frame color identity is {actual:?}, expected Working({expected:?})")]
    ColorSpace {
        /// Declared identity.
        expected: WorkingColorSpace,
        /// Rejected descriptor identity.
        actual: ColorFrameSpace,
    },
    /// The typed float payload does not carry the declared identity.
    #[error("working pixel identity is {actual:?}, expected {expected:?}")]
    PixelColorSpace {
        /// Declared identity.
        expected: WorkingColorSpace,
        /// Rejected pixel identity.
        actual: WorkingColorSpace,
    },
    /// Premultiplied pixels cannot cross the Effect/color seam.
    #[error("frame alpha association is {actual:?}, expected straight-compatible coverage")]
    Alpha {
        /// Rejected alpha contract.
        actual: ColorFrameAlpha,
    },
    /// Descriptor and typed pixels disagree.
    #[error(
        "frame descriptor/payload mismatch: descriptor {descriptor_width}x{descriptor_height}, \
         payload {payload_width}x{payload_height} with {payload_pixels} pixels"
    )]
    PayloadExtent {
        /// Descriptor width.
        descriptor_width: u32,
        /// Descriptor height.
        descriptor_height: u32,
        /// Pixel payload width.
        payload_width: u32,
        /// Pixel payload height.
        payload_height: u32,
        /// Pixel payload cardinality.
        payload_pixels: usize,
    },
}

/// Request validation, route preparation, or controlled execution failure.
#[derive(Debug, thiserror::Error)]
pub enum HeterogeneousCpuPrefixBatchError {
    /// Atomic batches must contain at least one item.
    #[error("heterogeneous CPU-prefix batch is empty")]
    EmptyBatch,
    /// Batch cardinality exceeds caller authority.
    #[error("heterogeneous CPU-prefix batch contains {actual} items; grant permits {limit}")]
    BatchItemLimitExceeded {
        /// Submitted item count.
        actual: usize,
        /// Granted maximum.
        limit: usize,
    },
    /// Two outputs would claim the same caller address.
    #[error("heterogeneous CPU-prefix batch repeats address {address}")]
    DuplicateAddress {
        /// Ambiguous address.
        address: u32,
    },
    /// One input violates the typed working-frame contract.
    #[error("heterogeneous CPU-prefix item {address} has an invalid frame: {violation}")]
    FrameContract {
        /// Caller address.
        address: u32,
        /// Rejected contract dimension.
        violation: HeterogeneousCpuPrefixFrameContractViolation,
    },
    /// The materialized frame no longer matches the pre-decoding route extent.
    #[error(
        "heterogeneous CPU-prefix item {address} extent changed after route preparation: prepared={prepared:?}, actual={actual:?}"
    )]
    PreparedExtentMismatch {
        /// Caller address.
        address: u32,
        /// Extent frozen by route preparation.
        prepared: EffectFrameExtent,
        /// Extent carried by the bound working frame.
        actual: EffectFrameExtent,
    },
    /// A caller attempted to bind work prepared under different graph-planning
    /// authority.
    #[error("heterogeneous CPU-prefix item {address} route budget differs from the batch grant")]
    PreparedBudgetMismatch {
        /// Caller address.
        address: u32,
    },
    /// Aggregate retained-pixel accounting overflowed.
    #[error("heterogeneous CPU-prefix batch pixel-byte accounting overflowed")]
    BatchPixelBytesOverflow,
    /// Immutable inputs plus successful outputs exceed caller authority.
    #[error(
        "heterogeneous CPU-prefix batch retains {required} pixel bytes; grant permits {limit}"
    )]
    BatchPixelBytesExceeded {
        /// Conservatively retained logical bytes.
        required: u64,
        /// Granted maximum.
        limit: u64,
    },
    /// Cooperative cancellation or deadline stopped the atomic batch.
    #[error("heterogeneous CPU-prefix batch stopped: {reason:?}")]
    Stopped {
        /// Caller-owned stop classification.
        reason: HeterogeneousCpuExecutionStopReason,
    },
    /// Scalar CPU-prefix execution failed.
    #[error("heterogeneous CPU-prefix item {address} execution failed: {source}")]
    Execute {
        /// Caller address.
        address: u32,
        /// Effect-owned reason.
        #[source]
        source: PreparedHeterogeneousEffectWorkError,
    },
}

/// Exclusive renderer execution owner for serial CPU-prefix batches.
///
/// One Preview worker or Export job owns one instance. A panic boundary must
/// replace the complete executor before admitting later work so no mutable
/// Effect Session survives an unwind.
#[derive(Default)]
pub struct HeterogeneousCpuPrefixBatchExecutor {
    scratch: TimelineCompositeScratch,
}

impl HeterogeneousCpuPrefixBatchExecutor {
    /// Execute every item serially and publish completions only if the complete
    /// batch succeeds.
    pub fn execute(
        &mut self,
        request: HeterogeneousCpuPrefixBatchRequest,
        generation: u64,
        mut checkpoint: impl FnMut() -> Option<HeterogeneousCpuExecutionStopReason>,
    ) -> Result<HeterogeneousCpuPrefixBatchOutput, HeterogeneousCpuPrefixBatchError> {
        request.validate()?;
        stop_if_requested(&mut checkpoint)?;
        self.scratch.reconfigure_effect_execution(request.grant.effect_session);
        self.scratch.bind_effect_execution_generation(generation);
        let mut completions = Vec::with_capacity(request.items.len());
        for item in request.items {
            stop_if_requested(&mut checkpoint)?;
            stop_if_requested(&mut checkpoint)?;
            let completion_result =
                self.scratch.execute_prepared_heterogeneous_cpu_prefix_with_checkpoint(
                    item.route.work(),
                    generation,
                    item.input.rgba_f32().data.as_slice(),
                    item.frame_seed,
                    item.working_color_space,
                    &mut checkpoint,
                );
            let completion = match completion_result {
                Ok(completion) => completion,
                Err(PreparedHeterogeneousEffectWorkError::ExecutionStopped { reason, .. }) => {
                    return Err(HeterogeneousCpuPrefixBatchError::Stopped { reason });
                }
                Err(source) => {
                    return Err(HeterogeneousCpuPrefixBatchError::Execute {
                        address: item.address,
                        source,
                    });
                }
            };
            stop_if_requested(&mut checkpoint)?;
            completions
                .push(HeterogeneousCpuPrefixBatchCompletion { address: item.address, completion });
        }
        Ok(HeterogeneousCpuPrefixBatchOutput { completions: completions.into_boxed_slice() })
    }
}

fn stop_if_requested(
    checkpoint: &mut impl FnMut() -> Option<HeterogeneousCpuExecutionStopReason>,
) -> Result<(), HeterogeneousCpuPrefixBatchError> {
    match checkpoint() {
        Some(reason) => Err(HeterogeneousCpuPrefixBatchError::Stopped { reason }),
        None => Ok(()),
    }
}

fn validate_batch_request(
    request: &HeterogeneousCpuPrefixBatchRequest,
) -> Result<(), HeterogeneousCpuPrefixBatchError> {
    if request.items.is_empty() {
        return Err(HeterogeneousCpuPrefixBatchError::EmptyBatch);
    }
    if request.items.len() > request.grant.max_batch_items {
        return Err(HeterogeneousCpuPrefixBatchError::BatchItemLimitExceeded {
            actual: request.items.len(),
            limit: request.grant.max_batch_items,
        });
    }
    let mut addresses = HashSet::with_capacity(request.items.len());
    let mut retained_pixel_bytes = 0_u64;
    for item in &request.items {
        if !addresses.insert(item.address) {
            return Err(HeterogeneousCpuPrefixBatchError::DuplicateAddress {
                address: item.address,
            });
        }
        validate_working_frame(item).map_err(|violation| {
            HeterogeneousCpuPrefixBatchError::FrameContract { address: item.address, violation }
        })?;
        let descriptor = item.input.descriptor();
        let actual_extent = EffectFrameExtent::new(descriptor.width, descriptor.height);
        if item.route.frame_extent() != actual_extent {
            return Err(HeterogeneousCpuPrefixBatchError::PreparedExtentMismatch {
                address: item.address,
                prepared: item.route.frame_extent(),
                actual: actual_extent,
            });
        }
        if item.route.budget() != request.grant.graph_execution {
            return Err(HeterogeneousCpuPrefixBatchError::PreparedBudgetMismatch {
                address: item.address,
            });
        }
        let frame_bytes = u64::from(descriptor.width)
            .checked_mul(u64::from(descriptor.height))
            .and_then(|pixels| {
                pixels
                    .checked_mul(u64::try_from(std::mem::size_of::<[f32; 4]>()).unwrap_or(u64::MAX))
            })
            .ok_or(HeterogeneousCpuPrefixBatchError::BatchPixelBytesOverflow)?;
        retained_pixel_bytes = retained_pixel_bytes
            .checked_add(frame_bytes)
            .and_then(|bytes| bytes.checked_add(frame_bytes))
            .ok_or(HeterogeneousCpuPrefixBatchError::BatchPixelBytesOverflow)?;
    }
    if retained_pixel_bytes > request.grant.max_batch_pixel_bytes {
        return Err(HeterogeneousCpuPrefixBatchError::BatchPixelBytesExceeded {
            required: retained_pixel_bytes,
            limit: request.grant.max_batch_pixel_bytes,
        });
    }
    Ok(())
}

fn validate_working_frame(
    item: &HeterogeneousCpuPrefixBatchItem,
) -> Result<(), HeterogeneousCpuPrefixFrameContractViolation> {
    let descriptor = item.input.descriptor();
    if descriptor.domain != ColorFrameDomain::Working {
        return Err(HeterogeneousCpuPrefixFrameContractViolation::Domain {
            actual: descriptor.domain,
        });
    }
    if descriptor.encoding != ColorFrameEncoding::LinearFloat {
        return Err(HeterogeneousCpuPrefixFrameContractViolation::Encoding {
            actual: descriptor.encoding,
        });
    }
    if descriptor.residency != ColorFrameResidency::Cpu {
        return Err(HeterogeneousCpuPrefixFrameContractViolation::Residency {
            actual: descriptor.residency,
        });
    }
    if descriptor.color_space != ColorFrameSpace::Working(item.working_color_space) {
        return Err(HeterogeneousCpuPrefixFrameContractViolation::ColorSpace {
            expected: item.working_color_space,
            actual: descriptor.color_space,
        });
    }
    let frame = item.input.rgba_f32();
    if frame.color_space != item.working_color_space {
        return Err(
            HeterogeneousCpuPrefixFrameContractViolation::PixelColorSpace {
                expected: item.working_color_space,
                actual: frame.color_space,
            },
        );
    }
    if !descriptor.alpha.is_straight_compatible() {
        return Err(HeterogeneousCpuPrefixFrameContractViolation::Alpha {
            actual: descriptor.alpha,
        });
    }
    let expected_pixels = usize::try_from(frame.width).ok().and_then(|width| {
        usize::try_from(frame.height).ok().and_then(|height| width.checked_mul(height))
    });
    if descriptor.width != frame.width
        || descriptor.height != frame.height
        || expected_pixels != Some(frame.data.len())
    {
        return Err(
            HeterogeneousCpuPrefixFrameContractViolation::PayloadExtent {
                descriptor_width: descriptor.width,
                descriptor_height: descriptor.height,
                payload_width: frame.width,
                payload_height: frame.height,
                payload_pixels: frame.data.len(),
            },
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::{
        effect_data::{EffectNode, EffectType},
        PropertyValue, TimelineTime, WorkingRgbaF32Frame,
    };
    use mondrian_effects::{EffectNodeExt, PreparedEffectProgram};

    const EXTENT: EffectFrameExtent = EffectFrameExtent::new(3, 2);
    const WORKING_SPACE: WorkingColorSpace = WorkingColorSpace::LinearRec2020;

    fn grant() -> HeterogeneousCpuPrefixBatchGrant {
        HeterogeneousCpuPrefixBatchGrant::new(
            EffectExecutionSessionConfig::uncached(16 * 1024 * 1024),
            EffectGraphExecutionBudget::new(
                16 * 1024 * 1024,
                16 * 1024 * 1024,
                16 * 1024 * 1024,
                32,
                64,
            ),
            4,
            16 * 1024 * 1024,
        )
    }

    fn graph() -> Arc<CompiledEffectGraph> {
        let mut correction = EffectNode::with_defaults(EffectType::BasicCorrection);
        correction
            .set_static_value_by_parameter(
                &EffectType::BasicCorrection
                    .parameter_id("exposure")
                    .expect("built-in exposure parameter"),
                PropertyValue::Float(0.25),
            )
            .expect("enable Basic Correction GPU operation");
        let mut grain = EffectNode::with_defaults(EffectType::Grain);
        grain
            .set_static_value_by_parameter(
                &EffectType::Grain
                    .parameter_id("amount")
                    .expect("built-in grain amount parameter"),
                PropertyValue::Float(0.1),
            )
            .expect("enable Grain GPU operation");
        let effects = [
            EffectNode::with_defaults(EffectType::GaussianBlur),
            correction,
            grain,
        ];
        PreparedEffectProgram::prepare(&effects, &[], WORKING_SPACE)
            .expect("prepare graph")
            .evaluate(TimelineTime::ZERO)
            .expect("compile graph")
    }

    fn item(address: u32) -> HeterogeneousCpuPrefixBatchItem {
        let grant = grant();
        let route =
            PreparedHeterogeneousEffectRoute::prepare(graph(), EXTENT, grant.graph_execution())
                .expect("prepare heterogeneous route");
        HeterogeneousCpuPrefixBatchItem::new(
            address,
            route,
            CpuColorFrame::working(WorkingRgbaF32Frame {
                width: EXTENT.width(),
                height: EXTENT.height(),
                color_space: WORKING_SPACE,
                data: vec![[0.2, 0.4, 0.6, 1.0]; 6],
            }),
            WORKING_SPACE,
            19,
        )
    }

    #[test]
    fn validation_rejects_duplicate_addresses_before_execution() {
        let request = HeterogeneousCpuPrefixBatchRequest::new(grant(), vec![item(7), item(7)]);
        assert!(matches!(
            request.validate(),
            Err(HeterogeneousCpuPrefixBatchError::DuplicateAddress { address: 7 })
        ));
    }

    #[test]
    fn stopped_atomic_batch_returns_no_partial_completion() {
        let request = HeterogeneousCpuPrefixBatchRequest::new(grant(), vec![item(1), item(2)]);
        let mut checkpoints = 0;
        let error = HeterogeneousCpuPrefixBatchExecutor::default()
            .execute(request, 11, || {
                checkpoints += 1;
                (checkpoints > 20).then_some(HeterogeneousCpuExecutionStopReason::Canceled)
            })
            .expect_err("controlled stop must discard the complete local batch");
        assert!(matches!(
            error,
            HeterogeneousCpuPrefixBatchError::Stopped {
                reason: HeterogeneousCpuExecutionStopReason::Canceled
            }
        ));
    }

    #[test]
    fn successful_batch_preserves_addresses_and_working_space_evidence() {
        let request = HeterogeneousCpuPrefixBatchRequest::new(grant(), vec![item(3), item(9)]);
        let output = HeterogeneousCpuPrefixBatchExecutor::default()
            .execute(request, 13, || None)
            .expect("execute CPU prefixes");
        assert_eq!(
            output
                .completions()
                .iter()
                .map(HeterogeneousCpuPrefixBatchCompletion::address)
                .collect::<Vec<_>>(),
            vec![3, 9]
        );
        assert!(output.completions().iter().all(|completion| {
            completion.completion().evidence().working_color_space() == WORKING_SPACE
        }));
    }

    #[test]
    fn validation_rejects_route_prepared_under_a_different_budget() {
        let request_grant = grant();
        let different_budget = EffectGraphExecutionBudget::new(
            8 * 1024 * 1024,
            8 * 1024 * 1024,
            8 * 1024 * 1024,
            32,
            64,
        );
        let route = PreparedHeterogeneousEffectRoute::prepare(graph(), EXTENT, different_budget)
            .expect("prepare route under different authority");
        let item = HeterogeneousCpuPrefixBatchItem::new(
            5,
            route,
            CpuColorFrame::working(WorkingRgbaF32Frame {
                width: EXTENT.width(),
                height: EXTENT.height(),
                color_space: WORKING_SPACE,
                data: vec![[0.2, 0.4, 0.6, 1.0]; 6],
            }),
            WORKING_SPACE,
            19,
        );
        let request = HeterogeneousCpuPrefixBatchRequest::new(request_grant, vec![item]);
        assert!(matches!(
            request.validate(),
            Err(HeterogeneousCpuPrefixBatchError::PreparedBudgetMismatch { address: 5 })
        ));
    }
}
