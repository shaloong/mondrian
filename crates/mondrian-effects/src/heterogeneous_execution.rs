//! Graph-value-aware heterogeneous Effect planning and executable CPU→GPU
//! handoff preparation.
//!
//! This Module derives every dispatch, transfer, completion dependency, and
//! release directly from one immutable [`crate::CompiledEffectGraph`]. It does
//! not introduce a second semantic graph. The current executable vertical
//! slice accepts a caller-supplied scene-linear Float32 CPU frame, executes an
//! exact plan-driven CPU DAG prefix, and prepares an exact fused GPU
//! point-operation suffix. Preview/Export scheduling remains outside this
//! Module.

use crate::{
    adjustment::{
        apply_render_op_f32_controlled, render_op_f32_scratch_frames, EffectRasterRegion,
    },
    execution::{apply_alpha_mask_f32_region_controlled, blend_rgba_f32_region_controlled},
    lower_effect_graph_nodes_to_gpu_plan, CompiledEffectGpuPlan, CompiledEffectGraph,
    EffectColorDomain, EffectExecutionEnvironment, EffectExecutionLane, EffectExecutionLaneId,
    EffectExecutionModes, EffectExecutionSession, EffectFrameExtent, EffectGpuPlanBlocker,
    EffectGraphNodeId, EffectGraphNodeKind, EffectProcessingBackend, EffectResourceLifetime,
    EffectStateModel, EffectTemporalInputExtent, EffectWorkingPrecision,
};
use mondrian_core::WorkingColorSpace;
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
    completed_cpu_token: EffectCompletionToken,
    required_transfer_wait: EffectCompletionToken,
    pending_gpu_input_token: EffectCompletionToken,
    pending_output_token: EffectCompletionToken,
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

    /// Completion token proved by the returned CPU pixels.
    pub const fn completed_cpu_token(&self) -> EffectCompletionToken {
        self.completed_cpu_token
    }

    /// Token the explicit CPU→GPU transfer must wait for.
    pub const fn required_transfer_wait(&self) -> EffectCompletionToken {
        self.required_transfer_wait
    }

    /// Token that only a completed upload may signal.
    pub const fn pending_gpu_input_token(&self) -> EffectCompletionToken {
        self.pending_gpu_input_token
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
    pixels: Vec<[f32; 4]>,
    execution_plan: Arc<CompiledEffectValueExecutionPlan>,
    gpu_plan: Arc<CompiledEffectGpuPlan>,
    evidence: HeterogeneousCpuCompletionEvidence,
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
    /// Completed scene-linear Float32 CPU pixels.
    pub fn pixels(&self) -> &[[f32; 4]] {
        &self.pixels
    }

    /// Complete immutable graph-value plan whose CPU prefix completed.
    ///
    /// The receiving Adapter can recover the exact transfer residency,
    /// materialization identities, waits, releases, and GPU dispatch tokens
    /// without rebuilding a route.
    pub fn execution_plan(&self) -> &CompiledEffectValueExecutionPlan {
        &self.execution_plan
    }

    /// Exact fused GPU suffix consuming the uploaded pixels.
    pub fn gpu_plan(&self) -> &CompiledEffectGpuPlan {
        &self.gpu_plan
    }

    /// Completed and still-pending token evidence.
    pub const fn evidence(&self) -> &HeterogeneousCpuCompletionEvidence {
        &self.evidence
    }

    /// Consume the handoff into Adapter-owned parts.
    pub fn into_parts(
        self,
    ) -> (
        Vec<[f32; 4]>,
        Arc<CompiledEffectValueExecutionPlan>,
        Arc<CompiledEffectGpuPlan>,
        HeterogeneousCpuCompletionEvidence,
    ) {
        (
            self.pixels,
            self.execution_plan,
            self.gpu_plan,
            self.evidence,
        )
    }
}

/// Why the executable CPU-DAG-prefix/GPU-tail route cannot be prepared or run.
#[derive(Debug, thiserror::Error)]
pub enum PreparedHeterogeneousEffectWorkError {
    /// Graph-value planning failed before any pixel execution.
    #[error(transparent)]
    Planning(#[from] EffectGraphExecutionPlanError),
    /// The selected graph route is valid but outside the executable vertical slice.
    #[error(
        "effect heterogeneous route is not the supported CPU-F32 DAG to GPU-F32 fused-tail shape: {reason}"
    )]
    UnsupportedRouteShape {
        /// Stable diagnostic label.
        reason: &'static str,
    },
    /// The exact GPU tail cannot be lowered.
    #[error(transparent)]
    GpuPlan(#[from] EffectGpuPlanBlocker),
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

/// Reusable exact CPU-DAG-prefix/GPU-tail work prepared from one compiled graph.
#[derive(Debug)]
pub struct PreparedHeterogeneousEffectWork {
    compiled: Arc<CompiledEffectGraph>,
    plan: Arc<CompiledEffectValueExecutionPlan>,
    cpu_nodes: Arc<[EffectGraphNodeId]>,
    cpu_dispatches: Arc<[PreparedCpuGraphDispatch]>,
    cpu_use_counts: Arc<HashMap<EffectMaterializationId, usize>>,
    cpu_input_materialization: EffectMaterializationId,
    cpu_output_materialization: EffectMaterializationId,
    gpu_plan: Arc<CompiledEffectGpuPlan>,
    cpu_completion_token: EffectCompletionToken,
    transfer_wait: EffectCompletionToken,
    gpu_input_token: EffectCompletionToken,
    output_token: EffectCompletionToken,
    frame_extent: EffectFrameExtent,
    cpu_required_working_bytes: usize,
}

impl PreparedHeterogeneousEffectWork {
    /// Plan and prepare the currently executable CPU-F32→GPU-F32 unary route.
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
        let mut transfer = None;
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
                } if !entered_gpu && transfer.is_none() => {
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
                } if transfer.is_none()
                    && !cpu_nodes.is_empty()
                    && from.format.precision == EffectWorkingPrecision::Float32
                    && to.format.precision == EffectWorkingPrecision::Float32
                    && lane_by_id(environment, from.lane)
                        .is_some_and(|lane| lane.backend() == EffectProcessingBackend::Cpu)
                    && lane_by_id(environment, to.lane)
                        .is_some_and(|lane| lane.backend() == EffectProcessingBackend::Gpu) =>
                {
                    transfer = Some((*input, *output, *wait, *signal));
                    entered_gpu = true;
                }
                EffectGraphExecutionStep::Dispatch {
                    node,
                    backend: EffectProcessingBackend::Gpu,
                    precision: EffectWorkingPrecision::Float32,
                    ..
                } if entered_gpu => gpu_nodes.push(*node),
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
        let (cpu_materialization, gpu_materialization, transfer_wait, gpu_input_token) = transfer
            .ok_or(
            PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                reason: "route_requires_exactly_one_cpu_to_gpu_transfer",
            },
        )?;
        let cpu_completion = plan.materialization(cpu_materialization).ok_or(
            PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                reason: "cpu_materialization_missing",
            },
        )?;
        let gpu_input = plan.materialization(gpu_materialization).ok_or(
            PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                reason: "gpu_input_materialization_missing",
            },
        )?;
        if cpu_completion.completion != transfer_wait
            || gpu_input.completion != gpu_input_token
            || cpu_completion.value
                != *cpu_nodes.last().ok_or(
                    PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                        reason: "cpu_prefix_missing",
                    },
                )?
            || gpu_input.value != cpu_completion.value
        {
            return Err(
                PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                    reason: "cpu_to_gpu_completion_tokens_do_not_match",
                },
            );
        }
        validate_cpu_dag_gpu_tail_partition(
            &compiled,
            &plan,
            &cpu_dispatches,
            cpu_materialization,
            &gpu_nodes,
        )?;
        let gpu_plan = Arc::new(lower_effect_graph_nodes_to_gpu_plan(&compiled, &gpu_nodes)?);
        if gpu_plan.source_value() != cpu_completion.value {
            return Err(
                PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                    reason: "gpu_tail_does_not_consume_transferred_cpu_value",
                },
            );
        }
        let frame_extent = plan.frame_extent();
        let cpu_input_materialization = plan.input_materialization();
        let cpu_use_counts = cpu_materialization_use_counts(&cpu_dispatches)?;
        let cpu_required_working_bytes = cpu_prefix_working_bytes(
            &compiled,
            &cpu_dispatches,
            &cpu_use_counts,
            cpu_input_materialization,
            cpu_materialization,
            frame_extent,
        )?;
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
            cpu_output_materialization: cpu_materialization,
            gpu_plan,
            cpu_completion_token: cpu_completion.completion,
            transfer_wait,
            gpu_input_token,
            output_token,
            frame_extent,
            cpu_required_working_bytes,
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

    /// Exact fused GPU suffix.
    pub fn gpu_plan(&self) -> &CompiledEffectGpuPlan {
        &self.gpu_plan
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
    /// Success proves only `completed_cpu_token`. Upload and GPU-output tokens
    /// remain pending in the returned evidence. This Module never catches a
    /// suffix failure and reinterprets the complete graph on CPU.
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
        let pixels = execute_cpu_dag_prefix(
            &self.compiled,
            &self.cpu_dispatches,
            &self.cpu_use_counts,
            self.cpu_input_materialization,
            self.cpu_output_materialization,
            self.frame_extent,
            input,
            frame_seed,
            &mut checkpoint_result,
        )?;
        Ok(PreparedHeterogeneousCpuCompletion {
            pixels,
            execution_plan: Arc::clone(&self.plan),
            gpu_plan: Arc::clone(&self.gpu_plan),
            evidence: HeterogeneousCpuCompletionEvidence {
                graph_fingerprint: self.compiled.semantic_fingerprint(),
                generation,
                frame_extent: self.frame_extent,
                frame_seed,
                working_color_space,
                completed_cpu_nodes: Arc::clone(&self.cpu_nodes),
                completed_cpu_token: self.cpu_completion_token,
                required_transfer_wait: self.transfer_wait,
                pending_gpu_input_token: self.gpu_input_token,
                pending_output_token: self.output_token,
            },
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_cpu_dag_prefix(
    compiled: &CompiledEffectGraph,
    dispatches: &[PreparedCpuGraphDispatch],
    use_counts: &HashMap<EffectMaterializationId, usize>,
    input_materialization: EffectMaterializationId,
    output_materialization: EffectMaterializationId,
    extent: EffectFrameExtent,
    input: &[[f32; 4]],
    frame_seed: i64,
    checkpoint: &mut impl FnMut() -> Result<(), PreparedHeterogeneousEffectWorkError>,
) -> Result<Vec<[f32; 4]>, PreparedHeterogeneousEffectWorkError> {
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
            EffectGraphNodeKind::Source | EffectGraphNodeKind::MaskSource { .. } => {
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
    let pixels = outputs
        .remove(&output_materialization)
        .ok_or(PreparedHeterogeneousEffectWorkError::InvalidCpuPrefix { node: output_node })?;
    if !outputs.is_empty() || remaining.values().any(|remaining| *remaining != 0) {
        return Err(PreparedHeterogeneousEffectWorkError::InvalidCpuPrefix { node: output_node });
    }
    Ok(pixels)
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
    output_materialization: EffectMaterializationId,
    extent: EffectFrameExtent,
) -> Result<usize, PreparedHeterogeneousEffectWorkError> {
    let frame_bytes = usize::try_from(frame_bytes(extent, EffectWorkingPrecision::Float32)?)
        .map_err(|_| PreparedHeterogeneousEffectWorkError::InputSizeOverflow)?;
    let mut remaining = use_counts.clone();
    let mut live_materializations = HashSet::from([input_materialization]);
    let mut live_frames = 1_usize;
    let mut peak_owned_frames = live_frames;
    for dispatch in dispatches {
        if dispatch.inputs.is_empty() {
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

        let node = compiled.graph().node(dispatch.node).ok_or(
            PreparedHeterogeneousEffectWorkError::InvalidCpuPrefix { node: dispatch.node },
        )?;
        let scratch_frames = cpu_node_scratch_frames(node)?;
        peak_owned_frames = peak_owned_frames.max(
            live_frames
                .checked_add(scratch_frames)
                .ok_or(PreparedHeterogeneousEffectWorkError::InputSizeOverflow)?,
        );
        live_frames = live_frames.checked_sub(dispatch.inputs.len().saturating_sub(1)).ok_or(
            PreparedHeterogeneousEffectWorkError::InvalidCpuPrefix { node: dispatch.node },
        )?;
        if !live_materializations.insert(dispatch.output) {
            return Err(PreparedHeterogeneousEffectWorkError::InvalidCpuPrefix {
                node: dispatch.node,
            });
        }
    }
    if live_frames != 1
        || live_materializations != HashSet::from([output_materialization])
        || remaining.values().any(|remaining| *remaining != 0)
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

fn validate_cpu_dag_gpu_tail_partition(
    compiled: &CompiledEffectGraph,
    plan: &CompiledEffectValueExecutionPlan,
    cpu_dispatches: &[PreparedCpuGraphDispatch],
    cpu_output_materialization: EffectMaterializationId,
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
    let output = plan.materialization(cpu_output_materialization).ok_or(
        PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
            reason: "cpu_transfer_materialization_missing",
        },
    )?;
    if !produced.contains(&cpu_output_materialization)
        || cpu_dispatches.last().map(|dispatch| dispatch.node) != Some(output.value())
    {
        return Err(
            PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                reason: "cpu_dag_does_not_end_at_the_transfer_value",
            },
        );
    }
    Ok(())
}

fn cpu_materialization_use_counts(
    dispatches: &[PreparedCpuGraphDispatch],
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
        EffectGraphNodeKind::Blend { .. } | EffectGraphNodeKind::Mask { .. } => Ok(0),
        EffectGraphNodeKind::MultiInput { inputs, .. } if !inputs.is_empty() => Ok(0),
        EffectGraphNodeKind::Source
        | EffectGraphNodeKind::MaskSource { .. }
        | EffectGraphNodeKind::MultiInput { .. } => {
            Err(PreparedHeterogeneousEffectWorkError::UnsupportedCpuOperation { node: node.id })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        adjustment::apply_render_op_f32, compile_reference_render_graph,
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
        assert_eq!(work.gpu_plan().node_ids().len(), 2);
        assert_eq!(work.gpu_plan().source_value(), work.cpu_nodes()[0]);
        assert_eq!(work.cpu_required_working_bytes(), 192);
        assert_eq!(
            work.gpu_plan().operations().len(),
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
        assert_eq!(completion.pixels(), reference);
        assert_eq!(completion.execution_plan(), work.plan());
        assert_eq!(
            completion.evidence().completed_cpu_token(),
            completion.evidence().required_transfer_wait()
        );
        assert_ne!(
            completion.evidence().completed_cpu_token(),
            completion.evidence().pending_gpu_input_token()
        );
        assert_eq!(
            completion.gpu_plan().graph_fingerprint(),
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
        assert_eq!(work.gpu_plan().node_ids().len(), 1);
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
        assert_eq!(completion.pixels(), expected);
        assert_eq!(
            completion.evidence().completed_cpu_nodes(),
            work.cpu_nodes()
        );
        assert_eq!(
            completion.gpu_plan().source_value(),
            work.cpu_nodes()[2],
            "the upload consumes the joined CPU graph value"
        );
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
    fn route_rejects_a_gpu_dag_tail_without_executing_pixels() {
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
        let output = builder.add_blend(left, right, BlendMode::Normal, 0.5);
        builder.set_current_output(output);
        let compiled = compile_reference_render_graph(builder.finish()).expect("DAG graph");
        assert!(matches!(
            PreparedHeterogeneousEffectWork::prepare(
                compiled,
                &test_environment(),
                request(EffectFrameExtent::new(8, 8)),
            ),
            Err(PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape { .. })
        ));
    }
}
