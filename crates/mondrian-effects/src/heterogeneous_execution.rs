//! Graph-value-aware heterogeneous Effect planning and executable CPU→GPU
//! handoff preparation.
//!
//! This Module derives every dispatch, transfer, completion dependency, and
//! release directly from one immutable [`crate::CompiledEffectGraph`]. It does
//! not introduce a second semantic graph. The current executable vertical
//! slice accepts a caller-supplied scene-linear Float32 CPU frame, executes an
//! exact plan-driven CPU DAG prefix, and prepares an exact GPU DAG suffix from
//! fused point chains plus admitted joins. Preview/Export scheduling remains
//! outside this Module.

use crate::{
    adjustment::{
        apply_render_op_f32_controlled, render_op_f32_scratch_frames, EffectRasterRegion,
    },
    execution::{apply_alpha_mask_f32_region_controlled, blend_rgba_f32_region_controlled},
    lower_effect_graph_node_to_gpu_point_plan, lower_effect_graph_nodes_to_gpu_plan,
    mask_raster::{ControlledMaskRasterError, PreparedMaskRasterSet},
    CompiledEffectGpuPlan, CompiledEffectGraph, EffectColorDomain, EffectExecutionEnvironment,
    EffectExecutionLane, EffectExecutionLaneId, EffectExecutionModes, EffectExecutionSession,
    EffectFrameExtent, EffectGpuPlanBlocker, EffectGraphNodeId, EffectGraphNodeKind,
    EffectProcessingBackend, EffectResourceLifetime, EffectStateModel, EffectTemporalInputExtent,
    EffectWorkingPrecision,
};
use mondrian_core::{types::BlendMode, WorkingColorSpace};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

/// Exact representation of one graph value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EffectValueFormat {
    precision: EffectWorkingPrecision,
    domain: EffectColorDomain,
}

impl EffectValueFormat {
    /// Construct one exact representation.
    pub const fn new(precision: EffectWorkingPrecision, domain: EffectColorDomain) -> Self {
        Self { precision, domain }
    }

    /// Sample representation.
    pub const fn precision(self) -> EffectWorkingPrecision {
        self.precision
    }

    /// Processing domain carried by the samples.
    pub const fn domain(self) -> EffectColorDomain {
        self.domain
    }
}

/// Exact lane and representation containing one graph value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EffectValueResidency {
    lane: EffectExecutionLaneId,
    format: EffectValueFormat,
}

impl EffectValueResidency {
    /// Construct one exact residency.
    pub const fn new(lane: EffectExecutionLaneId, format: EffectValueFormat) -> Self {
        Self { lane, format }
    }

    /// Owning execution lane.
    pub const fn lane(self) -> EffectExecutionLaneId {
        self.lane
    }

    /// Exact representation.
    pub const fn format(self) -> EffectValueFormat {
        self.format
    }
}

/// Stable plan-local identity of one concrete value materialization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EffectMaterializationId(u32);

impl EffectMaterializationId {
    /// Numeric plan-local identity for evidence and tests.
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Stable plan-local completion dependency.
///
/// Token zero denotes the externally supplied source value. Every dispatched
/// or transferred materialization receives one unique nonzero token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EffectCompletionToken(u32);

impl EffectCompletionToken {
    /// Numeric plan-local identity for evidence and Adapter submission.
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Checked resource limits for one graph-value execution plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectGraphExecutionBudget {
    max_host_bytes: u64,
    max_device_bytes: u64,
    max_transfer_bytes: u64,
    max_materializations: usize,
    max_steps: usize,
}

impl EffectGraphExecutionBudget {
    /// Construct exact frame-local plan limits.
    pub const fn new(
        max_host_bytes: u64,
        max_device_bytes: u64,
        max_transfer_bytes: u64,
        max_materializations: usize,
        max_steps: usize,
    ) -> Self {
        Self {
            max_host_bytes,
            max_device_bytes,
            max_transfer_bytes,
            max_materializations,
            max_steps,
        }
    }

    /// Maximum simultaneously live host bytes.
    pub const fn max_host_bytes(self) -> u64 {
        self.max_host_bytes
    }

    /// Maximum simultaneously live GPU-device bytes.
    pub const fn max_device_bytes(self) -> u64 {
        self.max_device_bytes
    }

    /// Maximum aggregate bytes copied by explicit transfer steps.
    pub const fn max_transfer_bytes(self) -> u64 {
        self.max_transfer_bytes
    }

    /// Maximum concrete materializations, including the source endpoint.
    pub const fn max_materializations(self) -> usize {
        self.max_materializations
    }

    /// Maximum Dispatch, Transfer, and Release steps.
    pub const fn max_steps(self) -> usize {
        self.max_steps
    }
}

/// One exact request for graph-value planning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectGraphExecutionRequest {
    frame_extent: EffectFrameExtent,
    input_residency: EffectValueResidency,
    output_residency: EffectValueResidency,
    budget: EffectGraphExecutionBudget,
}

impl EffectGraphExecutionRequest {
    /// Construct one immutable request.
    pub const fn new(
        frame_extent: EffectFrameExtent,
        input_residency: EffectValueResidency,
        output_residency: EffectValueResidency,
        budget: EffectGraphExecutionBudget,
    ) -> Self {
        Self {
            frame_extent,
            input_residency,
            output_residency,
            budget,
        }
    }

    /// Complete frame extent represented by every graph value.
    pub const fn frame_extent(self) -> EffectFrameExtent {
        self.frame_extent
    }

    /// Caller-provided source residency.
    pub const fn input_residency(self) -> EffectValueResidency {
        self.input_residency
    }

    /// Required final output residency.
    pub const fn output_residency(self) -> EffectValueResidency {
        self.output_residency
    }

    /// Frame-local resource limits.
    pub const fn budget(self) -> EffectGraphExecutionBudget {
        self.budget
    }
}

/// One concrete materialization retained by the plan's liveness model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectValueMaterialization {
    id: EffectMaterializationId,
    value: EffectGraphNodeId,
    residency: EffectValueResidency,
    completion: EffectCompletionToken,
    bytes: u64,
}

impl EffectValueMaterialization {
    /// Plan-local identity.
    pub const fn id(self) -> EffectMaterializationId {
        self.id
    }

    /// Semantic value from the unique compiled graph.
    pub const fn value(self) -> EffectGraphNodeId {
        self.value
    }

    /// Concrete lane and representation.
    pub const fn residency(self) -> EffectValueResidency {
        self.residency
    }

    /// Token proving that the producer completed this materialization.
    pub const fn completion(self) -> EffectCompletionToken {
        self.completion
    }

    /// Conservatively retained bytes.
    pub const fn bytes(self) -> u64 {
        self.bytes
    }
}

/// One ordered executable graph-value action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectGraphExecutionStep {
    /// Execute exactly one compiled graph node.
    Dispatch {
        /// Semantic node from the unique compiled graph.
        node: EffectGraphNodeId,
        /// Selected execution lane.
        lane: EffectExecutionLaneId,
        /// Concrete backend owned by that lane.
        backend: EffectProcessingBackend,
        /// Exact working representation.
        precision: EffectWorkingPrecision,
        /// Input materializations in the node's semantic input order.
        inputs: Arc<[EffectMaterializationId]>,
        /// New output materialization.
        output: EffectMaterializationId,
        /// Unique producer-completion dependencies.
        waits: Arc<[EffectCompletionToken]>,
        /// Output completion token.
        signal: EffectCompletionToken,
    },
    /// Copy or convert one graph value through an explicitly declared transfer.
    Transfer {
        /// Semantic graph value retained across the transfer.
        value: EffectGraphNodeId,
        /// Existing source materialization.
        input: EffectMaterializationId,
        /// New destination materialization.
        output: EffectMaterializationId,
        /// Exact source residency.
        from: EffectValueResidency,
        /// Exact destination residency.
        to: EffectValueResidency,
        /// Source producer completion.
        wait: EffectCompletionToken,
        /// Destination completion.
        signal: EffectCompletionToken,
    },
    /// Retire one materialization immediately after its final use.
    Release {
        /// Materialization whose last consumer has completed.
        materialization: EffectMaterializationId,
        /// Completion token of the final consumer. Plan order alone never
        /// authorizes an asynchronous Adapter to free the allocation. The
        /// Adapter must complete this retirement before advancing to the next
        /// plan step so the checked live-set budget remains authoritative.
        after: EffectCompletionToken,
    },
}

/// Immutable graph-value execution plan.
///
/// The plan contains scheduling and resource evidence only. Pixel semantics
/// remain exclusively in the referenced `CompiledEffectGraph`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledEffectValueExecutionPlan {
    graph_fingerprint: [u8; 32],
    frame_extent: EffectFrameExtent,
    input_value: EffectGraphNodeId,
    output_value: EffectGraphNodeId,
    input_materialization: EffectMaterializationId,
    output_materialization: EffectMaterializationId,
    materializations: Arc<[EffectValueMaterialization]>,
    steps: Arc<[EffectGraphExecutionStep]>,
    total_cost: u64,
    peak_host_bytes: u64,
    peak_device_bytes: u64,
    peak_device_materializations: u64,
    transfer_bytes: u64,
}

impl CompiledEffectValueExecutionPlan {
    /// Complete pixel-semantic fingerprint of the compiled graph.
    pub const fn graph_fingerprint(&self) -> [u8; 32] {
        self.graph_fingerprint
    }

    /// Complete frame extent represented by every materialization.
    pub const fn frame_extent(&self) -> EffectFrameExtent {
        self.frame_extent
    }

    /// Unique graph source value.
    pub const fn input_value(&self) -> EffectGraphNodeId {
        self.input_value
    }

    /// Unique graph output value.
    pub const fn output_value(&self) -> EffectGraphNodeId {
        self.output_value
    }

    /// Caller-provided source materialization.
    pub const fn input_materialization(&self) -> EffectMaterializationId {
        self.input_materialization
    }

    /// Materialization satisfying the requested endpoint.
    pub const fn output_materialization(&self) -> EffectMaterializationId {
        self.output_materialization
    }

    /// Every concrete materialization named by the plan.
    pub fn materializations(&self) -> &[EffectValueMaterialization] {
        &self.materializations
    }

    /// Deterministic Dispatch/Transfer/Release sequence.
    pub fn steps(&self) -> &[EffectGraphExecutionStep] {
        &self.steps
    }

    /// Sum of caller-defined dispatch and transfer costs.
    pub const fn total_cost(&self) -> u64 {
        self.total_cost
    }

    /// Checked maximum simultaneously live host bytes.
    pub const fn peak_host_bytes(&self) -> u64 {
        self.peak_host_bytes
    }

    /// Checked maximum simultaneously live GPU-device bytes.
    pub const fn peak_device_bytes(&self) -> u64 {
        self.peak_device_bytes
    }

    /// Exact maximum number of simultaneously live GPU-device values.
    ///
    /// This is measured from the verified liveness set and must be used for
    /// resource-count admission; byte totals cannot conservatively infer it
    /// when U8, F16, and F32 materializations coexist.
    pub const fn peak_device_materializations(&self) -> u64 {
        self.peak_device_materializations
    }

    /// Checked aggregate bytes copied by Transfer steps.
    pub const fn transfer_bytes(&self) -> u64 {
        self.transfer_bytes
    }

    /// Look up one plan-local materialization.
    pub fn materialization(
        &self,
        id: EffectMaterializationId,
    ) -> Option<EffectValueMaterialization> {
        self.materializations.get(id.0 as usize).copied()
    }
}

/// Resource dimension that exceeded its explicit budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EffectGraphExecutionBudgetKind {
    /// Simultaneously live CPU/external host memory.
    HostBytes,
    /// Simultaneously live GPU-device memory.
    DeviceBytes,
    /// Aggregate transfer traffic.
    TransferBytes,
    /// Number of value materializations.
    Materializations,
    /// Number of executable/lifetime steps.
    Steps,
}

/// Why a graph-value execution plan could not be produced safely.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EffectGraphExecutionPlanError {
    /// The graph did not contain exactly one reachable source.
    #[error("effect graph requires exactly one reachable source; observed {actual}")]
    InvalidSourceCount {
        /// Reachable source-node count.
        actual: usize,
    },
    /// The compiled graph has no output.
    #[error("effect graph has no output value")]
    MissingOutput,
    /// The compiled graph retained an invalid color-domain crossing.
    #[error("effect graph contains {blockers} blocked color-domain edges")]
    ColorDomainBlocked {
        /// Number of blockers.
        blockers: usize,
    },
    /// No color-domain transfer executor was explicitly supplied.
    #[error("effect graph requires {transitions} color-domain transitions")]
    ColorDomainTransitionsUnsupported {
        /// Number of explicit transitions.
        transitions: usize,
    },
    /// This current-frame planner cannot own continuity state.
    #[error("heterogeneous current-frame execution requires stateless effects")]
    StatefulExecutionUnsupported,
    /// This current-frame planner has no owner for mutable continuity resources.
    #[error("heterogeneous current-frame execution cannot own continuity-session resources")]
    ContinuityResourceUnsupported,
    /// This current-frame planner cannot fetch temporal inputs.
    #[error("heterogeneous current-frame execution requires temporal input {extent:?}")]
    TemporalInputUnsupported {
        /// Required input extent.
        extent: EffectTemporalInputExtent,
    },
    /// An endpoint references an absent lane.
    #[error("effect endpoint references unknown lane {lane:?}")]
    UnknownEndpointLane {
        /// Missing lane.
        lane: EffectExecutionLaneId,
    },
    /// An endpoint requests a representation absent from its lane.
    #[error("effect endpoint lane {lane:?} does not support {precision:?}")]
    UnsupportedEndpointPrecision {
        /// Endpoint lane.
        lane: EffectExecutionLaneId,
        /// Requested representation.
        precision: EffectWorkingPrecision,
    },
    /// The current planner has no concrete executor for the endpoint backend.
    #[error("effect endpoint lane {lane:?} uses unsupported backend {backend:?}")]
    UnsupportedEndpointBackend {
        /// Endpoint lane.
        lane: EffectExecutionLaneId,
        /// Unsupported backend.
        backend: EffectProcessingBackend,
    },
    /// Endpoint domain does not match the unique graph value.
    #[error("effect endpoint for value {value:?} declares {actual:?}, expected {expected:?}")]
    EndpointDomainMismatch {
        /// Source or output graph value.
        value: EffectGraphNodeId,
        /// Compiled value domain.
        expected: EffectColorDomain,
        /// Caller-declared domain.
        actual: EffectColorDomain,
    },
    /// No concrete lane supports one node's exact admitted modes.
    #[error("effect node {node:?} has no executable lane for exact modes {modes:?}")]
    NoExecutionLane {
        /// Unplaceable graph node.
        node: EffectGraphNodeId,
        /// Implementation/Definition mode intersection.
        modes: EffectExecutionModes,
    },
    /// Node-local lanes exist, but its inputs cannot reach any of them.
    #[error("effect node {node:?} cannot receive all graph values through declared transfers")]
    NoValueRoute {
        /// Unreachable graph node.
        node: EffectGraphNodeId,
    },
    /// Final graph output cannot reach the requested endpoint.
    #[error("effect output {value:?} cannot reach its requested residency")]
    NoOutputRoute {
        /// Compiled output value.
        value: EffectGraphNodeId,
    },
    /// Graph, materialization, or token identity exceeded its checked range.
    #[error("effect heterogeneous plan identity overflowed")]
    IdentityOverflow,
    /// Pixel-byte or relative-cost arithmetic overflowed.
    #[error("effect heterogeneous plan arithmetic overflowed")]
    ArithmeticOverflow,
    /// One explicit resource limit was exceeded.
    #[error("effect heterogeneous plan exceeded {kind:?}: required {required}, limit {limit}")]
    BudgetExceeded {
        /// Exhausted resource.
        kind: EffectGraphExecutionBudgetKind,
        /// Checked requirement.
        required: u64,
        /// Configured limit.
        limit: u64,
    },
    /// Internal plan verification found an impossible dependency/lifetime.
    #[error("effect heterogeneous plan verification failed: {reason}")]
    InvalidDerivedPlan {
        /// Stable internal invariant label.
        reason: &'static str,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct TransferState {
    lane: EffectExecutionLaneId,
    precision: EffectWorkingPrecision,
}

impl TransferState {
    fn from_residency(residency: EffectValueResidency) -> Self {
        Self {
            lane: residency.lane,
            precision: residency.format.precision,
        }
    }
}

#[derive(Debug, Clone)]
struct TransferPath {
    cost: u64,
    transfer_indices: Vec<usize>,
    visited: Vec<TransferState>,
}

#[derive(Debug, Clone)]
struct ExistingValueRoute {
    source: EffectMaterializationId,
    cost: u64,
    transfer_indices: Vec<usize>,
}

#[derive(Debug, Clone)]
struct NodePlacement {
    lane_index: usize,
    precision: EffectWorkingPrecision,
    selection_cost: u64,
}

struct EffectGraphValuePlanner<'a> {
    compiled: &'a CompiledEffectGraph,
    environment: &'a EffectExecutionEnvironment,
    request: EffectGraphExecutionRequest,
    source_value: EffectGraphNodeId,
    output_value: EffectGraphNodeId,
    materializations: Vec<EffectValueMaterialization>,
    by_value: HashMap<EffectGraphNodeId, Vec<EffectMaterializationId>>,
    raw_steps: Vec<EffectGraphExecutionStep>,
    total_cost: u64,
    arithmetic_overflowed: bool,
}

impl<'a> EffectGraphValuePlanner<'a> {
    fn new(
        compiled: &'a CompiledEffectGraph,
        environment: &'a EffectExecutionEnvironment,
        request: EffectGraphExecutionRequest,
    ) -> Result<Self, EffectGraphExecutionPlanError> {
        validate_current_frame_graph(compiled)?;
        let source_values = compiled
            .schedule()
            .ordered_nodes
            .iter()
            .copied()
            .filter(|node_id| {
                matches!(
                    compiled.graph().node(*node_id).map(|node| &node.kind),
                    Some(EffectGraphNodeKind::Source)
                )
            })
            .collect::<Vec<_>>();
        if source_values.len() != 1 {
            return Err(EffectGraphExecutionPlanError::InvalidSourceCount {
                actual: source_values.len(),
            });
        }
        let source_value = source_values[0];
        let output_value =
            compiled.graph().output.ok_or(EffectGraphExecutionPlanError::MissingOutput)?;
        validate_endpoint(compiled, environment, source_value, request.input_residency)?;
        validate_endpoint(
            compiled,
            environment,
            output_value,
            request.output_residency,
        )?;

        let source_bytes = frame_bytes(
            request.frame_extent,
            request.input_residency.format.precision,
        )?;
        enforce_count_budget(
            1,
            request.budget.max_materializations,
            EffectGraphExecutionBudgetKind::Materializations,
        )?;
        let source = EffectValueMaterialization {
            id: EffectMaterializationId(0),
            value: source_value,
            residency: request.input_residency,
            completion: EffectCompletionToken(0),
            bytes: source_bytes,
        };
        Ok(Self {
            compiled,
            environment,
            request,
            source_value,
            output_value,
            materializations: vec![source],
            by_value: HashMap::from([(source_value, vec![source.id])]),
            raw_steps: Vec::new(),
            total_cost: 0,
            arithmetic_overflowed: false,
        })
    }

    fn build(mut self) -> Result<CompiledEffectValueExecutionPlan, EffectGraphExecutionPlanError> {
        let compiled = self.compiled;
        for node_id in compiled.schedule().ordered_nodes.iter().copied() {
            let node = compiled.graph().node(node_id).ok_or(
                EffectGraphExecutionPlanError::InvalidDerivedPlan {
                    reason: "scheduled_node_missing",
                },
            )?;
            if matches!(&node.kind, EffectGraphNodeKind::Source) {
                continue;
            }
            let input_values = node.input_ids();
            let placement = self.choose_node_placement(node_id, &input_values)?;
            let lane = self.environment.lanes()[placement.lane_index];
            let mut routed_by_value = HashMap::new();
            let mut routed_values = HashSet::new();
            for value in input_values.iter().copied() {
                if !routed_values.insert(value) {
                    continue;
                }
                let domain = self.value_domain(value)?;
                let residency = EffectValueResidency::new(
                    lane.id(),
                    EffectValueFormat::new(placement.precision, domain),
                );
                let materialization =
                    self.ensure_materialization(value, residency).map_err(|error| match error {
                        EffectGraphExecutionPlanError::NoOutputRoute { .. } => {
                            EffectGraphExecutionPlanError::NoValueRoute { node: node_id }
                        }
                        other => other,
                    })?;
                routed_by_value.insert(value, materialization);
            }
            let inputs =
                input_values.iter().map(|value| routed_by_value[value]).collect::<Vec<_>>();
            let waits = unique_waits(&inputs, &self.materializations)?;
            let output_domain = self.value_domain(node_id)?;
            let output_residency = EffectValueResidency::new(
                lane.id(),
                EffectValueFormat::new(placement.precision, output_domain),
            );
            let output = self.allocate_materialization(node_id, output_residency)?;
            let signal = self.materialization(output)?.completion;
            self.push_raw_step(EffectGraphExecutionStep::Dispatch {
                node: node_id,
                lane: lane.id(),
                backend: lane.backend(),
                precision: placement.precision,
                inputs: inputs.into(),
                output,
                waits: waits.into(),
                signal,
            })?;
            self.by_value.entry(node_id).or_default().push(output);
            self.total_cost = self
                .total_cost
                .checked_add(u64::from(lane.dispatch_cost()))
                .ok_or(EffectGraphExecutionPlanError::ArithmeticOverflow)?;
        }

        let output_materialization = self
            .ensure_materialization(self.output_value, self.request.output_residency)
            .map_err(|error| match error {
                EffectGraphExecutionPlanError::NoOutputRoute { .. } => {
                    EffectGraphExecutionPlanError::NoOutputRoute { value: self.output_value }
                }
                other => other,
            })?;
        let steps = insert_last_use_releases(
            &self.raw_steps,
            &self.materializations,
            output_materialization,
        )?;
        enforce_count_budget(
            self.materializations.len(),
            self.request.budget.max_materializations,
            EffectGraphExecutionBudgetKind::Materializations,
        )?;
        enforce_count_budget(
            steps.len(),
            self.request.budget.max_steps,
            EffectGraphExecutionBudgetKind::Steps,
        )?;
        if self.arithmetic_overflowed {
            return Err(EffectGraphExecutionPlanError::ArithmeticOverflow);
        }
        let usage = verify_and_measure_plan(
            self.compiled,
            self.environment,
            &self.materializations,
            &steps,
            EffectMaterializationId(0),
            output_materialization,
            self.request.budget,
        )?;
        Ok(CompiledEffectValueExecutionPlan {
            graph_fingerprint: self.compiled.semantic_fingerprint(),
            frame_extent: self.request.frame_extent,
            input_value: self.source_value,
            output_value: self.output_value,
            input_materialization: EffectMaterializationId(0),
            output_materialization,
            materializations: self.materializations.into(),
            steps: steps.into(),
            total_cost: self.total_cost,
            peak_host_bytes: usage.peak_host_bytes,
            peak_device_bytes: usage.peak_device_bytes,
            peak_device_materializations: usage.peak_device_materializations,
            transfer_bytes: usage.transfer_bytes,
        })
    }

    fn choose_node_placement(
        &mut self,
        node_id: EffectGraphNodeId,
        input_values: &[EffectGraphNodeId],
    ) -> Result<NodePlacement, EffectGraphExecutionPlanError> {
        let modes = self
            .compiled
            .node_execution_modes(node_id)
            .unwrap_or(EffectExecutionModes::NONE);
        let mut has_lane = false;
        let mut best = None::<NodePlacement>;
        let environment = self.environment;
        for (lane_index, lane) in environment.lanes().iter().copied().enumerate() {
            if !supported_planner_backend(lane.backend()) {
                continue;
            }
            for precision in ordered_precisions() {
                if !lane.supported_precisions().contains(precision)
                    || !modes.contains(lane.backend(), precision)
                {
                    continue;
                }
                has_lane = true;
                let mut cost = u64::from(lane.dispatch_cost());
                let mut reachable = true;
                let mut seen_values = HashSet::new();
                for value in input_values.iter().copied() {
                    if !seen_values.insert(value) {
                        continue;
                    }
                    let domain = self.value_domain(value)?;
                    let target = EffectValueResidency::new(
                        lane.id(),
                        EffectValueFormat::new(precision, domain),
                    );
                    let Some(route) = self.route_existing_value(value, target) else {
                        reachable = false;
                        break;
                    };
                    let Some(next) = cost.checked_add(route.cost) else {
                        self.arithmetic_overflowed = true;
                        reachable = false;
                        break;
                    };
                    cost = next;
                }
                if !reachable {
                    continue;
                }
                if node_id == self.output_value {
                    let output_domain = self.value_domain(node_id)?;
                    let produced = EffectValueResidency::new(
                        lane.id(),
                        EffectValueFormat::new(precision, output_domain),
                    );
                    let Some(path) =
                        transfer_path(self.environment, produced, self.request.output_residency)
                    else {
                        continue;
                    };
                    let Some(next) = cost.checked_add(path.cost) else {
                        self.arithmetic_overflowed = true;
                        continue;
                    };
                    cost = next;
                }
                let candidate = NodePlacement { lane_index, precision, selection_cost: cost };
                if best
                    .as_ref()
                    .is_none_or(|current| candidate.selection_cost < current.selection_cost)
                {
                    best = Some(candidate);
                }
            }
        }
        best.ok_or({
            if !has_lane {
                EffectGraphExecutionPlanError::NoExecutionLane { node: node_id, modes }
            } else if self.arithmetic_overflowed {
                EffectGraphExecutionPlanError::ArithmeticOverflow
            } else {
                EffectGraphExecutionPlanError::NoValueRoute { node: node_id }
            }
        })
    }

    fn ensure_materialization(
        &mut self,
        value: EffectGraphNodeId,
        target: EffectValueResidency,
    ) -> Result<EffectMaterializationId, EffectGraphExecutionPlanError> {
        let route = self
            .route_existing_value(value, target)
            .ok_or(EffectGraphExecutionPlanError::NoOutputRoute { value })?;
        let mut current = route.source;
        for transfer_index in route.transfer_indices {
            let transfer = self.environment.transfers()[transfer_index];
            let source = self.materialization(current)?;
            let next_residency = EffectValueResidency::new(
                transfer.to_lane(),
                EffectValueFormat::new(transfer.to_precision(), source.residency.format.domain),
            );
            if let Some(existing) = self.exact_materialization(value, next_residency) {
                current = existing;
                continue;
            }
            let output = self.allocate_materialization(value, next_residency)?;
            let destination = self.materialization(output)?;
            self.push_raw_step(EffectGraphExecutionStep::Transfer {
                value,
                input: current,
                output,
                from: source.residency,
                to: destination.residency,
                wait: source.completion,
                signal: destination.completion,
            })?;
            self.by_value.entry(value).or_default().push(output);
            self.total_cost = self
                .total_cost
                .checked_add(u64::from(transfer.cost()))
                .ok_or(EffectGraphExecutionPlanError::ArithmeticOverflow)?;
            current = output;
        }
        if self.materialization(current)?.residency != target {
            return Err(EffectGraphExecutionPlanError::NoOutputRoute { value });
        }
        Ok(current)
    }

    fn route_existing_value(
        &self,
        value: EffectGraphNodeId,
        target: EffectValueResidency,
    ) -> Option<ExistingValueRoute> {
        let mut best = None::<ExistingValueRoute>;
        for source in self.by_value.get(&value)?.iter().copied() {
            let materialization = self.materializations.get(source.0 as usize)?;
            let Some(path) = transfer_path(self.environment, materialization.residency, target)
            else {
                continue;
            };
            let candidate = ExistingValueRoute {
                source,
                cost: path.cost,
                transfer_indices: path.transfer_indices,
            };
            if best.as_ref().is_none_or(|current| {
                (
                    candidate.cost,
                    candidate.source,
                    candidate.transfer_indices.as_slice(),
                ) < (
                    current.cost,
                    current.source,
                    current.transfer_indices.as_slice(),
                )
            }) {
                best = Some(candidate);
            }
        }
        best
    }

    fn exact_materialization(
        &self,
        value: EffectGraphNodeId,
        residency: EffectValueResidency,
    ) -> Option<EffectMaterializationId> {
        self.by_value.get(&value)?.iter().copied().find(|id| {
            self.materializations
                .get(id.0 as usize)
                .is_some_and(|materialization| materialization.residency == residency)
        })
    }

    fn allocate_materialization(
        &mut self,
        value: EffectGraphNodeId,
        residency: EffectValueResidency,
    ) -> Result<EffectMaterializationId, EffectGraphExecutionPlanError> {
        let required = self
            .materializations
            .len()
            .checked_add(1)
            .ok_or(EffectGraphExecutionPlanError::ArithmeticOverflow)?;
        enforce_count_budget(
            required,
            self.request.budget.max_materializations,
            EffectGraphExecutionBudgetKind::Materializations,
        )?;
        let index = u32::try_from(self.materializations.len())
            .map_err(|_| EffectGraphExecutionPlanError::IdentityOverflow)?;
        let completion = index;
        let bytes = frame_bytes(self.request.frame_extent, residency.format.precision)?;
        let id = EffectMaterializationId(index);
        self.materializations.push(EffectValueMaterialization {
            id,
            value,
            residency,
            completion: EffectCompletionToken(completion),
            bytes,
        });
        Ok(id)
    }

    fn push_raw_step(
        &mut self,
        step: EffectGraphExecutionStep,
    ) -> Result<(), EffectGraphExecutionPlanError> {
        let required = self
            .raw_steps
            .len()
            .checked_add(1)
            .ok_or(EffectGraphExecutionPlanError::ArithmeticOverflow)?;
        enforce_count_budget(
            required,
            self.request.budget.max_steps,
            EffectGraphExecutionBudgetKind::Steps,
        )?;
        self.raw_steps.push(step);
        Ok(())
    }

    fn materialization(
        &self,
        id: EffectMaterializationId,
    ) -> Result<EffectValueMaterialization, EffectGraphExecutionPlanError> {
        self.materializations.get(id.0 as usize).copied().ok_or(
            EffectGraphExecutionPlanError::InvalidDerivedPlan { reason: "materialization_missing" },
        )
    }

    fn value_domain(
        &self,
        value: EffectGraphNodeId,
    ) -> Result<EffectColorDomain, EffectGraphExecutionPlanError> {
        self.compiled.domain_plan().node_output_domains.get(&value).copied().ok_or(
            EffectGraphExecutionPlanError::InvalidDerivedPlan { reason: "value_domain_missing" },
        )
    }
}

/// Derive one deterministic graph-value-aware execution plan.
pub fn plan_effect_graph_value_execution(
    compiled: &CompiledEffectGraph,
    environment: &EffectExecutionEnvironment,
    request: EffectGraphExecutionRequest,
) -> Result<CompiledEffectValueExecutionPlan, EffectGraphExecutionPlanError> {
    EffectGraphValuePlanner::new(compiled, environment, request)?.build()
}

impl CompiledEffectGraph {
    /// Derive one deterministic graph-value-aware execution plan.
    pub fn plan_heterogeneous_execution(
        &self,
        environment: &EffectExecutionEnvironment,
        request: EffectGraphExecutionRequest,
    ) -> Result<CompiledEffectValueExecutionPlan, EffectGraphExecutionPlanError> {
        plan_effect_graph_value_execution(self, environment, request)
    }
}

fn validate_current_frame_graph(
    compiled: &CompiledEffectGraph,
) -> Result<(), EffectGraphExecutionPlanError> {
    if !compiled.domain_plan().blockers.is_empty() {
        return Err(EffectGraphExecutionPlanError::ColorDomainBlocked {
            blockers: compiled.domain_plan().blockers.len(),
        });
    }
    if !compiled.domain_plan().transitions.is_empty() {
        return Err(
            EffectGraphExecutionPlanError::ColorDomainTransitionsUnsupported {
                transitions: compiled.domain_plan().transitions.len(),
            },
        );
    }
    let aggregate = compiled.execution_envelope().aggregate();
    if aggregate.state_model != EffectStateModel::Stateless {
        return Err(EffectGraphExecutionPlanError::StatefulExecutionUnsupported);
    }
    if aggregate.resource_lifetime == EffectResourceLifetime::ContinuitySession {
        return Err(EffectGraphExecutionPlanError::ContinuityResourceUnsupported);
    }
    if aggregate.temporal_input != EffectTemporalInputExtent::CURRENT_FRAME {
        return Err(EffectGraphExecutionPlanError::TemporalInputUnsupported {
            extent: aggregate.temporal_input,
        });
    }
    Ok(())
}

fn validate_endpoint(
    compiled: &CompiledEffectGraph,
    environment: &EffectExecutionEnvironment,
    value: EffectGraphNodeId,
    residency: EffectValueResidency,
) -> Result<(), EffectGraphExecutionPlanError> {
    let lane = lane_by_id(environment, residency.lane)
        .ok_or(EffectGraphExecutionPlanError::UnknownEndpointLane { lane: residency.lane })?;
    if !lane.supported_precisions().contains(residency.format.precision) {
        return Err(
            EffectGraphExecutionPlanError::UnsupportedEndpointPrecision {
                lane: residency.lane,
                precision: residency.format.precision,
            },
        );
    }
    if !supported_planner_backend(lane.backend()) {
        return Err(EffectGraphExecutionPlanError::UnsupportedEndpointBackend {
            lane: residency.lane,
            backend: lane.backend(),
        });
    }
    let expected = compiled.domain_plan().node_output_domains.get(&value).copied().ok_or(
        EffectGraphExecutionPlanError::InvalidDerivedPlan { reason: "endpoint_domain_missing" },
    )?;
    if expected != residency.format.domain {
        return Err(EffectGraphExecutionPlanError::EndpointDomainMismatch {
            value,
            expected,
            actual: residency.format.domain,
        });
    }
    Ok(())
}

fn supported_planner_backend(backend: EffectProcessingBackend) -> bool {
    matches!(
        backend,
        EffectProcessingBackend::Cpu | EffectProcessingBackend::Gpu
    )
}

fn lane_by_id(
    environment: &EffectExecutionEnvironment,
    lane_id: EffectExecutionLaneId,
) -> Option<EffectExecutionLane> {
    environment.lanes().iter().copied().find(|lane| lane.id() == lane_id)
}

fn ordered_precisions() -> [EffectWorkingPrecision; 3] {
    [
        EffectWorkingPrecision::NormalizedU8,
        EffectWorkingPrecision::Float16,
        EffectWorkingPrecision::Float32,
    ]
}

fn transfer_path(
    environment: &EffectExecutionEnvironment,
    from: EffectValueResidency,
    to: EffectValueResidency,
) -> Option<TransferPath> {
    if from.format.domain != to.format.domain {
        return None;
    }
    let from_state = TransferState::from_residency(from);
    let to_state = TransferState::from_residency(to);
    if from_state == to_state {
        return Some(TransferPath {
            cost: 0,
            transfer_indices: Vec::new(),
            visited: vec![from_state],
        });
    }
    let mut states = Vec::new();
    for lane in environment.lanes().iter().copied() {
        for precision in ordered_precisions() {
            if lane.supported_precisions().contains(precision) {
                states.push(TransferState { lane: lane.id(), precision });
            }
        }
    }
    let from_index = states.iter().position(|state| *state == from_state)?;
    let to_index = states.iter().position(|state| *state == to_state)?;
    let mut best = vec![None::<TransferPath>; states.len()];
    best[from_index] = Some(TransferPath {
        cost: 0,
        transfer_indices: Vec::new(),
        visited: vec![from_state],
    });
    for _ in 0..states.len() {
        let previous = best.clone();
        let mut changed = false;
        for (transfer_index, transfer) in environment.transfers().iter().copied().enumerate() {
            let transfer_from = TransferState {
                lane: transfer.from_lane(),
                precision: transfer.from_precision(),
            };
            let transfer_to = TransferState {
                lane: transfer.to_lane(),
                precision: transfer.to_precision(),
            };
            let Some(source_index) = states.iter().position(|state| *state == transfer_from) else {
                continue;
            };
            let Some(destination_index) = states.iter().position(|state| *state == transfer_to)
            else {
                continue;
            };
            let Some(path) = previous[source_index].as_ref() else {
                continue;
            };
            if path.visited.contains(&transfer_to) {
                continue;
            }
            let Some(cost) = path.cost.checked_add(u64::from(transfer.cost())) else {
                continue;
            };
            let mut candidate = path.clone();
            candidate.cost = cost;
            candidate.transfer_indices.push(transfer_index);
            candidate.visited.push(transfer_to);
            if best[destination_index].as_ref().is_none_or(|current| {
                (candidate.cost, candidate.transfer_indices.as_slice())
                    < (current.cost, current.transfer_indices.as_slice())
            }) {
                best[destination_index] = Some(candidate);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    best[to_index].clone()
}

fn frame_bytes(
    extent: EffectFrameExtent,
    precision: EffectWorkingPrecision,
) -> Result<u64, EffectGraphExecutionPlanError> {
    let bytes_per_pixel = match precision {
        EffectWorkingPrecision::NormalizedU8 => 4_u64,
        EffectWorkingPrecision::Float16 => 8_u64,
        EffectWorkingPrecision::Float32 => 16_u64,
    };
    u64::from(extent.width())
        .checked_mul(u64::from(extent.height()))
        .and_then(|pixels| pixels.checked_mul(bytes_per_pixel))
        .ok_or(EffectGraphExecutionPlanError::ArithmeticOverflow)
}

fn unique_waits(
    inputs: &[EffectMaterializationId],
    materializations: &[EffectValueMaterialization],
) -> Result<Vec<EffectCompletionToken>, EffectGraphExecutionPlanError> {
    let mut waits = Vec::new();
    for input in inputs {
        let completion = materializations
            .get(input.0 as usize)
            .map(|materialization| materialization.completion)
            .ok_or(EffectGraphExecutionPlanError::InvalidDerivedPlan {
                reason: "dispatch_input_missing",
            })?;
        if !waits.contains(&completion) {
            waits.push(completion);
        }
    }
    Ok(waits)
}

fn step_inputs(step: &EffectGraphExecutionStep) -> &[EffectMaterializationId] {
    match step {
        EffectGraphExecutionStep::Dispatch { inputs, .. } => inputs,
        EffectGraphExecutionStep::Transfer { input, .. } => std::slice::from_ref(input),
        EffectGraphExecutionStep::Release { .. } => &[],
    }
}

fn step_signal(step: &EffectGraphExecutionStep) -> Option<EffectCompletionToken> {
    match step {
        EffectGraphExecutionStep::Dispatch { signal, .. }
        | EffectGraphExecutionStep::Transfer { signal, .. } => Some(*signal),
        EffectGraphExecutionStep::Release { .. } => None,
    }
}

fn insert_last_use_releases(
    raw_steps: &[EffectGraphExecutionStep],
    materializations: &[EffectValueMaterialization],
    output: EffectMaterializationId,
) -> Result<Vec<EffectGraphExecutionStep>, EffectGraphExecutionPlanError> {
    let mut last_use = vec![None::<usize>; materializations.len()];
    for (step_index, step) in raw_steps.iter().enumerate() {
        for input in step_inputs(step) {
            let slot = last_use.get_mut(input.0 as usize).ok_or(
                EffectGraphExecutionPlanError::InvalidDerivedPlan {
                    reason: "last_use_input_missing",
                },
            )?;
            *slot = Some(step_index);
        }
    }
    let mut releases = vec![Vec::<EffectMaterializationId>::new(); raw_steps.len()];
    for materialization in materializations {
        if materialization.id == output {
            continue;
        }
        let Some(step_index) = last_use[materialization.id.0 as usize] else {
            return Err(EffectGraphExecutionPlanError::InvalidDerivedPlan {
                reason: "non_output_materialization_has_no_consumer",
            });
        };
        releases[step_index].push(materialization.id);
    }
    let mut steps = Vec::with_capacity(
        raw_steps
            .len()
            .checked_add(materializations.len().saturating_sub(1))
            .ok_or(EffectGraphExecutionPlanError::ArithmeticOverflow)?,
    );
    for (step_index, step) in raw_steps.iter().enumerate() {
        steps.push(step.clone());
        let after = step_signal(step).ok_or(EffectGraphExecutionPlanError::InvalidDerivedPlan {
            reason: "raw_step_has_no_completion_signal",
        })?;
        releases[step_index].sort_unstable();
        for materialization in releases[step_index].iter().copied() {
            steps.push(EffectGraphExecutionStep::Release { materialization, after });
        }
    }
    Ok(steps)
}

#[derive(Debug, Clone, Copy)]
struct PlanResourceUsage {
    peak_host_bytes: u64,
    peak_device_bytes: u64,
    peak_device_materializations: u64,
    transfer_bytes: u64,
}

fn verify_and_measure_plan(
    compiled: &CompiledEffectGraph,
    environment: &EffectExecutionEnvironment,
    materializations: &[EffectValueMaterialization],
    steps: &[EffectGraphExecutionStep],
    input: EffectMaterializationId,
    output: EffectMaterializationId,
    budget: EffectGraphExecutionBudget,
) -> Result<PlanResourceUsage, EffectGraphExecutionPlanError> {
    for (index, materialization) in materializations.iter().copied().enumerate() {
        if materialization.id.0 as usize != index
            || materialization.completion.0 != materialization.id.0
            || compiled.graph().node(materialization.value).is_none()
        {
            return Err(EffectGraphExecutionPlanError::InvalidDerivedPlan {
                reason: "materialization_identity_or_value_invalid",
            });
        }
    }
    let mut live = HashSet::<EffectMaterializationId>::new();
    let mut host_bytes = 0_u64;
    let mut device_bytes = 0_u64;
    let mut peak_host_bytes = 0_u64;
    let mut peak_device_bytes = 0_u64;
    let mut device_materializations = 0_u64;
    let mut peak_device_materializations = 0_u64;
    let mut transfer_bytes = 0_u64;
    let mut last_submitted_signal = None;
    let mut last_submitted_inputs = Vec::new();
    let source = materializations.get(input.0 as usize).ok_or(
        EffectGraphExecutionPlanError::InvalidDerivedPlan {
            reason: "source_materialization_missing",
        },
    )?;
    if source.value
        != compiled
            .schedule()
            .ordered_nodes
            .iter()
            .copied()
            .find(|node_id| {
                matches!(
                    compiled.graph().node(*node_id).map(|node| &node.kind),
                    Some(EffectGraphNodeKind::Source)
                )
            })
            .ok_or(EffectGraphExecutionPlanError::InvalidDerivedPlan {
                reason: "source_value_missing_during_verification",
            })?
    {
        return Err(EffectGraphExecutionPlanError::InvalidDerivedPlan {
            reason: "source_materialization_value_mismatch",
        });
    }
    add_live_bytes(environment, *source, &mut host_bytes, &mut device_bytes)?;
    if materialization_uses_device(environment, *source)? {
        device_materializations = 1;
    }
    live.insert(input);
    peak_host_bytes = peak_host_bytes.max(host_bytes);
    peak_device_bytes = peak_device_bytes.max(device_bytes);
    peak_device_materializations = peak_device_materializations.max(device_materializations);

    for step in steps {
        match step {
            EffectGraphExecutionStep::Dispatch {
                node,
                lane,
                backend,
                precision,
                inputs,
                output: step_output,
                waits,
                signal,
                ..
            } => {
                for input in inputs.iter() {
                    if !live.contains(input) {
                        return Err(EffectGraphExecutionPlanError::InvalidDerivedPlan {
                            reason: "dispatch_use_before_ready_or_after_release",
                        });
                    }
                }
                let expected_waits = unique_waits(inputs, materializations)?;
                if expected_waits.as_slice() != waits.as_ref() {
                    return Err(EffectGraphExecutionPlanError::InvalidDerivedPlan {
                        reason: "dispatch_wait_set_mismatch",
                    });
                }
                let produced = materializations.get(step_output.0 as usize).ok_or(
                    EffectGraphExecutionPlanError::InvalidDerivedPlan {
                        reason: "dispatch_output_missing",
                    },
                )?;
                let graph_node = compiled.graph().node(*node).ok_or(
                    EffectGraphExecutionPlanError::InvalidDerivedPlan {
                        reason: "dispatch_node_missing",
                    },
                )?;
                let expected_inputs = graph_node.input_ids();
                let input_values = inputs
                    .iter()
                    .map(|input| {
                        materializations
                            .get(input.0 as usize)
                            .map(|materialization| materialization.value)
                            .ok_or(EffectGraphExecutionPlanError::InvalidDerivedPlan {
                                reason: "dispatch_input_materialization_missing",
                            })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let lane_contract = lane_by_id(environment, *lane).ok_or(
                    EffectGraphExecutionPlanError::InvalidDerivedPlan {
                        reason: "dispatch_lane_missing",
                    },
                )?;
                let expected_domain =
                    compiled.domain_plan().node_output_domains.get(node).copied().ok_or(
                        EffectGraphExecutionPlanError::InvalidDerivedPlan {
                            reason: "dispatch_output_domain_missing",
                        },
                    )?;
                if expected_inputs != input_values
                    || produced.value != *node
                    || produced.residency.lane != *lane
                    || produced.residency.format.precision != *precision
                    || produced.residency.format.domain != expected_domain
                    || lane_contract.backend() != *backend
                    || !lane_contract.supported_precisions().contains(*precision)
                    || !compiled
                        .node_execution_modes(*node)
                        .is_some_and(|modes| modes.contains(*backend, *precision))
                    || produced.completion != *signal
                    || !live.insert(*step_output)
                {
                    return Err(EffectGraphExecutionPlanError::InvalidDerivedPlan {
                        reason: "dispatch_contract_or_output_invalid",
                    });
                }
                add_live_bytes(environment, *produced, &mut host_bytes, &mut device_bytes)?;
                if materialization_uses_device(environment, *produced)? {
                    device_materializations = device_materializations
                        .checked_add(1)
                        .ok_or(EffectGraphExecutionPlanError::ArithmeticOverflow)?;
                }
                last_submitted_signal = Some(*signal);
                last_submitted_inputs.clear();
                last_submitted_inputs.extend_from_slice(inputs);
            }
            EffectGraphExecutionStep::Transfer {
                value,
                input: step_input,
                output: step_output,
                from,
                to,
                wait,
                signal,
            } => {
                if !live.contains(step_input) {
                    return Err(EffectGraphExecutionPlanError::InvalidDerivedPlan {
                        reason: "transfer_use_before_ready_or_after_release",
                    });
                }
                let source = materializations.get(step_input.0 as usize).ok_or(
                    EffectGraphExecutionPlanError::InvalidDerivedPlan {
                        reason: "transfer_input_missing",
                    },
                )?;
                let destination = materializations.get(step_output.0 as usize).ok_or(
                    EffectGraphExecutionPlanError::InvalidDerivedPlan {
                        reason: "transfer_output_missing",
                    },
                )?;
                if source.value != *value
                    || destination.value != *value
                    || source.residency != *from
                    || destination.residency != *to
                    || from.format.domain != to.format.domain
                    || !environment.transfers().iter().any(|transfer| {
                        transfer.from_lane() == from.lane
                            && transfer.from_precision() == from.format.precision
                            && transfer.to_lane() == to.lane
                            && transfer.to_precision() == to.format.precision
                    })
                    || source.completion != *wait
                    || destination.completion != *signal
                    || !live.insert(*step_output)
                {
                    return Err(EffectGraphExecutionPlanError::InvalidDerivedPlan {
                        reason: "transfer_contract_mismatch",
                    });
                }
                transfer_bytes = transfer_bytes
                    .checked_add(destination.bytes)
                    .ok_or(EffectGraphExecutionPlanError::ArithmeticOverflow)?;
                add_live_bytes(
                    environment,
                    *destination,
                    &mut host_bytes,
                    &mut device_bytes,
                )?;
                if materialization_uses_device(environment, *destination)? {
                    device_materializations = device_materializations
                        .checked_add(1)
                        .ok_or(EffectGraphExecutionPlanError::ArithmeticOverflow)?;
                }
                last_submitted_signal = Some(*signal);
                last_submitted_inputs.clear();
                last_submitted_inputs.push(*step_input);
            }
            EffectGraphExecutionStep::Release { materialization, after } => {
                if Some(*after) != last_submitted_signal
                    || !last_submitted_inputs.contains(materialization)
                    || *materialization == output
                    || !live.remove(materialization)
                {
                    return Err(EffectGraphExecutionPlanError::InvalidDerivedPlan {
                        reason: "invalid_duplicate_or_unordered_release",
                    });
                }
                let released = materializations.get(materialization.0 as usize).ok_or(
                    EffectGraphExecutionPlanError::InvalidDerivedPlan {
                        reason: "release_materialization_missing",
                    },
                )?;
                remove_live_bytes(environment, *released, &mut host_bytes, &mut device_bytes)?;
                if materialization_uses_device(environment, *released)? {
                    device_materializations = device_materializations.checked_sub(1).ok_or(
                        EffectGraphExecutionPlanError::InvalidDerivedPlan {
                            reason: "live_device_materialization_accounting_underflow",
                        },
                    )?;
                }
            }
        }
        peak_host_bytes = peak_host_bytes.max(host_bytes);
        peak_device_bytes = peak_device_bytes.max(device_bytes);
        peak_device_materializations = peak_device_materializations.max(device_materializations);
    }
    if live != HashSet::from([output]) {
        return Err(EffectGraphExecutionPlanError::InvalidDerivedPlan {
            reason: "terminal_live_set_is_not_exact_output",
        });
    }
    let final_output = materializations.get(output.0 as usize).ok_or(
        EffectGraphExecutionPlanError::InvalidDerivedPlan {
            reason: "terminal_output_materialization_missing",
        },
    )?;
    if compiled.graph().output != Some(final_output.value) {
        return Err(EffectGraphExecutionPlanError::InvalidDerivedPlan {
            reason: "terminal_output_value_mismatch",
        });
    }
    enforce_byte_budget(
        peak_host_bytes,
        budget.max_host_bytes,
        EffectGraphExecutionBudgetKind::HostBytes,
    )?;
    enforce_byte_budget(
        peak_device_bytes,
        budget.max_device_bytes,
        EffectGraphExecutionBudgetKind::DeviceBytes,
    )?;
    enforce_byte_budget(
        transfer_bytes,
        budget.max_transfer_bytes,
        EffectGraphExecutionBudgetKind::TransferBytes,
    )?;
    Ok(PlanResourceUsage {
        peak_host_bytes,
        peak_device_bytes,
        peak_device_materializations,
        transfer_bytes,
    })
}

fn materialization_uses_device(
    environment: &EffectExecutionEnvironment,
    materialization: EffectValueMaterialization,
) -> Result<bool, EffectGraphExecutionPlanError> {
    let lane = lane_by_id(environment, materialization.residency.lane).ok_or(
        EffectGraphExecutionPlanError::InvalidDerivedPlan {
            reason: "live_materialization_lane_missing",
        },
    )?;
    Ok(lane.backend() == EffectProcessingBackend::Gpu)
}

fn add_live_bytes(
    environment: &EffectExecutionEnvironment,
    materialization: EffectValueMaterialization,
    host_bytes: &mut u64,
    device_bytes: &mut u64,
) -> Result<(), EffectGraphExecutionPlanError> {
    let lane = lane_by_id(environment, materialization.residency.lane).ok_or(
        EffectGraphExecutionPlanError::InvalidDerivedPlan {
            reason: "live_materialization_lane_missing",
        },
    )?;
    let target = if lane.backend() == EffectProcessingBackend::Gpu {
        device_bytes
    } else {
        host_bytes
    };
    *target = target
        .checked_add(materialization.bytes)
        .ok_or(EffectGraphExecutionPlanError::ArithmeticOverflow)?;
    Ok(())
}

fn remove_live_bytes(
    environment: &EffectExecutionEnvironment,
    materialization: EffectValueMaterialization,
    host_bytes: &mut u64,
    device_bytes: &mut u64,
) -> Result<(), EffectGraphExecutionPlanError> {
    let lane = lane_by_id(environment, materialization.residency.lane).ok_or(
        EffectGraphExecutionPlanError::InvalidDerivedPlan {
            reason: "released_materialization_lane_missing",
        },
    )?;
    let target = if lane.backend() == EffectProcessingBackend::Gpu {
        device_bytes
    } else {
        host_bytes
    };
    *target = target.checked_sub(materialization.bytes).ok_or(
        EffectGraphExecutionPlanError::InvalidDerivedPlan {
            reason: "live_byte_accounting_underflow",
        },
    )?;
    Ok(())
}

fn enforce_count_budget(
    required: usize,
    limit: usize,
    kind: EffectGraphExecutionBudgetKind,
) -> Result<(), EffectGraphExecutionPlanError> {
    if required > limit {
        return Err(EffectGraphExecutionPlanError::BudgetExceeded {
            kind,
            required: u64::try_from(required).unwrap_or(u64::MAX),
            limit: u64::try_from(limit).unwrap_or(u64::MAX),
        });
    }
    Ok(())
}

fn enforce_byte_budget(
    required: u64,
    limit: u64,
    kind: EffectGraphExecutionBudgetKind,
) -> Result<(), EffectGraphExecutionPlanError> {
    if required > limit {
        return Err(EffectGraphExecutionPlanError::BudgetExceeded { kind, required, limit });
    }
    Ok(())
}

/// Exact CPU completion and pending GPU handoff facts for one frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeterogeneousCpuCompletionEvidence {
    graph_fingerprint: [u8; 32],
    generation: u64,
    frame_extent: EffectFrameExtent,
    frame_seed: i64,
    working_color_space: WorkingColorSpace,
    completed_cpu_nodes: Arc<[EffectGraphNodeId]>,
    transfers: Arc<[HeterogeneousCpuTransferEvidence]>,
    pending_output_token: EffectCompletionToken,
}

/// One exact CPU-frontier value and the pending upload token chain proved by
/// heterogeneous preparation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeterogeneousCpuTransferEvidence {
    cpu_materialization: EffectMaterializationId,
    gpu_materialization: EffectMaterializationId,
    format: EffectValueFormat,
    completed_cpu_token: EffectCompletionToken,
    pending_gpu_input_token: EffectCompletionToken,
}

impl HeterogeneousCpuTransferEvidence {
    /// CPU-resident graph-value materialization returned by the prefix.
    pub const fn cpu_materialization(self) -> EffectMaterializationId {
        self.cpu_materialization
    }

    /// GPU-resident materialization that only the exact upload may publish.
    pub const fn gpu_materialization(self) -> EffectMaterializationId {
        self.gpu_materialization
    }

    /// Exact non-converting value format carried across the transfer.
    pub const fn format(self) -> EffectValueFormat {
        self.format
    }

    /// CPU producer token that the upload must wait for.
    pub const fn completed_cpu_token(self) -> EffectCompletionToken {
        self.completed_cpu_token
    }

    /// Token that only the completed upload may signal.
    pub const fn pending_gpu_input_token(self) -> EffectCompletionToken {
        self.pending_gpu_input_token
    }
}

/// One executable GPU DAG dispatch prepared from the unique graph-value plan.
///
/// Point chains are renderer-neutral shader programs. Blend and ordered
/// MultiInput joins retain their exact materializations, authored mode, and
/// opacity. Other graph-node semantics remain blocked until a production GPU
/// Adapter exists.
#[derive(Debug, Clone)]
pub enum PreparedHeterogeneousGpuDispatch {
    /// One fused unary point chain. A linear whole suffix keeps the existing
    /// one-pass path; a DAG branch can use a one-node chain.
    PointChain {
        /// Exact semantic nodes represented by this dispatch.
        nodes: Arc<[EffectGraphNodeId]>,
        /// Existing GPU materialization consumed by the chain.
        input: EffectMaterializationId,
        /// New GPU materialization produced by the chain.
        output: EffectMaterializationId,
        /// Unique completion dependencies.
        waits: Arc<[EffectCompletionToken]>,
        /// Completion token proved by this dispatch.
        signal: EffectCompletionToken,
        /// Backend-neutral point program.
        plan: Arc<CompiledEffectGpuPlan>,
    },
    /// Bit-preserving materialization copy for an identity-shaped semantic
    /// node whose output must retain a distinct graph-value lifetime.
    Copy {
        /// Exact semantic node represented by the copy.
        node: EffectGraphNodeId,
        /// Existing GPU materialization copied by the Adapter.
        input: EffectMaterializationId,
        /// New GPU materialization with an independent lifetime.
        output: EffectMaterializationId,
        /// Producer-completion dependency.
        waits: Arc<[EffectCompletionToken]>,
        /// Completion token proved by the copy command.
        signal: EffectCompletionToken,
    },
    /// Canonical straight-alpha BlendMode join in scene-linear working space.
    Blend {
        /// Exact semantic Blend node.
        node: EffectGraphNodeId,
        /// Base materialization.
        base: EffectMaterializationId,
        /// Overlay materialization.
        overlay: EffectMaterializationId,
        /// New joined materialization.
        output: EffectMaterializationId,
        /// Both producer-completion dependencies.
        waits: Arc<[EffectCompletionToken]>,
        /// Completion token proved by the join.
        signal: EffectCompletionToken,
        /// Authored straight-alpha opacity.
        opacity: f32,
        /// Authored BlendMode evaluated by the shared CPU/GPU algebra.
        blend_mode: BlendMode,
    },
    /// Ordered N-input straight-alpha composition in scene-linear working
    /// space. At least two inputs are required by this GPU execution shape.
    MultiInput {
        /// Exact semantic MultiInput node.
        node: EffectGraphNodeId,
        /// Input materializations in authored evaluation order.
        inputs: Arc<[EffectMaterializationId]>,
        /// New joined materialization.
        output: EffectMaterializationId,
        /// Producer-completion dependencies in the same order as `inputs`.
        waits: Arc<[EffectCompletionToken]>,
        /// Completion token proved after the final ordered blend.
        signal: EffectCompletionToken,
        /// Authored straight-alpha opacity applied at every fold step.
        opacity: f32,
        /// Authored BlendMode evaluated by the shared CPU/GPU algebra.
        blend_mode: BlendMode,
    },
    /// Apply one scalar alpha-mask materialization to a scene-linear input.
    Mask {
        /// Exact semantic Mask node.
        node: EffectGraphNodeId,
        /// Scene-linear input materialization whose RGB is preserved.
        input: EffectMaterializationId,
        /// AlphaMask-domain materialization sampled from its alpha channel.
        mask: EffectMaterializationId,
        /// New scene-linear materialization with updated straight alpha.
        output: EffectMaterializationId,
        /// Both producer-completion dependencies in input/mask order.
        waits: Arc<[EffectCompletionToken]>,
        /// Completion token proved by the Mask dispatch.
        signal: EffectCompletionToken,
        /// Whether the sampled matte is inverted before applying the operation.
        invert: bool,
        /// Canonical authored alpha combination operation.
        mask_op: crate::mask::MaskOp,
    },
}

impl PreparedHeterogeneousGpuDispatch {
    /// New output materialization.
    pub const fn output(&self) -> EffectMaterializationId {
        match self {
            Self::PointChain { output, .. }
            | Self::Copy { output, .. }
            | Self::Blend { output, .. }
            | Self::MultiInput { output, .. }
            | Self::Mask { output, .. } => *output,
        }
    }

    /// Completion dependencies.
    pub fn waits(&self) -> &[EffectCompletionToken] {
        match self {
            Self::PointChain { waits, .. }
            | Self::Copy { waits, .. }
            | Self::Blend { waits, .. }
            | Self::MultiInput { waits, .. }
            | Self::Mask { waits, .. } => waits,
        }
    }

    /// Completion token produced by this dispatch.
    pub const fn signal(&self) -> EffectCompletionToken {
        match self {
            Self::PointChain { signal, .. }
            | Self::Copy { signal, .. }
            | Self::Blend { signal, .. }
            | Self::MultiInput { signal, .. }
            | Self::Mask { signal, .. } => *signal,
        }
    }

    fn append_nodes(&self, output: &mut Vec<EffectGraphNodeId>) {
        match self {
            Self::PointChain { nodes, .. } => output.extend(nodes.iter().copied()),
            Self::Copy { node, .. }
            | Self::Blend { node, .. }
            | Self::MultiInput { node, .. }
            | Self::Mask { node, .. } => output.push(*node),
        }
    }

    fn try_for_each_input<E>(
        &self,
        mut visit: impl FnMut(EffectMaterializationId) -> Result<(), E>,
    ) -> Result<(), E> {
        match self {
            Self::PointChain { input, .. } | Self::Copy { input, .. } => visit(*input)?,
            Self::Blend { base, overlay, .. } => {
                visit(*base)?;
                visit(*overlay)?;
            }
            Self::MultiInput { inputs, .. } => {
                for input in inputs.iter().copied() {
                    visit(input)?;
                }
            }
            Self::Mask { input, mask, .. } => {
                visit(*input)?;
                visit(*mask)?;
            }
        }
        Ok(())
    }
}

/// One GPU dispatch or exact materialization retirement.
#[derive(Debug, Clone)]
pub enum PreparedHeterogeneousGpuStep {
    /// Record one prepared GPU dispatch.
    Dispatch(PreparedHeterogeneousGpuDispatch),
    /// Retire an actual GPU materialization after its final scheduled use.
    Release {
        /// Plan-local materialization identity.
        materialization: EffectMaterializationId,
        /// Completion token of the final consumer.
        after: EffectCompletionToken,
    },
}

/// Executable GPU suffix derived from one immutable graph-value plan.
#[derive(Debug, Clone)]
pub struct PreparedHeterogeneousGpuSuffix {
    input_materializations: Arc<[EffectMaterializationId]>,
    output_materialization: EffectMaterializationId,
    steps: Arc<[PreparedHeterogeneousGpuStep]>,
    node_ids: Arc<[EffectGraphNodeId]>,
}

impl PreparedHeterogeneousGpuSuffix {
    /// Uploaded CPU-frontier materializations consumed by the suffix.
    pub fn input_materializations(&self) -> &[EffectMaterializationId] {
        &self.input_materializations
    }

    /// Final GPU materialization returned by the suffix.
    pub const fn output_materialization(&self) -> EffectMaterializationId {
        self.output_materialization
    }

    /// Deterministic dispatch/release schedule.
    pub fn steps(&self) -> &[PreparedHeterogeneousGpuStep] {
        &self.steps
    }

    /// Exact semantic GPU nodes in compiled topology order.
    pub fn node_ids(&self) -> &[EffectGraphNodeId] {
        &self.node_ids
    }
}

impl HeterogeneousCpuCompletionEvidence {
    /// Compiled graph whose CPU prefix completed.
    pub const fn graph_fingerprint(&self) -> [u8; 32] {
        self.graph_fingerprint
    }

    /// Caller-owned execution generation observed on the Session.
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Exact frame extent.
    pub const fn frame_extent(&self) -> EffectFrameExtent {
        self.frame_extent
    }

    /// Frame seed used by deterministic/frame-seeded operations.
    pub const fn frame_seed(&self) -> i64 {
        self.frame_seed
    }

    /// Exact working-space identity represented by the CPU pixels.
    pub const fn working_color_space(&self) -> WorkingColorSpace {
        self.working_color_space
    }

    /// Exact CPU nodes completed in compiled topology order.
    pub fn completed_cpu_nodes(&self) -> &[EffectGraphNodeId] {
        &self.completed_cpu_nodes
    }

    /// Exact CPU-frontier/upload token chains in deterministic plan order.
    pub fn transfers(&self) -> &[HeterogeneousCpuTransferEvidence] {
        &self.transfers
    }

    /// Final graph-output token that remains unproved.
    pub const fn pending_output_token(&self) -> EffectCompletionToken {
        self.pending_output_token
    }
}

/// CPU pixels plus the exact GPU continuation that must consume them.
///
/// This type intentionally exposes no whole-graph CPU fallback. After a caller
/// accepts this completion it must either submit the explicit transfer/GPU
/// continuation or terminate the heterogeneous attempt with typed evidence.
#[derive(Debug)]
pub struct PreparedHeterogeneousCpuCompletion {
    boundary_values: Vec<PreparedHeterogeneousCpuBoundaryValue>,
    execution_plan: Arc<CompiledEffectValueExecutionPlan>,
    gpu_suffix: Arc<PreparedHeterogeneousGpuSuffix>,
    evidence: HeterogeneousCpuCompletionEvidence,
}

/// One CPU-resident boundary frame and its exact pending GPU materialization.
#[derive(Debug)]
pub struct PreparedHeterogeneousCpuBoundaryValue {
    transfer: HeterogeneousCpuTransferEvidence,
    pixels: Vec<[f32; 4]>,
}

impl PreparedHeterogeneousCpuBoundaryValue {
    /// Exact transfer identity and token chain.
    pub const fn transfer(&self) -> HeterogeneousCpuTransferEvidence {
        self.transfer
    }

    /// Scene-linear Float32 pixels for this boundary materialization.
    pub fn pixels(&self) -> &[[f32; 4]] {
        &self.pixels
    }

    /// Consume the boundary into its transfer evidence and pixels.
    pub fn into_parts(self) -> (HeterogeneousCpuTransferEvidence, Vec<[f32; 4]>) {
        (self.transfer, self.pixels)
    }
}

/// Cooperative stop requested by the execution attempt that owns a CPU
/// prefix.
///
/// The Effect Module does not sample a global clock or infer generation
/// validity. A scheduler injects those decisions through
/// [`PreparedHeterogeneousEffectWork::execute_cpu_prefix_with_checkpoint`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeterogeneousCpuExecutionStopReason {
    /// The immutable execution generation was canceled or superseded.
    Canceled,
    /// The caller's authoritative monotonic deadline expired.
    DeadlineExpired,
}

impl PreparedHeterogeneousCpuCompletion {
    /// Atomically completed CPU-frontier values in deterministic transfer order.
    pub fn boundary_values(&self) -> &[PreparedHeterogeneousCpuBoundaryValue] {
        &self.boundary_values
    }

    /// Complete immutable graph-value plan whose CPU prefix completed.
    ///
    /// The receiving Adapter can recover the exact transfer residency,
    /// materialization identities, waits, releases, and GPU dispatch tokens
    /// without rebuilding a route.
    pub fn execution_plan(&self) -> &CompiledEffectValueExecutionPlan {
        &self.execution_plan
    }

    /// Exact GPU DAG suffix consuming the uploaded pixels.
    pub fn gpu_suffix(&self) -> &PreparedHeterogeneousGpuSuffix {
        &self.gpu_suffix
    }

    /// Completed and still-pending token evidence.
    pub const fn evidence(&self) -> &HeterogeneousCpuCompletionEvidence {
        &self.evidence
    }

    /// Consume the handoff into Adapter-owned parts.
    pub fn into_parts(
        self,
    ) -> (
        Vec<PreparedHeterogeneousCpuBoundaryValue>,
        Arc<CompiledEffectValueExecutionPlan>,
        Arc<PreparedHeterogeneousGpuSuffix>,
        HeterogeneousCpuCompletionEvidence,
    ) {
        (
            self.boundary_values,
            self.execution_plan,
            self.gpu_suffix,
            self.evidence,
        )
    }
}

/// Why the executable CPU-DAG-prefix/GPU-DAG-suffix route cannot be prepared or run.
#[derive(Debug, thiserror::Error)]
pub enum PreparedHeterogeneousEffectWorkError {
    /// Graph-value planning failed before any pixel execution.
    #[error(transparent)]
    Planning(#[from] EffectGraphExecutionPlanError),
    /// The selected graph route is valid but outside the executable vertical slice.
    #[error(
        "effect heterogeneous route is not the supported CPU-F32 DAG to GPU-F32 DAG-suffix shape: {reason}"
    )]
    UnsupportedRouteShape {
        /// Stable diagnostic label.
        reason: &'static str,
    },
    /// One exact GPU point dispatch cannot be lowered.
    #[error(transparent)]
    GpuPlan(#[from] EffectGpuPlanBlocker),
    /// Synthetic Mask geometry or raster preparation failed before CPU
    /// completion could be proved.
    #[error(transparent)]
    MaskRaster(#[from] crate::MaskRasterError),
    /// The caller did not bind the expected execution generation.
    #[error(
        "effect execution Session generation mismatch: expected {expected}, observed {observed:?}"
    )]
    SessionGenerationMismatch {
        /// Required caller generation.
        expected: u64,
        /// Current Session generation.
        observed: Option<u64>,
    },
    /// The owning execution attempt stopped cooperatively before it could
    /// prove CPU completion.
    #[error("heterogeneous CPU prefix generation {generation} stopped: {reason:?}")]
    ExecutionStopped {
        /// Caller-owned execution generation.
        generation: u64,
        /// Exact scheduler decision observed at a checkpoint.
        reason: HeterogeneousCpuExecutionStopReason,
    },
    /// Caller-supplied pixels do not match the prepared extent.
    #[error("heterogeneous CPU input contains {actual} pixels; expected {expected}")]
    InputSizeMismatch {
        /// Checked required pixels.
        expected: usize,
        /// Supplied pixels.
        actual: usize,
    },
    /// The Session's transient working budget cannot hold the CPU prefix output.
    #[error("heterogeneous CPU prefix requires {required} bytes; Session permits {limit}")]
    WorkingBudgetExceeded {
        /// Required output bytes.
        required: usize,
        /// Session limit.
        limit: usize,
    },
    /// Runtime Mask preparation exceeded the deterministic amount derived
    /// from the same immutable graph during route preparation.
    #[error(
        "heterogeneous Mask resources changed after preparation: planned={planned}, actual={actual}"
    )]
    MaskResourceContractMismatch {
        /// Conservative prepared geometry plus row scratch bytes.
        planned: usize,
        /// Runtime preparation result from the same graph.
        actual: usize,
    },
    /// A prepared CPU node disappeared or no longer forms the exact chain.
    #[error("heterogeneous CPU prefix graph contract changed at node {node:?}")]
    InvalidCpuPrefix {
        /// First invalid node.
        node: EffectGraphNodeId,
    },
    /// The scalar Float32 implementation cannot execute one selected CPU node.
    #[error("heterogeneous CPU prefix node {node:?} has no Float32 implementation")]
    UnsupportedCpuOperation {
        /// Unsupported node.
        node: EffectGraphNodeId,
    },
    /// Checked pixel-size arithmetic overflowed the host address space.
    #[error("heterogeneous CPU input size overflowed")]
    InputSizeOverflow,
}

#[derive(Debug, Clone)]
struct PreparedCpuGraphDispatch {
    node: EffectGraphNodeId,
    inputs: Arc<[EffectMaterializationId]>,
    output: EffectMaterializationId,
}

/// Reusable exact CPU-DAG-prefix/GPU-DAG-suffix work prepared from one compiled graph.
#[derive(Debug)]
pub struct PreparedHeterogeneousEffectWork {
    compiled: Arc<CompiledEffectGraph>,
    plan: Arc<CompiledEffectValueExecutionPlan>,
    cpu_nodes: Arc<[EffectGraphNodeId]>,
    cpu_dispatches: Arc<[PreparedCpuGraphDispatch]>,
    cpu_use_counts: Arc<HashMap<EffectMaterializationId, usize>>,
    cpu_input_materialization: EffectMaterializationId,
    transfers: Arc<[HeterogeneousCpuTransferEvidence]>,
    gpu_suffix: Arc<PreparedHeterogeneousGpuSuffix>,
    output_token: EffectCompletionToken,
    frame_extent: EffectFrameExtent,
    cpu_required_working_bytes: usize,
    mask_auxiliary_bytes: usize,
}

impl PreparedHeterogeneousEffectWork {
    /// Plan and prepare the currently executable CPU-F32→GPU-F32 graph route.
    ///
    /// This operation executes no pixels and performs no implicit fallback.
    pub fn prepare(
        compiled: Arc<CompiledEffectGraph>,
        environment: &EffectExecutionEnvironment,
        request: EffectGraphExecutionRequest,
    ) -> Result<Self, PreparedHeterogeneousEffectWorkError> {
        let plan = Arc::new(plan_effect_graph_value_execution(
            &compiled,
            environment,
            request,
        )?);
        let input_lane = lane_by_id(environment, request.input_residency.lane).ok_or(
            PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                reason: "input_lane_missing_after_planning",
            },
        )?;
        let output_lane = lane_by_id(environment, request.output_residency.lane).ok_or(
            PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                reason: "output_lane_missing_after_planning",
            },
        )?;
        if input_lane.backend() != EffectProcessingBackend::Cpu
            || request.input_residency.format.precision != EffectWorkingPrecision::Float32
            || output_lane.backend() != EffectProcessingBackend::Gpu
            || request.output_residency.format.precision != EffectWorkingPrecision::Float32
        {
            return Err(
                PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                    reason: "endpoints_are_not_cpu_f32_to_gpu_f32",
                },
            );
        }

        let mut cpu_nodes = Vec::new();
        let mut cpu_dispatches = Vec::new();
        let mut gpu_nodes = Vec::new();
        let mut transfers = Vec::new();
        let mut entered_gpu = false;
        for step in plan.steps() {
            match step {
                EffectGraphExecutionStep::Release { .. } => {}
                EffectGraphExecutionStep::Dispatch {
                    node,
                    backend: EffectProcessingBackend::Cpu,
                    precision: EffectWorkingPrecision::Float32,
                    inputs,
                    output,
                    ..
                } if !entered_gpu => {
                    cpu_nodes.push(*node);
                    cpu_dispatches.push(PreparedCpuGraphDispatch {
                        node: *node,
                        inputs: Arc::clone(inputs),
                        output: *output,
                    });
                }
                EffectGraphExecutionStep::Transfer {
                    input,
                    output,
                    from,
                    to,
                    wait,
                    signal,
                    ..
                } if !entered_gpu
                    && from.format.precision == EffectWorkingPrecision::Float32
                    && to.format.precision == EffectWorkingPrecision::Float32
                    && from.format == to.format
                    && lane_by_id(environment, from.lane)
                        .is_some_and(|lane| lane.backend() == EffectProcessingBackend::Cpu)
                    && lane_by_id(environment, to.lane)
                        .is_some_and(|lane| lane.backend() == EffectProcessingBackend::Gpu) =>
                {
                    transfers.push(HeterogeneousCpuTransferEvidence {
                        cpu_materialization: *input,
                        gpu_materialization: *output,
                        format: from.format,
                        completed_cpu_token: *wait,
                        pending_gpu_input_token: *signal,
                    });
                }
                EffectGraphExecutionStep::Dispatch {
                    node,
                    backend: EffectProcessingBackend::Gpu,
                    precision: EffectWorkingPrecision::Float32,
                    ..
                } => {
                    entered_gpu = true;
                    gpu_nodes.push(*node);
                }
                _ => {
                    return Err(
                        PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                            reason: "route_contains_an_unexecutable_dispatch_or_transfer",
                        },
                    );
                }
            }
        }
        if cpu_nodes.is_empty() || gpu_nodes.is_empty() {
            return Err(
                PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                    reason: "route_requires_nonempty_cpu_prefix_and_gpu_tail",
                },
            );
        }
        if transfers.is_empty() {
            return Err(
                PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                    reason: "route_requires_at_least_one_cpu_to_gpu_transfer",
                },
            );
        }
        let mut cpu_materializations = Vec::with_capacity(transfers.len());
        let mut gpu_materializations = Vec::with_capacity(transfers.len());
        let mut cpu_materialization_set = HashSet::with_capacity(transfers.len());
        let mut gpu_materialization_set = HashSet::with_capacity(transfers.len());
        for transfer in &transfers {
            let cpu_completion = plan.materialization(transfer.cpu_materialization).ok_or(
                PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                    reason: "cpu_materialization_missing",
                },
            )?;
            let gpu_input = plan.materialization(transfer.gpu_materialization).ok_or(
                PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                    reason: "gpu_input_materialization_missing",
                },
            )?;
            if cpu_completion.completion != transfer.completed_cpu_token
                || gpu_input.completion != transfer.pending_gpu_input_token
                || gpu_input.value != cpu_completion.value
                || cpu_completion.residency.format != transfer.format
                || gpu_input.residency.format != transfer.format
                || !cpu_materialization_set.insert(transfer.cpu_materialization)
                || !gpu_materialization_set.insert(transfer.gpu_materialization)
            {
                return Err(
                    PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                        reason: "cpu_to_gpu_completion_tokens_do_not_match",
                    },
                );
            }
            cpu_materializations.push(transfer.cpu_materialization);
            gpu_materializations.push(transfer.gpu_materialization);
        }
        validate_cpu_dag_gpu_tail_partition(
            &compiled,
            &plan,
            &cpu_dispatches,
            &cpu_materializations,
            &gpu_nodes,
        )?;
        let gpu_suffix = Arc::new(prepare_gpu_suffix(
            &compiled,
            &plan,
            &gpu_materializations,
            &gpu_nodes,
        )?);
        if gpu_suffix.input_materializations() != gpu_materializations {
            return Err(
                PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                    reason: "gpu_tail_does_not_consume_transferred_cpu_value",
                },
            );
        }
        let frame_extent = plan.frame_extent();
        let cpu_input_materialization = plan.input_materialization();
        let cpu_use_counts =
            cpu_materialization_use_counts(&cpu_dispatches, &cpu_materializations)?;
        let mask_auxiliary_bytes =
            PreparedMaskRasterSet::required_retained_bytes(compiled.graph())?
                .checked_add(PreparedMaskRasterSet::required_max_scratch_bytes(
                    compiled.graph(),
                )?)
                .ok_or(PreparedHeterogeneousEffectWorkError::InputSizeOverflow)?;
        let cpu_required_working_bytes = cpu_prefix_working_bytes(
            &compiled,
            &cpu_dispatches,
            &cpu_use_counts,
            cpu_input_materialization,
            &cpu_materializations,
            frame_extent,
        )?
        .checked_add(mask_auxiliary_bytes)
        .ok_or(PreparedHeterogeneousEffectWorkError::InputSizeOverflow)?;
        let output_token = plan
            .materialization(plan.output_materialization())
            .ok_or(
                PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                    reason: "output_materialization_missing",
                },
            )?
            .completion;
        Ok(Self {
            compiled,
            plan,
            cpu_nodes: cpu_nodes.into(),
            cpu_dispatches: cpu_dispatches.into(),
            cpu_use_counts: Arc::new(cpu_use_counts),
            cpu_input_materialization,
            transfers: transfers.into(),
            gpu_suffix,
            output_token,
            frame_extent,
            cpu_required_working_bytes,
            mask_auxiliary_bytes,
        })
    }

    /// Derived graph-value plan.
    pub fn plan(&self) -> &CompiledEffectValueExecutionPlan {
        &self.plan
    }

    /// Exact CPU prefix.
    pub fn cpu_nodes(&self) -> &[EffectGraphNodeId] {
        &self.cpu_nodes
    }

    /// Exact prepared GPU DAG suffix.
    pub fn gpu_suffix(&self) -> &PreparedHeterogeneousGpuSuffix {
        &self.gpu_suffix
    }

    /// Exact transient scalar bytes required by the prepared CPU prefix.
    ///
    /// This is implementation-private scratch in addition to graph-value
    /// residency. A scheduler may compare it with the bound Session before
    /// dispatch; execution repeats the check before allocating.
    pub const fn cpu_required_working_bytes(&self) -> usize {
        self.cpu_required_working_bytes
    }

    /// Execute only the prepared CPU prefix without cooperative cancellation.
    ///
    /// This wrapper is restricted to scalar references, tests, and explicitly
    /// uncancelled offline callers. Production Preview and Export must use
    /// [`Self::execute_cpu_prefix_with_checkpoint`].
    pub fn execute_cpu_prefix_uncancelled(
        &self,
        session: &EffectExecutionSession,
        generation: u64,
        input: &[[f32; 4]],
        frame_seed: i64,
        working_color_space: WorkingColorSpace,
    ) -> Result<PreparedHeterogeneousCpuCompletion, PreparedHeterogeneousEffectWorkError> {
        self.execute_cpu_prefix_with_checkpoint(
            session,
            generation,
            input,
            frame_seed,
            working_color_space,
            || None,
        )
    }

    /// Execute only the prepared CPU prefix with caller-owned cooperative
    /// cancellation/deadline checkpoints.
    ///
    /// The callback returns the exact stop reason selected by the owning
    /// execution attempt. It is invoked before allocation and at deterministic
    /// row/block boundaries in long-running scalar kernels. A stop discards the
    /// private partial buffer and never returns a CPU completion token.
    ///
    /// Success proves the CPU-frontier producer tokens. Upload and GPU-output
    /// tokens remain pending in the returned evidence. This Module never
    /// catches a suffix failure and reinterprets the complete graph on CPU.
    pub fn execute_cpu_prefix_with_checkpoint(
        &self,
        session: &EffectExecutionSession,
        generation: u64,
        input: &[[f32; 4]],
        frame_seed: i64,
        working_color_space: WorkingColorSpace,
        mut checkpoint: impl FnMut() -> Option<HeterogeneousCpuExecutionStopReason>,
    ) -> Result<PreparedHeterogeneousCpuCompletion, PreparedHeterogeneousEffectWorkError> {
        let mut checkpoint_result = || {
            checkpoint().map_or(Ok(()), |reason| {
                Err(PreparedHeterogeneousEffectWorkError::ExecutionStopped { generation, reason })
            })
        };
        checkpoint_result()?;
        let diagnostics = session.diagnostics();
        if diagnostics.generation != Some(generation) {
            return Err(
                PreparedHeterogeneousEffectWorkError::SessionGenerationMismatch {
                    expected: generation,
                    observed: diagnostics.generation,
                },
            );
        }
        let expected = usize::try_from(self.frame_extent.width())
            .ok()
            .and_then(|width| {
                usize::try_from(self.frame_extent.height())
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .ok_or(PreparedHeterogeneousEffectWorkError::InputSizeOverflow)?;
        if input.len() != expected {
            return Err(PreparedHeterogeneousEffectWorkError::InputSizeMismatch {
                expected,
                actual: input.len(),
            });
        }
        if self.cpu_required_working_bytes > diagnostics.max_working_bytes {
            return Err(
                PreparedHeterogeneousEffectWorkError::WorkingBudgetExceeded {
                    required: self.cpu_required_working_bytes,
                    limit: diagnostics.max_working_bytes,
                },
            );
        }
        checkpoint_result()?;
        let prepared_masks = PreparedMaskRasterSet::prepare_controlled(
            self.compiled.graph(),
            self.frame_extent,
            &mut checkpoint_result,
        )
        .map_err(map_controlled_mask_error)?;
        let actual_mask_auxiliary_bytes = prepared_masks
            .retained_bytes()
            .checked_add(prepared_masks.max_scratch_bytes())
            .ok_or(PreparedHeterogeneousEffectWorkError::InputSizeOverflow)?;
        if actual_mask_auxiliary_bytes > self.mask_auxiliary_bytes {
            return Err(
                PreparedHeterogeneousEffectWorkError::MaskResourceContractMismatch {
                    planned: self.mask_auxiliary_bytes,
                    actual: actual_mask_auxiliary_bytes,
                },
            );
        }
        checkpoint_result()?;
        let boundary_pixels = execute_cpu_dag_prefix(
            &self.compiled,
            &self.cpu_dispatches,
            &self.cpu_use_counts,
            self.cpu_input_materialization,
            &self.transfers,
            self.frame_extent,
            input,
            frame_seed,
            &prepared_masks,
            &mut checkpoint_result,
        )?;
        let boundary_values = self
            .transfers
            .iter()
            .copied()
            .zip(boundary_pixels)
            .map(|(transfer, pixels)| PreparedHeterogeneousCpuBoundaryValue { transfer, pixels })
            .collect();
        Ok(PreparedHeterogeneousCpuCompletion {
            boundary_values,
            execution_plan: Arc::clone(&self.plan),
            gpu_suffix: Arc::clone(&self.gpu_suffix),
            evidence: HeterogeneousCpuCompletionEvidence {
                graph_fingerprint: self.compiled.semantic_fingerprint(),
                generation,
                frame_extent: self.frame_extent,
                frame_seed,
                working_color_space,
                completed_cpu_nodes: Arc::clone(&self.cpu_nodes),
                transfers: Arc::clone(&self.transfers),
                pending_output_token: self.output_token,
            },
        })
    }
}

fn map_controlled_mask_error(
    error: ControlledMaskRasterError<PreparedHeterogeneousEffectWorkError>,
) -> PreparedHeterogeneousEffectWorkError {
    match error {
        ControlledMaskRasterError::Raster(error) => error.into(),
        ControlledMaskRasterError::Checkpoint(error) => error,
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_cpu_dag_prefix(
    compiled: &CompiledEffectGraph,
    dispatches: &[PreparedCpuGraphDispatch],
    use_counts: &HashMap<EffectMaterializationId, usize>,
    input_materialization: EffectMaterializationId,
    transfers: &[HeterogeneousCpuTransferEvidence],
    extent: EffectFrameExtent,
    input: &[[f32; 4]],
    frame_seed: i64,
    prepared_masks: &PreparedMaskRasterSet,
    checkpoint: &mut impl FnMut() -> Result<(), PreparedHeterogeneousEffectWorkError>,
) -> Result<Vec<Vec<[f32; 4]>>, PreparedHeterogeneousEffectWorkError> {
    let mut outputs = HashMap::with_capacity(dispatches.len().saturating_add(1));
    outputs.insert(
        input_materialization,
        copy_cpu_pixels_controlled(input, checkpoint)?,
    );
    let mut remaining = use_counts.clone();
    let raster_region = EffectRasterRegion::full_frame(extent.width(), extent.height());
    checkpoint()?;

    for dispatch in dispatches {
        checkpoint()?;
        let node = compiled.graph().node(dispatch.node).cloned().ok_or(
            PreparedHeterogeneousEffectWorkError::InvalidCpuPrefix { node: dispatch.node },
        )?;
        let mut inputs = dispatch
            .inputs
            .iter()
            .copied()
            .map(|materialization| {
                take_cpu_materialization(
                    &mut outputs,
                    &mut remaining,
                    materialization,
                    dispatch.node,
                    checkpoint,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;

        let output = match node.kind {
            EffectGraphNodeKind::UnaryEffect { op, .. }
            | EffectGraphNodeKind::DomainEffect { op, .. } => {
                let mut output = take_single_cpu_input(&mut inputs, dispatch.node)?;
                if !apply_render_op_f32_controlled(
                    &mut output,
                    extent.width(),
                    extent.height(),
                    &op,
                    frame_seed,
                    checkpoint,
                )? {
                    return Err(
                        PreparedHeterogeneousEffectWorkError::UnsupportedCpuOperation {
                            node: dispatch.node,
                        },
                    );
                }
                output
            }
            EffectGraphNodeKind::Blend { blend_mode, opacity, .. } => {
                if inputs.len() != 2 {
                    return Err(PreparedHeterogeneousEffectWorkError::InvalidCpuPrefix {
                        node: dispatch.node,
                    });
                }
                let overlay =
                    inputs.pop().ok_or(PreparedHeterogeneousEffectWorkError::InvalidCpuPrefix {
                        node: dispatch.node,
                    })?;
                let mut base =
                    inputs.pop().ok_or(PreparedHeterogeneousEffectWorkError::InvalidCpuPrefix {
                        node: dispatch.node,
                    })?;
                if !blend_rgba_f32_region_controlled(
                    &mut base,
                    &overlay,
                    raster_region,
                    opacity,
                    blend_mode,
                    frame_seed,
                    checkpoint,
                )? {
                    return Err(PreparedHeterogeneousEffectWorkError::InvalidCpuPrefix {
                        node: dispatch.node,
                    });
                }
                base
            }
            EffectGraphNodeKind::Mask { invert, mask_op, .. } => {
                if inputs.len() != 2 {
                    return Err(PreparedHeterogeneousEffectWorkError::InvalidCpuPrefix {
                        node: dispatch.node,
                    });
                }
                let mask =
                    inputs.pop().ok_or(PreparedHeterogeneousEffectWorkError::InvalidCpuPrefix {
                        node: dispatch.node,
                    })?;
                let mut output =
                    inputs.pop().ok_or(PreparedHeterogeneousEffectWorkError::InvalidCpuPrefix {
                        node: dispatch.node,
                    })?;
                if output.len() != mask.len() {
                    return Err(PreparedHeterogeneousEffectWorkError::InvalidCpuPrefix {
                        node: dispatch.node,
                    });
                }
                apply_alpha_mask_f32_region_controlled(
                    &mut output,
                    &mask,
                    invert,
                    mask_op,
                    checkpoint,
                )?;
                output
            }
            EffectGraphNodeKind::MultiInput { blend_mode, opacity, .. } => {
                if inputs.is_empty() {
                    return Err(PreparedHeterogeneousEffectWorkError::InvalidCpuPrefix {
                        node: dispatch.node,
                    });
                }
                let mut inputs = inputs.into_iter();
                let mut output = inputs.next().ok_or(
                    PreparedHeterogeneousEffectWorkError::InvalidCpuPrefix { node: dispatch.node },
                )?;
                for overlay in inputs {
                    if !blend_rgba_f32_region_controlled(
                        &mut output,
                        &overlay,
                        raster_region,
                        opacity,
                        blend_mode,
                        frame_seed,
                        checkpoint,
                    )? {
                        return Err(PreparedHeterogeneousEffectWorkError::InvalidCpuPrefix {
                            node: dispatch.node,
                        });
                    }
                }
                output
            }
            EffectGraphNodeKind::MaskSource { .. } => {
                if !inputs.is_empty() {
                    return Err(PreparedHeterogeneousEffectWorkError::InvalidCpuPrefix {
                        node: dispatch.node,
                    });
                }
                let raster = prepared_masks.get(dispatch.node).ok_or(
                    PreparedHeterogeneousEffectWorkError::InvalidCpuPrefix { node: dispatch.node },
                )?;
                raster
                    .rasterize_rgba_f32_controlled(extent.full_frame_roi(), checkpoint)
                    .map_err(map_controlled_mask_error)?
            }
            EffectGraphNodeKind::Source => {
                return Err(
                    PreparedHeterogeneousEffectWorkError::UnsupportedCpuOperation {
                        node: dispatch.node,
                    },
                );
            }
        };
        if outputs.insert(dispatch.output, output).is_some() {
            return Err(PreparedHeterogeneousEffectWorkError::InvalidCpuPrefix {
                node: dispatch.node,
            });
        }
    }
    checkpoint()?;
    let output_node = dispatches.last().map(|dispatch| dispatch.node).ok_or(
        PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
            reason: "cpu_dag_has_no_dispatch",
        },
    )?;
    let mut boundary_values = Vec::with_capacity(transfers.len());
    for transfer in transfers {
        let output = &transfer.cpu_materialization;
        let remaining_uses = remaining
            .get_mut(output)
            .ok_or(PreparedHeterogeneousEffectWorkError::InvalidCpuPrefix { node: output_node })?;
        if *remaining_uses != 1 {
            return Err(PreparedHeterogeneousEffectWorkError::InvalidCpuPrefix {
                node: output_node,
            });
        }
        *remaining_uses = 0;
        boundary_values.push(
            outputs.remove(output).ok_or(
                PreparedHeterogeneousEffectWorkError::InvalidCpuPrefix { node: output_node },
            )?,
        );
    }
    if !outputs.is_empty() || remaining.values().any(|remaining| *remaining != 0) {
        return Err(PreparedHeterogeneousEffectWorkError::InvalidCpuPrefix { node: output_node });
    }
    Ok(boundary_values)
}

fn take_single_cpu_input(
    inputs: &mut Vec<Vec<[f32; 4]>>,
    node: EffectGraphNodeId,
) -> Result<Vec<[f32; 4]>, PreparedHeterogeneousEffectWorkError> {
    if inputs.len() != 1 {
        return Err(PreparedHeterogeneousEffectWorkError::InvalidCpuPrefix { node });
    }
    inputs
        .pop()
        .ok_or(PreparedHeterogeneousEffectWorkError::InvalidCpuPrefix { node })
}

fn take_cpu_materialization(
    outputs: &mut HashMap<EffectMaterializationId, Vec<[f32; 4]>>,
    remaining: &mut HashMap<EffectMaterializationId, usize>,
    materialization: EffectMaterializationId,
    node: EffectGraphNodeId,
    checkpoint: &mut impl FnMut() -> Result<(), PreparedHeterogeneousEffectWorkError>,
) -> Result<Vec<[f32; 4]>, PreparedHeterogeneousEffectWorkError> {
    let remaining_uses = remaining
        .get_mut(&materialization)
        .ok_or(PreparedHeterogeneousEffectWorkError::InvalidCpuPrefix { node })?;
    if *remaining_uses == 0 {
        return Err(PreparedHeterogeneousEffectWorkError::InvalidCpuPrefix { node });
    }
    *remaining_uses -= 1;
    if *remaining_uses == 0 {
        outputs
            .remove(&materialization)
            .ok_or(PreparedHeterogeneousEffectWorkError::InvalidCpuPrefix { node })
    } else {
        let pixels = outputs
            .get(&materialization)
            .ok_or(PreparedHeterogeneousEffectWorkError::InvalidCpuPrefix { node })?;
        copy_cpu_pixels_controlled(pixels, checkpoint)
    }
}

fn copy_cpu_pixels_controlled(
    source: &[[f32; 4]],
    checkpoint: &mut impl FnMut() -> Result<(), PreparedHeterogeneousEffectWorkError>,
) -> Result<Vec<[f32; 4]>, PreparedHeterogeneousEffectWorkError> {
    let mut copy = Vec::with_capacity(source.len());
    for chunk in source.chunks(4_096) {
        checkpoint()?;
        copy.extend_from_slice(chunk);
    }
    checkpoint()?;
    Ok(copy)
}

fn cpu_prefix_working_bytes(
    compiled: &CompiledEffectGraph,
    dispatches: &[PreparedCpuGraphDispatch],
    use_counts: &HashMap<EffectMaterializationId, usize>,
    input_materialization: EffectMaterializationId,
    output_materializations: &[EffectMaterializationId],
    extent: EffectFrameExtent,
) -> Result<usize, PreparedHeterogeneousEffectWorkError> {
    let frame_bytes = usize::try_from(frame_bytes(extent, EffectWorkingPrecision::Float32)?)
        .map_err(|_| PreparedHeterogeneousEffectWorkError::InputSizeOverflow)?;
    let mut remaining = use_counts.clone();
    let mut live_materializations = HashSet::from([input_materialization]);
    let mut live_frames = 1_usize;
    let mut peak_owned_frames = live_frames;
    for dispatch in dispatches {
        let node = compiled.graph().node(dispatch.node).ok_or(
            PreparedHeterogeneousEffectWorkError::InvalidCpuPrefix { node: dispatch.node },
        )?;
        let is_mask_source = matches!(node.kind, EffectGraphNodeKind::MaskSource { .. });
        if dispatch.inputs.is_empty() && !is_mask_source {
            return Err(
                PreparedHeterogeneousEffectWorkError::UnsupportedCpuOperation {
                    node: dispatch.node,
                },
            );
        }
        for input in dispatch.inputs.iter() {
            if !live_materializations.contains(input) {
                return Err(PreparedHeterogeneousEffectWorkError::InvalidCpuPrefix {
                    node: dispatch.node,
                });
            }
            let remaining_uses = remaining.get_mut(input).ok_or(
                PreparedHeterogeneousEffectWorkError::InvalidCpuPrefix { node: dispatch.node },
            )?;
            if *remaining_uses == 0 {
                return Err(PreparedHeterogeneousEffectWorkError::InvalidCpuPrefix {
                    node: dispatch.node,
                });
            }
            *remaining_uses -= 1;
            if *remaining_uses == 0 {
                live_materializations.remove(input);
            } else {
                live_frames = live_frames
                    .checked_add(1)
                    .ok_or(PreparedHeterogeneousEffectWorkError::InputSizeOverflow)?;
                peak_owned_frames = peak_owned_frames.max(live_frames);
            }
        }

        let scratch_frames = cpu_node_scratch_frames(node)?;
        peak_owned_frames = peak_owned_frames.max(
            live_frames
                .checked_add(scratch_frames)
                .ok_or(PreparedHeterogeneousEffectWorkError::InputSizeOverflow)?,
        );
        if is_mask_source {
            live_frames = live_frames
                .checked_add(1)
                .ok_or(PreparedHeterogeneousEffectWorkError::InputSizeOverflow)?;
            peak_owned_frames = peak_owned_frames.max(live_frames);
        } else {
            live_frames = live_frames.checked_sub(dispatch.inputs.len().saturating_sub(1)).ok_or(
                PreparedHeterogeneousEffectWorkError::InvalidCpuPrefix { node: dispatch.node },
            )?;
        }
        if !live_materializations.insert(dispatch.output) {
            return Err(PreparedHeterogeneousEffectWorkError::InvalidCpuPrefix {
                node: dispatch.node,
            });
        }
    }
    let output_set = output_materializations.iter().copied().collect::<HashSet<_>>();
    if output_set.len() != output_materializations.len()
        || live_frames != output_materializations.len()
        || live_materializations != output_set
        || remaining.iter().any(|(materialization, remaining)| {
            *remaining != usize::from(output_materializations.contains(materialization))
        })
    {
        return Err(
            PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                reason: "cpu_dag_liveness_does_not_end_at_transfer_value",
            },
        );
    }
    frame_bytes
        .checked_mul(peak_owned_frames)
        .ok_or(PreparedHeterogeneousEffectWorkError::InputSizeOverflow)
}

fn prepare_gpu_suffix(
    compiled: &CompiledEffectGraph,
    plan: &CompiledEffectValueExecutionPlan,
    input_materializations: &[EffectMaterializationId],
    gpu_nodes: &[EffectGraphNodeId],
) -> Result<PreparedHeterogeneousGpuSuffix, PreparedHeterogeneousEffectWorkError> {
    let gpu_dispatches = plan
        .steps()
        .iter()
        .filter_map(|step| match step {
            EffectGraphExecutionStep::Dispatch {
                node,
                backend: EffectProcessingBackend::Gpu,
                precision: EffectWorkingPrecision::Float32,
                inputs,
                output,
                waits,
                signal,
                ..
            } => Some((
                *node,
                Arc::clone(inputs),
                *output,
                Arc::clone(waits),
                *signal,
            )),
            _ => None,
        })
        .collect::<Vec<_>>();
    if gpu_dispatches.len() != gpu_nodes.len()
        || gpu_dispatches.iter().map(|dispatch| dispatch.0).ne(gpu_nodes.iter().copied())
    {
        return Err(
            PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                reason: "gpu_dispatches_do_not_match_compiled_schedule",
            },
        );
    }

    let mut produced = input_materializations.iter().copied().collect::<HashSet<_>>();
    if produced.len() != input_materializations.len() {
        return Err(
            PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                reason: "gpu_suffix_input_materialization_reused",
            },
        );
    }
    for (node_id, inputs, output, waits, signal) in &gpu_dispatches {
        let node = compiled.graph().node(*node_id).ok_or(
            PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                reason: "gpu_partition_node_missing",
            },
        )?;
        if !compiled.node_execution_modes(*node_id).is_some_and(|modes| {
            modes.contains(
                EffectProcessingBackend::Gpu,
                EffectWorkingPrecision::Float32,
            )
        }) {
            return Err(
                PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                    reason: "gpu_partition_node_is_not_admitted_for_gpu_f32",
                },
            );
        }
        let semantic_inputs = node.input_ids();
        if semantic_inputs.len() != inputs.len() || waits.len() != inputs.len() {
            return Err(
                PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                    reason: "gpu_dispatch_input_arity_changed",
                },
            );
        }
        for ((semantic_input, materialization_id), wait) in
            semantic_inputs.iter().zip(inputs.iter()).zip(waits.iter())
        {
            let materialization = plan.materialization(*materialization_id).ok_or(
                PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                    reason: "gpu_dispatch_input_materialization_missing",
                },
            )?;
            if materialization.value() != *semantic_input
                || materialization.completion() != *wait
                || !produced.contains(materialization_id)
            {
                return Err(
                    PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                        reason: "gpu_dispatch_input_does_not_match_the_compiled_graph",
                    },
                );
            }
        }
        let output_materialization = plan.materialization(*output).ok_or(
            PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                reason: "gpu_dispatch_output_materialization_missing",
            },
        )?;
        if output_materialization.value() != *node_id
            || output_materialization.completion() != *signal
            || !produced.insert(*output)
        {
            return Err(
                PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                    reason: "gpu_dispatch_output_does_not_match_the_compiled_graph",
                },
            );
        }
    }
    if !produced.contains(&plan.output_materialization()) {
        return Err(
            PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                reason: "gpu_suffix_does_not_produce_graph_output",
            },
        );
    }

    let dispatches = match lower_effect_graph_nodes_to_gpu_plan(compiled, gpu_nodes) {
        Ok(fused) => {
            let first = gpu_dispatches.first().ok_or(
                PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                    reason: "gpu_suffix_has_no_dispatch",
                },
            )?;
            let last = gpu_dispatches.last().ok_or(
                PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                    reason: "gpu_suffix_has_no_dispatch",
                },
            )?;
            if input_materializations.len() != 1 || first.1.as_ref() != input_materializations {
                return Err(
                    PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                        reason: "fused_gpu_suffix_does_not_consume_transfer",
                    },
                );
            }
            vec![PreparedHeterogeneousGpuDispatch::PointChain {
                nodes: Arc::from(gpu_nodes),
                input: input_materializations[0],
                output: last.2,
                waits: Arc::clone(&first.3),
                signal: last.4,
                plan: Arc::new(fused),
            }]
        }
        Err(
            EffectGpuPlanBlocker::UnsupportedTopology { .. }
            | EffectGpuPlanBlocker::DisconnectedChain { .. }
            | EffectGpuPlanBlocker::TooManyOperations { .. },
        ) => gpu_dispatches
            .iter()
            .map(|(node_id, inputs, output, waits, signal)| {
                let node = compiled.graph().node(*node_id).ok_or(
                    PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                        reason: "gpu_partition_node_missing",
                    },
                )?;
                match &node.kind {
                    EffectGraphNodeKind::UnaryEffect { .. }
                    | EffectGraphNodeKind::DomainEffect { .. } => {
                        if inputs.len() != 1 {
                            return Err(
                                PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                                    reason: "gpu_point_dispatch_input_arity",
                                },
                            );
                        }
                        Ok(PreparedHeterogeneousGpuDispatch::PointChain {
                            nodes: Arc::from([*node_id]),
                            input: inputs[0],
                            output: *output,
                            waits: Arc::clone(waits),
                            signal: *signal,
                            plan: Arc::new(lower_effect_graph_node_to_gpu_point_plan(
                                compiled, *node_id,
                            )?),
                        })
                    }
                    EffectGraphNodeKind::Blend { blend_mode, opacity, .. } => {
                        if inputs.len() != 2 {
                            return Err(
                                PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                                    reason: "gpu_blend_dispatch_input_arity",
                                },
                            );
                        }
                        Ok(PreparedHeterogeneousGpuDispatch::Blend {
                            node: *node_id,
                            base: inputs[0],
                            overlay: inputs[1],
                            output: *output,
                            waits: Arc::clone(waits),
                            signal: *signal,
                            opacity: *opacity,
                            blend_mode: *blend_mode,
                        })
                    }
                    EffectGraphNodeKind::Mask { invert, mask_op, .. } => {
                        if inputs.len() != 2 {
                            return Err(
                                PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                                    reason: "gpu_mask_dispatch_input_arity",
                                },
                            );
                        }
                        let input_format = plan
                            .materialization(inputs[0])
                            .map(EffectValueMaterialization::residency)
                            .map(EffectValueResidency::format);
                        let mask_format = plan
                            .materialization(inputs[1])
                            .map(EffectValueMaterialization::residency)
                            .map(EffectValueResidency::format);
                        let output_format = plan
                            .materialization(*output)
                            .map(EffectValueMaterialization::residency)
                            .map(EffectValueResidency::format);
                        let scene_linear = EffectValueFormat::new(
                            EffectWorkingPrecision::Float32,
                            EffectColorDomain::SceneLinearRgb,
                        );
                        let alpha_mask = EffectValueFormat::new(
                            EffectWorkingPrecision::Float32,
                            EffectColorDomain::AlphaMask,
                        );
                        if input_format != Some(scene_linear)
                            || mask_format != Some(alpha_mask)
                            || output_format != Some(scene_linear)
                        {
                            return Err(
                                PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                                    reason: "gpu_mask_dispatch_domain_mismatch",
                                },
                            );
                        }
                        Ok(PreparedHeterogeneousGpuDispatch::Mask {
                            node: *node_id,
                            input: inputs[0],
                            mask: inputs[1],
                            output: *output,
                            waits: Arc::clone(waits),
                            signal: *signal,
                            invert: *invert,
                            mask_op: *mask_op,
                        })
                    }
                    EffectGraphNodeKind::MultiInput { blend_mode, opacity, .. } => {
                        match inputs.len() {
                            0 => Err(
                                PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                                    reason: "gpu_multi_input_dispatch_requires_an_input",
                                },
                            ),
                            1 => Ok(PreparedHeterogeneousGpuDispatch::Copy {
                                node: *node_id,
                                input: inputs[0],
                                output: *output,
                                waits: Arc::clone(waits),
                                signal: *signal,
                            }),
                            _ => Ok(PreparedHeterogeneousGpuDispatch::MultiInput {
                                node: *node_id,
                                inputs: Arc::clone(inputs),
                                output: *output,
                                waits: Arc::clone(waits),
                                signal: *signal,
                                opacity: *opacity,
                                blend_mode: *blend_mode,
                            }),
                        }
                    }
                    _ => Err(
                        PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                            reason: "gpu_dag_contains_unimplemented_node",
                        },
                    ),
                }
            })
            .collect::<Result<Vec<_>, _>>()?,
        Err(error) => return Err(error.into()),
    };

    let mut use_counts = HashMap::<EffectMaterializationId, usize>::new();
    for dispatch in &dispatches {
        dispatch.try_for_each_input(|input| {
            let count = use_counts.entry(input).or_default();
            *count = count
                .checked_add(1)
                .ok_or(PreparedHeterogeneousEffectWorkError::InputSizeOverflow)?;
            Ok::<(), PreparedHeterogeneousEffectWorkError>(())
        })?;
    }
    let mut steps = Vec::new();
    for dispatch in dispatches {
        let signal = dispatch.signal();
        let mut releases = Vec::new();
        dispatch.try_for_each_input(|input| {
            let remaining = use_counts.get_mut(&input).ok_or(
                PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                    reason: "gpu_suffix_input_use_count_missing",
                },
            )?;
            *remaining = remaining.checked_sub(1).ok_or(
                PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                    reason: "gpu_suffix_input_use_count_underflow",
                },
            )?;
            if *remaining == 0 && input != plan.output_materialization() {
                releases.push(input);
            }
            Ok::<(), PreparedHeterogeneousEffectWorkError>(())
        })?;
        steps.push(PreparedHeterogeneousGpuStep::Dispatch(dispatch));
        steps.extend(releases.into_iter().map(|materialization| {
            PreparedHeterogeneousGpuStep::Release { materialization, after: signal }
        }));
    }
    if use_counts.values().any(|remaining| *remaining != 0) {
        return Err(
            PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                reason: "gpu_suffix_use_count_did_not_close",
            },
        );
    }
    let mut node_ids = Vec::new();
    for step in &steps {
        if let PreparedHeterogeneousGpuStep::Dispatch(dispatch) = step {
            dispatch.append_nodes(&mut node_ids);
        }
    }
    if node_ids != gpu_nodes {
        return Err(
            PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                reason: "prepared_gpu_suffix_changed_node_order",
            },
        );
    }
    Ok(PreparedHeterogeneousGpuSuffix {
        input_materializations: Arc::from(input_materializations),
        output_materialization: plan.output_materialization(),
        steps: steps.into(),
        node_ids: node_ids.into(),
    })
}

fn validate_cpu_dag_gpu_tail_partition(
    compiled: &CompiledEffectGraph,
    plan: &CompiledEffectValueExecutionPlan,
    cpu_dispatches: &[PreparedCpuGraphDispatch],
    cpu_output_materializations: &[EffectMaterializationId],
    gpu_nodes: &[EffectGraphNodeId],
) -> Result<(), PreparedHeterogeneousEffectWorkError> {
    let source = compiled
        .schedule()
        .ordered_nodes
        .iter()
        .copied()
        .find(|node_id| {
            matches!(
                compiled.graph().node(*node_id).map(|node| &node.kind),
                Some(EffectGraphNodeKind::Source)
            )
        })
        .ok_or(
            PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                reason: "compiled_source_missing",
            },
        )?;
    if plan.input_value() != source {
        return Err(
            PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                reason: "cpu_input_materialization_is_not_the_compiled_source",
            },
        );
    }
    let scheduled = compiled
        .schedule()
        .ordered_nodes
        .iter()
        .copied()
        .filter(|node_id| *node_id != source)
        .collect::<Vec<_>>();
    let partition = cpu_dispatches
        .iter()
        .map(|dispatch| dispatch.node)
        .chain(gpu_nodes.iter().copied())
        .collect::<Vec<_>>();
    if partition != scheduled {
        return Err(
            PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                reason: "cpu_gpu_partition_does_not_cover_the_compiled_schedule",
            },
        );
    }

    let mut produced = HashSet::from([plan.input_materialization()]);
    for dispatch in cpu_dispatches {
        let node = compiled.graph().node(dispatch.node).ok_or(
            PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                reason: "cpu_partition_node_missing",
            },
        )?;
        if !compiled.node_execution_modes(dispatch.node).is_some_and(|modes| {
            modes.contains(
                EffectProcessingBackend::Cpu,
                EffectWorkingPrecision::Float32,
            )
        }) {
            return Err(
                PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                    reason: "cpu_partition_node_is_not_admitted_for_cpu_f32",
                },
            );
        }
        cpu_node_scratch_frames(node)?;
        let semantic_inputs = node.input_ids();
        if semantic_inputs.len() != dispatch.inputs.len() {
            return Err(
                PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                    reason: "cpu_dispatch_input_arity_changed",
                },
            );
        }
        for (semantic_input, materialization_id) in
            semantic_inputs.iter().zip(dispatch.inputs.iter())
        {
            let materialization = plan.materialization(*materialization_id).ok_or(
                PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                    reason: "cpu_dispatch_input_materialization_missing",
                },
            )?;
            if materialization.value() != *semantic_input || !produced.contains(materialization_id)
            {
                return Err(
                    PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                        reason: "cpu_dispatch_input_does_not_match_the_compiled_graph",
                    },
                );
            }
        }
        let output = plan.materialization(dispatch.output).ok_or(
            PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                reason: "cpu_dispatch_output_materialization_missing",
            },
        )?;
        if output.value() != dispatch.node || !produced.insert(dispatch.output) {
            return Err(
                PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                    reason: "cpu_dispatch_output_does_not_match_the_compiled_graph",
                },
            );
        }
    }
    for output_materialization in cpu_output_materializations {
        if plan.materialization(*output_materialization).is_none()
            || !produced.contains(output_materialization)
        {
            return Err(
                PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                    reason: "cpu_transfer_materialization_missing",
                },
            );
        }
    }
    Ok(())
}

fn cpu_materialization_use_counts(
    dispatches: &[PreparedCpuGraphDispatch],
    output_materializations: &[EffectMaterializationId],
) -> Result<HashMap<EffectMaterializationId, usize>, PreparedHeterogeneousEffectWorkError> {
    let mut use_counts = HashMap::new();
    for dispatch in dispatches {
        for input in dispatch.inputs.iter().copied() {
            let count = use_counts.entry(input).or_insert(0_usize);
            *count = count
                .checked_add(1)
                .ok_or(PreparedHeterogeneousEffectWorkError::InputSizeOverflow)?;
        }
    }
    for output in output_materializations {
        let count = use_counts.entry(*output).or_insert(0_usize);
        *count = count
            .checked_add(1)
            .ok_or(PreparedHeterogeneousEffectWorkError::InputSizeOverflow)?;
    }
    Ok(use_counts)
}

fn cpu_node_scratch_frames(
    node: &crate::EffectGraphNode,
) -> Result<usize, PreparedHeterogeneousEffectWorkError> {
    match &node.kind {
        EffectGraphNodeKind::UnaryEffect { op, .. }
        | EffectGraphNodeKind::DomainEffect { op, .. } => match op {
            crate::EffectRenderOp::GaussianBlur { .. }
            | crate::EffectRenderOp::Sharpen { .. }
            | crate::EffectRenderOp::ChromaticAberration { .. }
            | crate::EffectRenderOp::ColorAdjust { .. }
            | crate::EffectRenderOp::Vignette { .. }
            | crate::EffectRenderOp::Grain { .. }
            | crate::EffectRenderOp::Lut3D { .. } => Ok(render_op_f32_scratch_frames(op)),
            crate::EffectRenderOp::TemporalFrameBlend { .. }
            | crate::EffectRenderOp::Custom { .. } => {
                Err(PreparedHeterogeneousEffectWorkError::UnsupportedCpuOperation { node: node.id })
            }
        },
        EffectGraphNodeKind::Blend { .. }
        | EffectGraphNodeKind::Mask { .. }
        | EffectGraphNodeKind::MaskSource { .. } => Ok(0),
        EffectGraphNodeKind::MultiInput { inputs, .. } if !inputs.is_empty() => Ok(0),
        EffectGraphNodeKind::Source | EffectGraphNodeKind::MultiInput { .. } => {
            Err(PreparedHeterogeneousEffectWorkError::UnsupportedCpuOperation { node: node.id })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        adjustment::apply_render_op_f32,
        compile_reference_render_graph,
        mask::{MaskOp, MaskShape},
        register_effect_definition, EffectColorDomainContract, EffectDefinition,
        EffectExecutionContract, EffectExecutionSessionConfig, EffectExecutionTransfer,
        EffectGraphBuilder, EffectGraphBuilderState, EffectGraphPreparer, EffectNodeExt,
        EffectRenderOp, PreparedEffectEvaluator, PreparedEffectProgram,
    };
    use mondrian_core::{
        automation::PropertyValue,
        effect_data::{EffectNode, EffectType},
        types::BlendMode,
        TimelineTime, WorkingColorSpace,
    };

    const CPU_LANE: EffectExecutionLaneId = EffectExecutionLaneId::new(1);
    const GPU_LANE: EffectExecutionLaneId = EffectExecutionLaneId::new(2);

    fn test_environment() -> EffectExecutionEnvironment {
        EffectExecutionEnvironment::new(
            Arc::from([
                EffectExecutionLane::new(
                    CPU_LANE,
                    EffectProcessingBackend::Cpu,
                    crate::EffectWorkingPrecisions::FLOAT32,
                    10,
                ),
                EffectExecutionLane::new(
                    GPU_LANE,
                    EffectProcessingBackend::Gpu,
                    crate::EffectWorkingPrecisions::FLOAT32,
                    1,
                ),
            ]),
            Arc::from([
                EffectExecutionTransfer::new(
                    CPU_LANE,
                    EffectWorkingPrecision::Float32,
                    GPU_LANE,
                    EffectWorkingPrecision::Float32,
                    2,
                ),
                EffectExecutionTransfer::new(
                    GPU_LANE,
                    EffectWorkingPrecision::Float32,
                    CPU_LANE,
                    EffectWorkingPrecision::Float32,
                    3,
                ),
            ]),
        )
        .expect("test environment")
    }

    fn generous_budget() -> EffectGraphExecutionBudget {
        EffectGraphExecutionBudget::new(
            512 * 1024 * 1024,
            512 * 1024 * 1024,
            1024 * 1024 * 1024,
            128,
            256,
        )
    }

    fn request(extent: EffectFrameExtent) -> EffectGraphExecutionRequest {
        let format = EffectValueFormat::new(
            EffectWorkingPrecision::Float32,
            EffectColorDomain::SceneLinearRgb,
        );
        EffectGraphExecutionRequest::new(
            extent,
            EffectValueResidency::new(CPU_LANE, format),
            EffectValueResidency::new(GPU_LANE, format),
            generous_budget(),
        )
    }

    fn tracer_graph() -> Arc<CompiledEffectGraph> {
        let mut correction = EffectNode::with_defaults(EffectType::BasicCorrection);
        correction
            .set_static_value_by_parameter(
                &EffectType::BasicCorrection
                    .parameter_id("exposure")
                    .expect("built-in exposure parameter"),
                PropertyValue::Float(0.25),
            )
            .expect("enable Basic Correction tracer operation");
        let mut grain = EffectNode::with_defaults(EffectType::Grain);
        grain
            .set_static_value_by_parameter(
                &EffectType::Grain
                    .parameter_id("amount")
                    .expect("built-in grain amount parameter"),
                PropertyValue::Float(0.2),
            )
            .expect("enable Grain tracer operation");
        let effects = [
            EffectNode::with_defaults(EffectType::GaussianBlur),
            correction,
            grain,
        ];
        PreparedEffectProgram::prepare(&effects, &[], WorkingColorSpace::LinearRec2020)
            .expect("prepared tracer effects")
            .evaluate(TimelineTime::ZERO)
            .expect("compiled tracer graph")
    }

    fn cpu_dag_to_gpu_tail_graph() -> Arc<CompiledEffectGraph> {
        static NEXT_DEFINITION_ID: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(1);
        let definition_id = NEXT_DEFINITION_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let cpu_type =
            EffectType::Plugin(format!("test.heterogeneous.cpu-dag.{definition_id}.cpu"));
        let gpu_type =
            EffectType::Plugin(format!("test.heterogeneous.cpu-dag.{definition_id}.gpu"));
        register_effect_definition(
            EffectDefinition::new(
                cpu_type.key(),
                "CPU DAG stage",
                Default::default(),
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(EffectExecutionContract {
                execution_modes: EffectExecutionModes::CPU_F32,
                ..EffectExecutionContract::IDENTITY
            })
            .with_branching_graph_builder(Arc::new(|_, _, graph| {
                let source = graph.current_output();
                let left = graph.add_unary_from(
                    source,
                    EffectRenderOp::ColorAdjust {
                        exposure: 0.5,
                        contrast: 1.0,
                        saturation: 1.0,
                        working_color_space: WorkingColorSpace::LinearRec2020,
                    },
                );
                let right = graph.add_unary_from(
                    source,
                    EffectRenderOp::Vignette { intensity: 0.2, feather: 0.75 },
                );
                let output = graph.add_blend(left, right, BlendMode::Screen, 0.35);
                graph.set_current_output(output);
                Ok(())
            })),
        )
        .expect("register CPU DAG definition");
        register_effect_definition(
            EffectDefinition::new(
                gpu_type.key(),
                "GPU point tail",
                Default::default(),
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(EffectExecutionContract {
                execution_modes: EffectExecutionModes::GPU_F32,
                determinism: crate::EffectDeterminism::FrameSeeded,
                ..EffectExecutionContract::IDENTITY
            })
            .with_graph_builder(Arc::new(|_, _, graph| {
                graph.append_unary(EffectRenderOp::Grain { amount: 0.1 });
                Ok(())
            })),
        )
        .expect("register GPU-tail definition");

        PreparedEffectProgram::prepare(
            &[EffectNode::new(cpu_type), EffectNode::new(gpu_type)],
            &[],
            WorkingColorSpace::LinearRec2020,
        )
        .expect("prepare CPU-DAG/GPU-tail program")
        .evaluate(TimelineTime::ZERO)
        .expect("compile CPU-DAG/GPU-tail graph")
    }

    fn mask_source_to_gpu_tail_graph() -> Arc<CompiledEffectGraph> {
        static NEXT_DEFINITION_ID: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(1);
        let definition_id = NEXT_DEFINITION_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mask_type = EffectType::Plugin(format!(
            "test.heterogeneous.mask-source.{definition_id}.cpu"
        ));
        let gpu_type = EffectType::Plugin(format!(
            "test.heterogeneous.mask-source.{definition_id}.gpu"
        ));
        register_effect_definition(
            EffectDefinition::new(
                mask_type.key(),
                "CPU Mask stage",
                Default::default(),
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(EffectExecutionContract {
                execution_modes: EffectExecutionModes::CPU_F32,
                ..EffectExecutionContract::IDENTITY
            })
            .with_branching_graph_builder(Arc::new(|_, _, graph| {
                let input = graph.current_output();
                let mask = graph.add_mask_source(
                    MaskShape::Rectangle {
                        x: 0.0,
                        y: 0.0,
                        width: 1.0,
                        height: 1.0,
                        corner_radius: 0.0,
                    },
                    0.0,
                    0.0,
                    0.5,
                );
                let output = graph.add_mask(input, mask, false, MaskOp::Add);
                graph.set_current_output(output);
                Ok(())
            })),
        )
        .expect("register CPU Mask definition");
        register_effect_definition(
            EffectDefinition::new(
                gpu_type.key(),
                "GPU point tail",
                Default::default(),
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(EffectExecutionContract {
                execution_modes: EffectExecutionModes::GPU_F32,
                determinism: crate::EffectDeterminism::FrameSeeded,
                ..EffectExecutionContract::IDENTITY
            })
            .with_graph_builder(Arc::new(|_, _, graph| {
                graph.append_unary(EffectRenderOp::Grain { amount: 0.0 });
                Ok(())
            })),
        )
        .expect("register GPU-tail definition");

        PreparedEffectProgram::prepare(
            &[EffectNode::new(mask_type), EffectNode::new(gpu_type)],
            &[],
            WorkingColorSpace::LinearRec2020,
        )
        .expect("prepare CPU-Mask/GPU-tail program")
        .evaluate(TimelineTime::ZERO)
        .expect("compile CPU-Mask/GPU-tail graph")
    }

    fn cpu_mask_source_to_gpu_mask_graph(
        invert: bool,
        mask_op: MaskOp,
    ) -> Arc<CompiledEffectGraph> {
        let mut graph = EffectGraphBuilderState::new();
        let source = graph.source();
        let filtered = graph.add_unary_from(source, EffectRenderOp::GaussianBlur { radius: 1.0 });
        let mask = graph.add_mask_source(
            MaskShape::Rectangle {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
                corner_radius: 0.0,
            },
            0.0,
            0.0,
            0.35,
        );
        let output = graph.add_mask(filtered, mask, invert, mask_op);
        graph.set_current_output(output);
        compile_reference_render_graph(graph.finish()).expect("compile GPU Mask graph")
    }

    fn lifetime_partition_graph(
        resource_lifetime: EffectResourceLifetime,
    ) -> Arc<CompiledEffectGraph> {
        static NEXT_DEFINITION_ID: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(1);
        let definition_id = NEXT_DEFINITION_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let cpu_type = EffectType::Plugin(format!(
            "test.heterogeneous.resource-lifetime.{definition_id}.cpu"
        ));
        let gpu_type = EffectType::Plugin(format!(
            "test.heterogeneous.resource-lifetime.{definition_id}.gpu"
        ));
        let cpu_definition = EffectDefinition::new(
            cpu_type.key(),
            "CPU lifetime stage",
            Default::default(),
            EffectColorDomainContract::SCENE_LINEAR,
        )
        .with_execution_contract(EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            resource_lifetime,
            ..EffectExecutionContract::IDENTITY
        });
        let cpu_definition = if resource_lifetime == EffectResourceLifetime::PreparedProgram {
            let preparer: EffectGraphPreparer = Arc::new(|_, _, _| {
                let evaluator: EffectGraphBuilder = Arc::new(|_, _, graph| {
                    graph.append_unary(EffectRenderOp::Vignette { intensity: 0.25, feather: 0.5 });
                    Ok(())
                });
                Ok(PreparedEffectEvaluator::new(evaluator))
            });
            cpu_definition.with_prepared_graph_builder(preparer)
        } else {
            cpu_definition.with_graph_builder(Arc::new(|_, _, graph| {
                graph.append_unary(EffectRenderOp::Vignette { intensity: 0.25, feather: 0.5 });
                Ok(())
            }))
        };
        register_effect_definition(cpu_definition).expect("register CPU lifetime definition");
        register_effect_definition(
            EffectDefinition::new(
                gpu_type.key(),
                "GPU lifetime stage",
                Default::default(),
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(EffectExecutionContract {
                execution_modes: EffectExecutionModes::GPU_F32,
                ..EffectExecutionContract::IDENTITY
            })
            .with_graph_builder(Arc::new(|_, _, graph| {
                graph.append_unary(EffectRenderOp::Vignette { intensity: 0.1, feather: 0.75 });
                Ok(())
            })),
        )
        .expect("register GPU lifetime definition");

        PreparedEffectProgram::prepare(
            &[EffectNode::new(cpu_type), EffectNode::new(gpu_type)],
            &[],
            WorkingColorSpace::LinearRec2020,
        )
        .expect("prepare lifetime partition")
        .evaluate(TimelineTime::ZERO)
        .expect("compile lifetime partition")
    }

    #[test]
    fn current_frame_heterogeneous_planner_rejects_unowned_continuity_resources() {
        let environment = test_environment();
        let extent = EffectFrameExtent::new(4, 4);

        for lifetime in [
            EffectResourceLifetime::Frame,
            EffectResourceLifetime::PreparedProgram,
        ] {
            let graph = lifetime_partition_graph(lifetime);
            let plan = plan_effect_graph_value_execution(&graph, &environment, request(extent))
                .expect("frame/prepared-program resources have an explicit owner");
            let semantic_steps = plan
                .steps()
                .iter()
                .filter(|step| !matches!(step, EffectGraphExecutionStep::Release { .. }))
                .collect::<Vec<_>>();
            assert!(matches!(
                semantic_steps.as_slice(),
                [
                    EffectGraphExecutionStep::Dispatch {
                        backend: EffectProcessingBackend::Cpu,
                        ..
                    },
                    EffectGraphExecutionStep::Transfer { .. },
                    EffectGraphExecutionStep::Dispatch {
                        backend: EffectProcessingBackend::Gpu,
                        ..
                    }
                ]
            ));
        }

        let graph = lifetime_partition_graph(EffectResourceLifetime::ContinuitySession);
        assert_eq!(
            plan_effect_graph_value_execution(&graph, &environment, request(extent)),
            Err(EffectGraphExecutionPlanError::ContinuityResourceUnsupported)
        );
    }

    #[test]
    fn plans_and_executes_real_cpu_blur_to_gpu_point_tail() {
        let compiled = tracer_graph();
        assert_eq!(compiled.stage_bindings().len(), 3);
        let environment = test_environment();
        let extent = EffectFrameExtent::new(3, 2);
        let work = PreparedHeterogeneousEffectWork::prepare(
            Arc::clone(&compiled),
            &environment,
            request(extent),
        )
        .expect("prepared heterogeneous work");

        assert_eq!(work.plan().frame_extent(), extent);
        assert_eq!(work.cpu_nodes().len(), 1);
        assert_eq!(work.gpu_suffix().node_ids().len(), 2);
        assert_eq!(
            work.plan()
                .materialization(work.gpu_suffix().input_materializations()[0])
                .expect("GPU input materialization")
                .value(),
            work.cpu_nodes()[0]
        );
        assert_eq!(work.cpu_required_working_bytes(), 192);
        assert_eq!(
            match &work.gpu_suffix().steps()[0] {
                PreparedHeterogeneousGpuStep::Dispatch(
                    PreparedHeterogeneousGpuDispatch::PointChain { plan, .. },
                ) => plan.operations().len(),
                _ => 0,
            },
            2,
            "BasicCorrection and Grain form the exact GPU tail"
        );
        let semantic_steps = work
            .plan()
            .steps()
            .iter()
            .filter(|step| !matches!(step, EffectGraphExecutionStep::Release { .. }))
            .cloned()
            .collect::<Vec<_>>();
        assert!(matches!(
            semantic_steps.as_slice(),
            [
                EffectGraphExecutionStep::Dispatch {
                    backend: EffectProcessingBackend::Cpu,
                    precision: EffectWorkingPrecision::Float32,
                    ..
                },
                EffectGraphExecutionStep::Transfer { .. },
                EffectGraphExecutionStep::Dispatch {
                    backend: EffectProcessingBackend::Gpu,
                    precision: EffectWorkingPrecision::Float32,
                    ..
                },
                EffectGraphExecutionStep::Dispatch {
                    backend: EffectProcessingBackend::Gpu,
                    precision: EffectWorkingPrecision::Float32,
                    ..
                }
            ]
        ));

        let input = vec![
            [0.0, 0.1, 0.2, 1.0],
            [0.2, 0.3, 0.4, 1.0],
            [0.4, 0.5, 0.6, 1.0],
            [0.6, 0.7, 0.8, 1.0],
            [0.8, 0.9, 1.0, 1.0],
            [1.0, 0.9, 0.8, 1.0],
        ];
        let blur_node = compiled.graph().node(work.cpu_nodes()[0]).expect("blur node");
        let blur_op = match &blur_node.kind {
            EffectGraphNodeKind::UnaryEffect { op, .. } => op,
            _ => panic!("expected unary blur"),
        };
        let mut reference = input.clone();
        assert!(apply_render_op_f32(
            &mut reference,
            extent.width(),
            extent.height(),
            blur_op,
            19,
        ));

        let mut session =
            EffectExecutionSession::new(EffectExecutionSessionConfig::uncached(8 * 1024 * 1024));
        session.bind_generation(7);
        let completion = work
            .execute_cpu_prefix_uncancelled(
                &session,
                7,
                &input,
                19,
                WorkingColorSpace::LinearRec2020,
            )
            .expect("CPU prefix");
        assert_eq!(completion.boundary_values()[0].pixels(), reference);
        assert_eq!(completion.execution_plan(), work.plan());
        let transfer = completion.evidence().transfers()[0];
        assert_ne!(
            transfer.completed_cpu_token(),
            transfer.pending_gpu_input_token()
        );
        assert_eq!(
            completion.evidence().graph_fingerprint(),
            compiled.semantic_fingerprint()
        );
        assert_eq!(
            completion.evidence().working_color_space(),
            WorkingColorSpace::LinearRec2020
        );

        let mut undersized_session =
            EffectExecutionSession::new(EffectExecutionSessionConfig::uncached(191));
        undersized_session.bind_generation(7);
        assert!(matches!(
            work.execute_cpu_prefix_uncancelled(
                &undersized_session,
                7,
                &input,
                19,
                WorkingColorSpace::LinearRec2020,
            ),
            Err(
                PreparedHeterogeneousEffectWorkError::WorkingBudgetExceeded {
                    required: 192,
                    limit: 191
                }
            )
        ));
    }

    #[test]
    fn working_budget_keeps_sharpen_original_blur_and_scratch_alive() {
        let mut sharpen = EffectNode::with_defaults(EffectType::Sharpen);
        sharpen
            .set_static_value_by_parameter(
                &EffectType::Sharpen
                    .parameter_id("amount")
                    .expect("built-in sharpen amount parameter"),
                PropertyValue::Float(0.75),
            )
            .expect("enable Sharpen tracer operation");
        let mut grain = EffectNode::with_defaults(EffectType::Grain);
        grain
            .set_static_value_by_parameter(
                &EffectType::Grain
                    .parameter_id("amount")
                    .expect("built-in grain amount parameter"),
                PropertyValue::Float(0.2),
            )
            .expect("enable Grain tracer operation");
        let effects = [sharpen, grain];
        let compiled =
            PreparedEffectProgram::prepare(&effects, &[], WorkingColorSpace::LinearRec2020)
                .expect("prepared sharpen tracer effects")
                .evaluate(TimelineTime::ZERO)
                .expect("compiled sharpen tracer graph");
        let extent = EffectFrameExtent::new(3, 2);
        let work = PreparedHeterogeneousEffectWork::prepare(
            compiled,
            &test_environment(),
            request(extent),
        )
        .expect("prepared heterogeneous sharpen work");

        assert_eq!(work.cpu_nodes().len(), 1);
        assert_eq!(work.cpu_required_working_bytes(), 288);
    }

    #[test]
    fn cpu_prefix_stop_never_returns_partial_pixels_or_completion_token() {
        let extent = EffectFrameExtent::new(32, 16);
        let work = PreparedHeterogeneousEffectWork::prepare(
            tracer_graph(),
            &test_environment(),
            request(extent),
        )
        .expect("prepared heterogeneous work");
        let input = vec![
            [0.25, 0.5, 0.75, 1.0];
            usize::try_from(extent.width()).expect("width")
                * usize::try_from(extent.height()).expect("height")
        ];
        let mut session =
            EffectExecutionSession::new(EffectExecutionSessionConfig::uncached(8 * 1024 * 1024));
        session.bind_generation(41);

        let immediate = work
            .execute_cpu_prefix_with_checkpoint(
                &session,
                41,
                &input,
                3,
                WorkingColorSpace::LinearRec2020,
                || Some(HeterogeneousCpuExecutionStopReason::Canceled),
            )
            .expect_err("canceled work cannot publish completion");
        assert!(matches!(
            immediate,
            PreparedHeterogeneousEffectWorkError::ExecutionStopped {
                generation: 41,
                reason: HeterogeneousCpuExecutionStopReason::Canceled
            }
        ));

        let mut checkpoints = 0_u32;
        let expired = work
            .execute_cpu_prefix_with_checkpoint(
                &session,
                41,
                &input,
                3,
                WorkingColorSpace::LinearRec2020,
                || {
                    checkpoints = checkpoints.saturating_add(1);
                    (checkpoints >= 10)
                        .then_some(HeterogeneousCpuExecutionStopReason::DeadlineExpired)
                },
            )
            .expect_err("expired kernel cannot publish completion");
        assert!(matches!(
            expired,
            PreparedHeterogeneousEffectWorkError::ExecutionStopped {
                generation: 41,
                reason: HeterogeneousCpuExecutionStopReason::DeadlineExpired
            }
        ));
        assert!(
            checkpoints >= 10,
            "the fixed kernel cadence must observe the injected stop"
        );
    }

    #[test]
    fn fanout_reuses_one_uploaded_materialization_and_releases_after_last_use() {
        let mut builder = EffectGraphBuilderState::new();
        let source = builder.source();
        let blur = builder.add_unary_from(source, EffectRenderOp::GaussianBlur { radius: 2.0 });
        let correction = builder.add_unary_from(
            blur,
            EffectRenderOp::ColorAdjust {
                exposure: 0.25,
                contrast: 1.1,
                saturation: 0.9,
                working_color_space: WorkingColorSpace::LinearRec2020,
            },
        );
        let grain = builder.add_unary_from(blur, EffectRenderOp::Grain { amount: 0.2 });
        let output = builder.add_blend(correction, grain, BlendMode::Normal, 0.5);
        builder.set_current_output(output);
        let compiled = compile_reference_render_graph(builder.finish()).expect("fanout graph");
        let environment = test_environment();
        let format = EffectValueFormat::new(
            EffectWorkingPrecision::Float32,
            EffectColorDomain::SceneLinearRgb,
        );
        let plan = plan_effect_graph_value_execution(
            &compiled,
            &environment,
            EffectGraphExecutionRequest::new(
                EffectFrameExtent::new(16, 9),
                EffectValueResidency::new(CPU_LANE, format),
                EffectValueResidency::new(CPU_LANE, format),
                generous_budget(),
            ),
        )
        .expect("fanout plan");
        let blur_uploads = plan
            .steps()
            .iter()
            .filter(|step| {
                matches!(
                    step,
                    EffectGraphExecutionStep::Transfer {
                        value,
                        from,
                        to,
                        ..
                    } if *value == blur
                        && from.lane() == CPU_LANE
                        && to.lane() == GPU_LANE
                )
            })
            .count();
        assert_eq!(blur_uploads, 1, "both GPU branches share one upload");

        let upload_output = plan
            .steps()
            .iter()
            .find_map(|step| match step {
                EffectGraphExecutionStep::Transfer { value, output, to, .. }
                    if *value == blur && to.lane() == GPU_LANE =>
                {
                    Some(*output)
                }
                _ => None,
            })
            .expect("uploaded blur materialization");
        let consuming_dispatches = plan
            .steps()
            .iter()
            .filter(|step| {
                matches!(
                    step,
                    EffectGraphExecutionStep::Dispatch { inputs, .. }
                        if inputs.contains(&upload_output)
                )
            })
            .count();
        assert_eq!(consuming_dispatches, 2);
        let (release_index, release_after) = plan
            .steps()
            .iter()
            .enumerate()
            .find_map(|(index, step)| match step {
                EffectGraphExecutionStep::Release { materialization, after }
                    if *materialization == upload_output =>
                {
                    Some((index, *after))
                }
                _ => None,
            })
            .expect("last-use release");
        let (final_consumer_index, final_consumer_signal) = plan
            .steps()
            .iter()
            .enumerate()
            .filter_map(|(index, step)| match step {
                EffectGraphExecutionStep::Dispatch { inputs, signal, .. }
                    if inputs.contains(&upload_output) =>
                {
                    Some((index, *signal))
                }
                _ => None,
            })
            .max_by_key(|(index, _)| *index)
            .expect("fanout consumers");
        assert_eq!(release_index, final_consumer_index + 1);
        assert_eq!(release_after, final_consumer_signal);
    }

    #[test]
    fn planning_is_deterministic_and_budget_failures_are_typed() {
        let compiled = tracer_graph();
        let environment = test_environment();
        let request = request(EffectFrameExtent::new(64, 36));
        let first =
            plan_effect_graph_value_execution(&compiled, &environment, request).expect("first");
        let second =
            plan_effect_graph_value_execution(&compiled, &environment, request).expect("second");
        assert_eq!(first, second);

        let tiny = EffectGraphExecutionRequest::new(
            request.frame_extent,
            request.input_residency,
            request.output_residency,
            EffectGraphExecutionBudget::new(1, 1, 1, 1, 1),
        );
        assert!(matches!(
            plan_effect_graph_value_execution(&compiled, &environment, tiny),
            Err(EffectGraphExecutionPlanError::BudgetExceeded { .. })
        ));
    }

    #[test]
    fn declared_multi_hop_transfer_path_preserves_value_and_token_order() {
        let compiled = compile_reference_render_graph(crate::EffectRenderGraph::identity())
            .expect("identity graph");
        let environment = EffectExecutionEnvironment::new(
            Arc::from([
                EffectExecutionLane::new(
                    CPU_LANE,
                    EffectProcessingBackend::Cpu,
                    crate::EffectWorkingPrecisions::NORMALIZED_U8
                        .union(crate::EffectWorkingPrecisions::FLOAT32),
                    1,
                ),
                EffectExecutionLane::new(
                    GPU_LANE,
                    EffectProcessingBackend::Gpu,
                    crate::EffectWorkingPrecisions::FLOAT32,
                    1,
                ),
            ]),
            Arc::from([
                EffectExecutionTransfer::new(
                    CPU_LANE,
                    EffectWorkingPrecision::NormalizedU8,
                    CPU_LANE,
                    EffectWorkingPrecision::Float32,
                    2,
                ),
                EffectExecutionTransfer::new(
                    CPU_LANE,
                    EffectWorkingPrecision::Float32,
                    GPU_LANE,
                    EffectWorkingPrecision::Float32,
                    3,
                ),
            ]),
        )
        .expect("multi-hop environment");
        let input_format = EffectValueFormat::new(
            EffectWorkingPrecision::NormalizedU8,
            EffectColorDomain::SceneLinearRgb,
        );
        let output_format = EffectValueFormat::new(
            EffectWorkingPrecision::Float32,
            EffectColorDomain::SceneLinearRgb,
        );
        let plan = plan_effect_graph_value_execution(
            &compiled,
            &environment,
            EffectGraphExecutionRequest::new(
                EffectFrameExtent::new(4, 4),
                EffectValueResidency::new(CPU_LANE, input_format),
                EffectValueResidency::new(GPU_LANE, output_format),
                generous_budget(),
            ),
        )
        .expect("multi-hop plan");
        let transfers = plan
            .steps()
            .iter()
            .filter_map(|step| match step {
                EffectGraphExecutionStep::Transfer { from, to, wait, signal, .. } => {
                    Some((*from, *to, *wait, *signal))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(transfers.len(), 2);
        assert_eq!(
            (
                transfers[0].0.format().precision(),
                transfers[0].1.format().precision(),
                transfers[1].0.format().precision(),
                transfers[1].1.format().precision(),
            ),
            (
                EffectWorkingPrecision::NormalizedU8,
                EffectWorkingPrecision::Float32,
                EffectWorkingPrecision::Float32,
                EffectWorkingPrecision::Float32,
            )
        );
        assert_eq!(transfers[0].3, transfers[1].2);
        assert_eq!(
            plan.materialization(plan.output_materialization())
                .expect("output materialization")
                .completion(),
            transfers[1].3
        );
    }

    #[test]
    fn device_materialization_peak_is_not_inferred_from_mixed_precision_bytes() {
        let compiled = compile_reference_render_graph(crate::EffectRenderGraph::identity())
            .expect("identity graph");
        let environment = EffectExecutionEnvironment::new(
            Arc::from([EffectExecutionLane::new(
                GPU_LANE,
                EffectProcessingBackend::Gpu,
                crate::EffectWorkingPrecisions::NORMALIZED_U8
                    .union(crate::EffectWorkingPrecisions::FLOAT16),
                1,
            )]),
            Arc::from([EffectExecutionTransfer::new(
                GPU_LANE,
                EffectWorkingPrecision::NormalizedU8,
                GPU_LANE,
                EffectWorkingPrecision::Float16,
                1,
            )]),
        )
        .expect("mixed-precision device environment");
        let extent = EffectFrameExtent::new(4, 4);
        let input = EffectValueFormat::new(
            EffectWorkingPrecision::NormalizedU8,
            EffectColorDomain::SceneLinearRgb,
        );
        let output = EffectValueFormat::new(
            EffectWorkingPrecision::Float16,
            EffectColorDomain::SceneLinearRgb,
        );
        let plan = plan_effect_graph_value_execution(
            &compiled,
            &environment,
            EffectGraphExecutionRequest::new(
                extent,
                EffectValueResidency::new(GPU_LANE, input),
                EffectValueResidency::new(GPU_LANE, output),
                generous_budget(),
            ),
        )
        .expect("mixed-precision device plan");

        assert_eq!(plan.peak_device_materializations(), 2);
        assert_eq!(plan.peak_device_bytes(), 4_u64 * 4 * (4 + 8));
        assert!(
            plan.peak_device_bytes() < 4_u64 * 4 * 16,
            "a byte-derived RGBA32F count would under-estimate two live textures"
        );
    }

    #[test]
    fn missing_transfer_and_frame_byte_overflow_fail_closed() {
        let compiled = tracer_graph();
        let no_transfer = EffectExecutionEnvironment::new(
            test_environment().lanes().to_vec(),
            Arc::<[EffectExecutionTransfer]>::from([]),
        )
        .expect("valid disconnected environment");
        assert!(matches!(
            plan_effect_graph_value_execution(
                &compiled,
                &no_transfer,
                request(EffectFrameExtent::new(8, 8)),
            ),
            Err(EffectGraphExecutionPlanError::NoValueRoute { .. })
                | Err(EffectGraphExecutionPlanError::NoOutputRoute { .. })
        ));
        assert!(matches!(
            plan_effect_graph_value_execution(
                &compiled,
                &test_environment(),
                request(EffectFrameExtent::new(u32::MAX, u32::MAX)),
            ),
            Err(EffectGraphExecutionPlanError::ArithmeticOverflow)
        ));
    }

    #[test]
    fn executes_cpu_f32_fanout_join_before_fused_gpu_tail() {
        let compiled = cpu_dag_to_gpu_tail_graph();
        let extent = EffectFrameExtent::new(2, 2);
        let work = PreparedHeterogeneousEffectWork::prepare(
            Arc::clone(&compiled),
            &test_environment(),
            request(extent),
        )
        .expect("prepare CPU DAG route");
        assert_eq!(work.cpu_nodes().len(), 3);
        assert_eq!(work.gpu_suffix().node_ids().len(), 1);
        assert!(matches!(
            compiled.graph().node(work.cpu_nodes()[2]).map(|node| &node.kind),
            Some(EffectGraphNodeKind::Blend { .. })
        ));
        assert_eq!(
            work.cpu_required_working_bytes(),
            2 * 2 * 2 * std::mem::size_of::<[f32; 4]>(),
            "the shared source is cloned once for fan-out and joins back to one transfer value"
        );

        let input = vec![
            [0.05, 0.15, 0.25, 1.0],
            [0.25, 0.35, 0.45, 1.0],
            [0.45, 0.55, 0.65, 1.0],
            [0.65, 0.75, 0.85, 1.0],
        ];
        let mut left = input.clone();
        let mut right = input.clone();
        let left_op = match &compiled.graph().node(work.cpu_nodes()[0]).expect("left CPU node").kind
        {
            EffectGraphNodeKind::UnaryEffect { op, .. } => op,
            _ => panic!("expected left unary node"),
        };
        let right_op =
            match &compiled.graph().node(work.cpu_nodes()[1]).expect("right CPU node").kind {
                EffectGraphNodeKind::UnaryEffect { op, .. } => op,
                _ => panic!("expected right unary node"),
            };
        assert!(apply_render_op_f32(
            &mut left,
            extent.width(),
            extent.height(),
            left_op,
            23,
        ));
        assert!(apply_render_op_f32(
            &mut right,
            extent.width(),
            extent.height(),
            right_op,
            23,
        ));
        let expected = left
            .iter()
            .zip(&right)
            .map(|(base, overlay)| {
                crate::adjustment::blend_rgba_f32_pixel(*base, *overlay, 0.35, BlendMode::Screen)
            })
            .collect::<Vec<_>>();

        let mut session =
            EffectExecutionSession::new(EffectExecutionSessionConfig::uncached(1024 * 1024));
        session.bind_generation(11);
        let completion = work
            .execute_cpu_prefix_uncancelled(
                &session,
                11,
                &input,
                23,
                WorkingColorSpace::LinearRec2020,
            )
            .expect("execute CPU DAG prefix");
        assert_eq!(completion.boundary_values()[0].pixels(), expected);
        assert_eq!(
            completion.evidence().completed_cpu_nodes(),
            work.cpu_nodes()
        );
        assert_eq!(
            completion
                .execution_plan()
                .materialization(completion.gpu_suffix().input_materializations()[0])
                .expect("GPU input materialization")
                .value(),
            work.cpu_nodes()[2],
            "the upload consumes the joined CPU graph value"
        );
    }

    #[test]
    fn executes_cancellable_mask_source_in_cpu_dag_before_gpu_tail() {
        let extent = EffectFrameExtent::new(4, 3);
        let work = PreparedHeterogeneousEffectWork::prepare(
            mask_source_to_gpu_tail_graph(),
            &test_environment(),
            request(extent),
        )
        .expect("prepare CPU Mask route");
        assert_eq!(work.cpu_nodes().len(), 2);
        assert_eq!(work.gpu_suffix().node_ids().len(), 1);
        assert!(
            work.cpu_required_working_bytes() > 2 * 4 * 3 * std::mem::size_of::<[f32; 4]>(),
            "the working contract includes prepared Mask geometry and row scratch"
        );

        let input = vec![[0.2, 0.4, 0.6, 0.8]; 12];
        let mut session =
            EffectExecutionSession::new(EffectExecutionSessionConfig::uncached(1024 * 1024));
        session.bind_generation(71);
        let completion = work
            .execute_cpu_prefix_uncancelled(
                &session,
                71,
                &input,
                5,
                WorkingColorSpace::LinearRec2020,
            )
            .expect("execute CPU Mask route");
        for pixel in completion.boundary_values()[0].pixels() {
            assert_eq!(pixel[0..3], input[0][0..3]);
            assert!((pixel[3] - 0.4).abs() <= f32::EPSILON);
        }

        let mut checkpoints = 0_u32;
        let stopped = work
            .execute_cpu_prefix_with_checkpoint(
                &session,
                71,
                &input,
                5,
                WorkingColorSpace::LinearRec2020,
                || {
                    checkpoints = checkpoints.saturating_add(1);
                    (checkpoints == 8)
                        .then_some(HeterogeneousCpuExecutionStopReason::DeadlineExpired)
                },
            )
            .expect_err("Mask preparation/raster cancellation cannot publish completion");
        assert!(matches!(
            stopped,
            PreparedHeterogeneousEffectWorkError::ExecutionStopped {
                generation: 71,
                reason: HeterogeneousCpuExecutionStopReason::DeadlineExpired,
            }
        ));
        assert_eq!(checkpoints, 8);
    }

    #[test]
    fn prepares_cpu_mask_source_as_alpha_frontier_for_gpu_mask() {
        let extent = EffectFrameExtent::new(4, 3);
        let work = PreparedHeterogeneousEffectWork::prepare(
            cpu_mask_source_to_gpu_mask_graph(true, MaskOp::Difference),
            &test_environment(),
            request(extent),
        )
        .expect("prepare CPU MaskSource to GPU Mask route");
        assert_eq!(work.cpu_nodes().len(), 2);
        assert_eq!(work.gpu_suffix().node_ids().len(), 1);
        assert!(matches!(
            work.gpu_suffix().steps().iter().find_map(|step| match step {
                PreparedHeterogeneousGpuStep::Dispatch(dispatch) => Some(dispatch),
                PreparedHeterogeneousGpuStep::Release { .. } => None,
            }),
            Some(PreparedHeterogeneousGpuDispatch::Mask {
                invert: true,
                mask_op: MaskOp::Difference,
                ..
            })
        ));

        let input = vec![[0.2, 0.4, 0.6, 0.8]; 12];
        let mut session =
            EffectExecutionSession::new(EffectExecutionSessionConfig::uncached(1024 * 1024));
        session.bind_generation(73);
        let completion = work
            .execute_cpu_prefix_uncancelled(
                &session,
                73,
                &input,
                5,
                WorkingColorSpace::LinearRec2020,
            )
            .expect("execute CPU MaskSource frontier");
        let formats = completion
            .evidence()
            .transfers()
            .iter()
            .map(|transfer| transfer.format())
            .collect::<HashSet<_>>();
        assert_eq!(
            formats,
            HashSet::from([
                EffectValueFormat::new(
                    EffectWorkingPrecision::Float32,
                    EffectColorDomain::SceneLinearRgb,
                ),
                EffectValueFormat::new(
                    EffectWorkingPrecision::Float32,
                    EffectColorDomain::AlphaMask,
                ),
            ])
        );
        assert_eq!(completion.boundary_values().len(), 2);
    }

    #[test]
    fn cpu_dag_fanout_copy_observes_bounded_stop_checkpoints() {
        let source = vec![[0.25, 0.5, 0.75, 1.0]; 8_193];
        let mut checkpoints = 0_u32;
        let result = copy_cpu_pixels_controlled(&source, &mut || {
            checkpoints = checkpoints.saturating_add(1);
            if checkpoints == 2 {
                Err(PreparedHeterogeneousEffectWorkError::ExecutionStopped {
                    generation: 31,
                    reason: HeterogeneousCpuExecutionStopReason::DeadlineExpired,
                })
            } else {
                Ok(())
            }
        });
        assert!(matches!(
            result,
            Err(PreparedHeterogeneousEffectWorkError::ExecutionStopped {
                generation: 31,
                reason: HeterogeneousCpuExecutionStopReason::DeadlineExpired,
            })
        ));
        assert_eq!(checkpoints, 2);
    }

    #[test]
    fn prepares_gpu_fanout_join_suffix_from_the_unique_value_plan() {
        let mut builder = EffectGraphBuilderState::new();
        let source = builder.source();
        let first = builder.add_unary_from(source, EffectRenderOp::GaussianBlur { radius: 1.0 });
        let left = builder.add_unary_from(
            first,
            EffectRenderOp::ColorAdjust {
                exposure: 0.0,
                contrast: 1.0,
                saturation: 1.0,
                working_color_space: WorkingColorSpace::LinearRec2020,
            },
        );
        let right = builder.add_unary_from(first, EffectRenderOp::Grain { amount: 0.1 });
        let output = builder.add_blend(left, right, BlendMode::Screen, 0.5);
        builder.set_current_output(output);
        let compiled = compile_reference_render_graph(builder.finish()).expect("DAG graph");
        let work = PreparedHeterogeneousEffectWork::prepare(
            Arc::clone(&compiled),
            &test_environment(),
            request(EffectFrameExtent::new(8, 8)),
        )
        .expect("GPU DAG suffix");
        assert_eq!(work.cpu_nodes(), [first]);
        assert_eq!(work.gpu_suffix().node_ids(), [left, right, output]);
        let dispatches = work
            .gpu_suffix()
            .steps()
            .iter()
            .filter_map(|step| match step {
                PreparedHeterogeneousGpuStep::Dispatch(dispatch) => Some(dispatch),
                PreparedHeterogeneousGpuStep::Release { .. } => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(dispatches.len(), 3);
        assert!(matches!(
            dispatches.as_slice(),
            [
                PreparedHeterogeneousGpuDispatch::PointChain { nodes: left_nodes, .. },
                PreparedHeterogeneousGpuDispatch::PointChain { nodes: right_nodes, .. },
                PreparedHeterogeneousGpuDispatch::Blend {
                    node: blend,
                    blend_mode: BlendMode::Screen,
                    ..
                },
            ] if left_nodes.as_ref() == [left]
                && right_nodes.as_ref() == [right]
                && *blend == output
        ));
        let releases = work
            .gpu_suffix()
            .steps()
            .iter()
            .filter(|step| matches!(step, PreparedHeterogeneousGpuStep::Release { .. }))
            .count();
        assert_eq!(
            releases, 3,
            "upload and both branch outputs retire exactly once"
        );
        assert_eq!(
            work.gpu_suffix().output_materialization(),
            work.plan().output_materialization()
        );
    }

    #[test]
    fn prepares_ordered_gpu_multi_input_join_without_parallel_semantic_ir() {
        let mut builder = EffectGraphBuilderState::new();
        let source = builder.source();
        let first = builder.add_unary_from(source, EffectRenderOp::GaussianBlur { radius: 1.0 });
        let left = builder.add_unary_from(
            first,
            EffectRenderOp::ColorAdjust {
                exposure: 0.2,
                contrast: 1.0,
                saturation: 1.0,
                working_color_space: WorkingColorSpace::LinearRec2020,
            },
        );
        let middle = builder.add_unary_from(first, EffectRenderOp::Grain { amount: 0.1 });
        let right = builder.add_unary_from(
            first,
            EffectRenderOp::ColorAdjust {
                exposure: -0.15,
                contrast: 1.1,
                saturation: 0.9,
                working_color_space: WorkingColorSpace::LinearRec2020,
            },
        );
        let output = builder.add_multi_input(vec![left, middle, right], BlendMode::SoftLight, 0.35);
        builder.set_current_output(output);
        let compiled = compile_reference_render_graph(builder.finish()).expect("MultiInput graph");
        let work = PreparedHeterogeneousEffectWork::prepare(
            Arc::clone(&compiled),
            &test_environment(),
            request(EffectFrameExtent::new(8, 8)),
        )
        .expect("GPU MultiInput suffix");

        assert_eq!(work.cpu_nodes(), [first]);
        assert_eq!(work.gpu_suffix().node_ids(), [left, middle, right, output]);
        let dispatches = work
            .gpu_suffix()
            .steps()
            .iter()
            .filter_map(|step| match step {
                PreparedHeterogeneousGpuStep::Dispatch(dispatch) => Some(dispatch),
                PreparedHeterogeneousGpuStep::Release { .. } => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(dispatches.len(), 4);
        assert!(matches!(
            dispatches.last(),
            Some(PreparedHeterogeneousGpuDispatch::MultiInput {
                node,
                inputs,
                waits,
                blend_mode: BlendMode::SoftLight,
                opacity,
                ..
            }) if *node == output
                && inputs.len() == 3
                && waits.len() == inputs.len()
                && opacity.to_bits() == 0.35_f32.to_bits()
        ));
        assert_eq!(
            work.gpu_suffix()
                .steps()
                .iter()
                .filter(|step| matches!(step, PreparedHeterogeneousGpuStep::Release { .. }))
                .count(),
            4,
            "upload and all three ordered inputs retire exactly once"
        );
    }

    #[test]
    fn prepares_two_cpu_frontier_values_for_one_gpu_join() {
        let mut builder = EffectGraphBuilderState::new();
        let source = builder.source();
        let blurred = builder.add_unary_from(source, EffectRenderOp::GaussianBlur { radius: 1.0 });
        let sharpened = builder.add_unary_from(source, EffectRenderOp::Sharpen { amount: 0.4 });
        let output = builder.add_blend(blurred, sharpened, BlendMode::Screen, 0.6);
        builder.set_current_output(output);
        let compiled = compile_reference_render_graph(builder.finish()).expect("frontier graph");
        let work = PreparedHeterogeneousEffectWork::prepare(
            Arc::clone(&compiled),
            &test_environment(),
            request(EffectFrameExtent::new(8, 8)),
        )
        .expect("two-value CPU frontier");

        assert_eq!(work.cpu_nodes(), [blurred, sharpened]);
        assert_eq!(work.gpu_suffix().node_ids(), [output]);
        assert_eq!(work.gpu_suffix().input_materializations().len(), 2);
        assert!(matches!(
            work.gpu_suffix().steps().first(),
            Some(PreparedHeterogeneousGpuStep::Dispatch(
                PreparedHeterogeneousGpuDispatch::Blend {
                    node,
                    base,
                    overlay,
                    ..
                }
            )) if *node == output
                && [*base, *overlay] == work.gpu_suffix().input_materializations()
        ));

        let input = vec![[0.2, 0.4, 0.6, 0.8]; 64];
        let mut session =
            EffectExecutionSession::new(EffectExecutionSessionConfig::uncached(8 * 1024 * 1024));
        session.bind_generation(83);
        let completion = work
            .execute_cpu_prefix_uncancelled(
                &session,
                83,
                &input,
                31,
                WorkingColorSpace::LinearRec2020,
            )
            .expect("execute two-value CPU frontier");
        assert_eq!(completion.boundary_values().len(), 2);
        assert_eq!(completion.evidence().transfers().len(), 2);
        for (boundary, expected_materialization) in completion
            .boundary_values()
            .iter()
            .zip(work.gpu_suffix().input_materializations())
        {
            assert_eq!(
                boundary.transfer().gpu_materialization(),
                *expected_materialization
            );
            assert_eq!(boundary.pixels().len(), 64);
        }
    }

    #[test]
    fn one_input_multi_input_lowers_to_exact_gpu_materialization_copy() {
        let mut builder = EffectGraphBuilderState::new();
        let source = builder.source();
        let blurred = builder.add_unary_from(source, EffectRenderOp::GaussianBlur { radius: 1.0 });
        let output = builder.add_multi_input(vec![blurred], BlendMode::Normal, 1.0);
        builder.set_current_output(output);
        let compiled = compile_reference_render_graph(builder.finish()).expect("one-input graph");
        let modes = compiled.node_execution_modes(output).expect("compiled node modes");
        assert!(modes.contains(
            EffectProcessingBackend::Cpu,
            EffectWorkingPrecision::Float32
        ));
        assert!(modes.contains(
            EffectProcessingBackend::Gpu,
            EffectWorkingPrecision::Float32
        ));
        let work = PreparedHeterogeneousEffectWork::prepare(
            Arc::clone(&compiled),
            &test_environment(),
            request(EffectFrameExtent::new(8, 8)),
        )
        .expect("GPU identity copy suffix");
        assert_eq!(work.cpu_nodes(), [blurred]);
        assert_eq!(work.gpu_suffix().node_ids(), [output]);
        assert!(matches!(
            work.gpu_suffix().steps().first(),
            Some(PreparedHeterogeneousGpuStep::Dispatch(
                PreparedHeterogeneousGpuDispatch::Copy { node, .. }
            )) if *node == output
        ));
    }

    #[test]
    fn oversized_linear_gpu_tail_splits_into_admitted_point_dispatches() {
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(EffectRenderOp::GaussianBlur { radius: 1.0 });
        for index in 0..=crate::MAX_FUSED_GPU_EFFECT_OPS {
            builder.append_unary(EffectRenderOp::ColorAdjust {
                exposure: index as f32 * 0.01,
                contrast: 1.0,
                saturation: 1.0,
                working_color_space: WorkingColorSpace::LinearRec2020,
            });
        }
        let compiled = compile_reference_render_graph(builder.finish()).expect("long GPU tail");
        let work = PreparedHeterogeneousEffectWork::prepare(
            compiled,
            &test_environment(),
            request(EffectFrameExtent::new(8, 8)),
        )
        .expect("split long GPU tail");
        assert_eq!(work.cpu_nodes().len(), 1);
        assert_eq!(
            work.gpu_suffix().node_ids().len(),
            crate::MAX_FUSED_GPU_EFFECT_OPS + 1
        );
        assert_eq!(
            work.gpu_suffix()
                .steps()
                .iter()
                .filter(|step| matches!(step, PreparedHeterogeneousGpuStep::Dispatch(_)))
                .count(),
            crate::MAX_FUSED_GPU_EFFECT_OPS + 1
        );
    }
}
