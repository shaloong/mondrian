//! Device-side schedule for one frozen finite temporal value program.
//!
//! The scalar executor in the parent module evaluates every time-expanded value
//! on the CPU. This module freezes the same program into an ordered device
//! schedule: leaves name the exact source instants and auxiliary rasters a host
//! must deliver, and every remaining step is one already-admitted GPU pass over
//! values that live on the device. Nothing here touches pixels, and no step is
//! invented for a graph shape the device cannot run: an unsupported value fails
//! the whole lowering so callers keep the CPU executor instead of executing a
//! silently different graph.

use super::*;
use crate::gpu_pipeline::EffectGpuSpatialPass;
use crate::gpu_plan::CompiledEffectGpuPlan;
use mondrian_core::BlendMode;
use std::fmt;

/// One value in the device schedule, addressed exactly like the scalar program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TemporalGpuValue {
    /// One exact source instant.
    Source(TimelineTime),
    /// One host auxiliary raster, resolved by its evaluation context and slot.
    Auxiliary {
        /// Exact author time of the context that reads this slot.
        context_time: TimelineTime,
        /// Graph-declared external slot.
        slot: u32,
    },
    /// One graph value inside one time-expanded context.
    Graph {
        /// Index into the frozen context list.
        context: usize,
        /// Node that produced the value.
        node_id: EffectGraphNodeId,
    },
}

impl TemporalGpuValue {
    /// Whether this value is delivered by the host instead of computed here.
    pub const fn is_leaf(self) -> bool {
        !matches!(self, Self::Graph { .. })
    }
}

/// Exact host request for one leaf value.
#[derive(Debug, Clone, Copy)]
pub enum TemporalGpuLeafRequest {
    /// A raw source instant.
    Source(EffectTemporalFrameRequest),
    /// A host auxiliary raster for one evaluation context.
    Auxiliary(EffectTemporalAuxiliaryDemand),
}

/// One leaf the host must materialize before the device schedule runs.
#[derive(Debug, Clone, Copy)]
pub struct TemporalGpuLeaf {
    value: TemporalGpuValue,
    request: TemporalGpuLeafRequest,
}

impl TemporalGpuLeaf {
    /// Value this raster feeds.
    pub const fn value(self) -> TemporalGpuValue {
        self.value
    }

    /// Exact host request.
    pub const fn request(self) -> TemporalGpuLeafRequest {
        self.request
    }
}

/// One device pass over values that are already resident.
#[derive(Debug, Clone)]
pub enum TemporalGpuStepOperation {
    /// One admitted point-wise operation.
    Point {
        /// Input value.
        input: TemporalGpuValue,
        /// Admitted fused plan; it reads `input` and writes the step's value.
        plan: Arc<CompiledEffectGpuPlan>,
    },
    /// One admitted spatial pass over a single value.
    Spatial {
        /// Input value.
        input: TemporalGpuValue,
        /// Admitted pass.
        pass: EffectGpuSpatialPass,
    },
    /// Straight-alpha cross-fade between the current instant and one sample.
    TemporalBlend {
        /// Value at the context's own instant.
        input: TemporalGpuValue,
        /// Value sampled at another instant.
        sample: TemporalGpuValue,
        /// Weight of `sample`, matching `coverage::mix_straight_rgba`.
        mix: f32,
    },
    /// Composite one value over another with the graph's own blend policy.
    Blend {
        /// Base value.
        base: TemporalGpuValue,
        /// Overlay value.
        overlay: TemporalGpuValue,
        /// Overlay weight.
        opacity: f32,
        /// Blend policy.
        blend_mode: BlendMode,
    },
    /// An auxiliary pass that reads a second, graph-local value.
    Auxiliary {
        /// Primary value.
        input: TemporalGpuValue,
        /// Auxiliary value; it is a resident value, never a host slot.
        auxiliary: TemporalGpuValue,
        /// Admitted pass with no external slot.
        pass: EffectGpuSpatialPass,
    },
}

/// One scheduled device pass.
#[derive(Debug, Clone)]
pub struct TemporalGpuStep {
    value: TemporalGpuValue,
    operation: TemporalGpuStepOperation,
}

impl TemporalGpuStep {
    /// Value this pass writes.
    pub const fn value(&self) -> TemporalGpuValue {
        self.value
    }

    /// Operation this pass performs.
    pub const fn operation(&self) -> &TemporalGpuStepOperation {
        &self.operation
    }
}

/// Why one frozen temporal program cannot be lowered onto the device.
///
/// Every variant is a fail-closed refusal: the caller must keep the CPU
/// executor. There is no partial schedule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectTemporalGpuPlanBlocker {
    /// The frozen request covers a sub-frame ROI.
    PartialRoi,
    /// The graph uses a node shape this schedule does not place on the device.
    UnsupportedNode {
        /// Refusing node.
        node_id: EffectGraphNodeId,
        /// Node kind name.
        kind: &'static str,
    },
    /// The node's operation has no admitted GPU pass.
    UnsupportedOperation {
        /// Refusing node.
        node_id: EffectGraphNodeId,
        /// Operation name.
        op: &'static str,
    },
    /// A point plan for the node was refused by the shared GPU admission.
    NodeNotAdmitted {
        /// Refusing node.
        node_id: EffectGraphNodeId,
        /// Exact shared admission reason.
        reason: String,
    },
    /// A node input address does not resolve inside its context.
    MissingGraphNode {
        /// Node whose input could not be addressed.
        node_id: EffectGraphNodeId,
    },
    /// A temporal operation has no expanded sample edge.
    MissingSampleEdge {
        /// Refusing node.
        node_id: EffectGraphNodeId,
    },
    /// A demanded leaf has no exact host request.
    MissingLeafRequest {
        /// Value whose request is missing.
        value: TemporalGpuValue,
    },
    /// A source instant resolved to an extent the output cannot consume.
    ExtentMismatch {
        /// Value whose extent differs from the output.
        value: TemporalGpuValue,
    },
    /// The time-expanded context could not be read back.
    MissingContext,
}

impl fmt::Display for EffectTemporalGpuPlanBlocker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PartialRoi => {
                formatter.write_str("frozen temporal request covers a sub-frame ROI")
            }
            Self::UnsupportedNode { node_id, kind } => {
                write!(
                    formatter,
                    "temporal node {:?} has unsupported kind {kind}",
                    node_id
                )
            }
            Self::UnsupportedOperation { node_id, op } => {
                write!(
                    formatter,
                    "temporal node {:?} has no device pass for {op}",
                    node_id
                )
            }
            Self::NodeNotAdmitted { node_id, reason } => {
                write!(
                    formatter,
                    "temporal node {:?} was not admitted: {reason}",
                    node_id
                )
            }
            Self::MissingGraphNode { node_id } => {
                write!(
                    formatter,
                    "temporal node {:?} has an unresolvable input",
                    node_id
                )
            }
            Self::MissingSampleEdge { node_id } => {
                write!(
                    formatter,
                    "temporal node {:?} has no expanded sample edge",
                    node_id
                )
            }
            Self::MissingLeafRequest { value } => {
                write!(
                    formatter,
                    "temporal leaf {value:?} has no exact host request"
                )
            }
            Self::ExtentMismatch { value } => {
                write!(
                    formatter,
                    "temporal source {value:?} does not match the output extent"
                )
            }
            Self::MissingContext => formatter.write_str("time-expanded graph context is missing"),
        }
    }
}

impl std::error::Error for EffectTemporalGpuPlanBlocker {}

/// One frozen, fully admitted device schedule for a finite temporal program.
///
/// The schedule is ordered: every step reads only values produced before it, so
/// a backend may record the passes in array order and materialize the root once.
#[derive(Debug, Clone)]
pub struct PreparedEffectTemporalGpuExecution {
    leaves: Arc<[TemporalGpuLeaf]>,
    steps: Arc<[TemporalGpuStep]>,
    root: TemporalGpuValue,
    request: EffectTemporalExecutionRequest,
    fingerprint: [u8; 32],
    cache_identity: [u8; 32],
}

impl PreparedEffectTemporalGpuExecution {
    /// Exact host rasters this schedule needs, in demand order.
    pub fn leaves(&self) -> &[TemporalGpuLeaf] {
        &self.leaves
    }

    /// Ordered device passes.
    pub fn steps(&self) -> &[TemporalGpuStep] {
        &self.steps
    }

    /// Value the caller must read back.
    pub const fn root(&self) -> TemporalGpuValue {
        self.root
    }

    /// Exact scheduler request this schedule was frozen from.
    pub const fn request(&self) -> &EffectTemporalExecutionRequest {
        &self.request
    }

    /// Output extent every pass works in.
    pub const fn frame_extent(&self) -> EffectFrameExtent {
        self.request.frame_extent()
    }

    /// Output sampling.
    pub const fn sampling(&self) -> mondrian_core::ImageSampling {
        self.request.sampling()
    }

    /// Frozen program fingerprint this schedule was derived from.
    pub const fn fingerprint(&self) -> [u8; 32] {
        self.fingerprint
    }

    /// Identity of a device-completed tile for this exact schedule.
    pub const fn cache_identity(&self) -> [u8; 32] {
        self.cache_identity
    }

    /// Number of device passes.
    pub fn step_count(&self) -> usize {
        self.steps.len()
    }

    /// Whether the schedule needs no device pass beyond the host leaves.
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }
}

impl EffectTemporalExecutionOutput {
    /// Package one device-completed root tile under this schedule's identity.
    pub fn from_device_tile(
        tile: EffectFrameTileF32,
        cache_identity: [u8; 32],
        provider_requests: usize,
        peak_working_bytes: usize,
    ) -> Self {
        Self {
            tile,
            cache_identity,
            provider_requests,
            execution_tiles: 1,
            peak_working_bytes,
        }
    }
}

impl PreparedEffectTemporalExecution {
    /// Freeze this program into an ordered device schedule.
    ///
    /// Returns a typed blocker instead of a partial schedule whenever the graph
    /// uses a shape the device cannot run exactly.
    pub fn plan_gpu_execution(
        &self,
    ) -> Result<PreparedEffectTemporalGpuExecution, EffectTemporalGpuPlanBlocker> {
        let request = self.request();
        let roi = request.output_roi();
        let frame = request.frame_extent();
        if roi.x() != 0
            || roi.y() != 0
            || roi.width() != frame.width()
            || roi.height() != frame.height()
        {
            return Err(EffectTemporalGpuPlanBlocker::PartialRoi);
        }
        let program = self.program.as_ref();
        let extent = request.frame_extent();
        let mut leaves = Vec::new();
        let mut steps = Vec::new();
        for address in program.ordered_values() {
            let value = gpu_value(program, *address)?;
            match *address {
                TemporalValueAddress::Source(time) => {
                    let exact = self
                        .demands()
                        .requests()
                        .iter()
                        .copied()
                        .find(|request| request.time() == time)
                        .ok_or(EffectTemporalGpuPlanBlocker::MissingLeafRequest { value })?;
                    if exact.frame_extent() != extent {
                        return Err(EffectTemporalGpuPlanBlocker::ExtentMismatch { value });
                    }
                    leaves.push(TemporalGpuLeaf {
                        value,
                        request: TemporalGpuLeafRequest::Source(exact),
                    });
                }
                TemporalValueAddress::Auxiliary { context, slot } => {
                    let context_time = program
                        .context(context)
                        .map_err(|_| EffectTemporalGpuPlanBlocker::MissingContext)?
                        .time;
                    let exact = self
                        .auxiliary_demands()
                        .demands()
                        .iter()
                        .copied()
                        .find(|demand| {
                            demand.context_time() == context_time && demand.slot() == slot
                        })
                        .ok_or(EffectTemporalGpuPlanBlocker::MissingLeafRequest { value })?;
                    leaves.push(TemporalGpuLeaf {
                        value,
                        request: TemporalGpuLeafRequest::Auxiliary(exact),
                    });
                }
                TemporalValueAddress::Graph { context, node_id } => {
                    let frozen = program
                        .context(context)
                        .map_err(|_| EffectTemporalGpuPlanBlocker::MissingContext)?;
                    let modes = frozen
                        .graph
                        .node_execution_modes(node_id)
                        .ok_or(EffectTemporalGpuPlanBlocker::MissingGraphNode { node_id })?;
                    if !modes.contains(
                        EffectProcessingBackend::Gpu,
                        EffectWorkingPrecision::Float32,
                    ) {
                        return Err(EffectTemporalGpuPlanBlocker::NodeNotAdmitted {
                            node_id,
                            reason: "node does not admit GPU Float32 execution".to_owned(),
                        });
                    }
                    let node = frozen
                        .graph
                        .graph()
                        .node(node_id)
                        .ok_or(EffectTemporalGpuPlanBlocker::MissingGraphNode { node_id })?;
                    let operation = match &node.kind {
                        EffectGraphNodeKind::UnaryEffect { input, op }
                        | EffectGraphNodeKind::DomainEffect { input, op, .. } => {
                            let input = gpu_value(program, node_input(program, context, *input)?)?;
                            if let crate::EffectRenderOp::TemporalFrameBlend { mix, .. } = op {
                                let sample = program.temporal_sample(*address).ok_or(
                                    EffectTemporalGpuPlanBlocker::MissingSampleEdge { node_id },
                                )?;
                                TemporalGpuStepOperation::TemporalBlend {
                                    input,
                                    sample: gpu_value(program, sample)?,
                                    mix: *mix,
                                }
                            } else if let Some(pass) = crate::gpu_pipeline::spatial_pass_for(op) {
                                TemporalGpuStepOperation::Spatial { input, pass }
                            } else {
                                let plan =
                                    crate::gpu_plan::lower_effect_graph_node_to_gpu_point_plan(
                                        &frozen.graph,
                                        node_id,
                                    )
                                    .map_err(|blocker| {
                                        EffectTemporalGpuPlanBlocker::NodeNotAdmitted {
                                            node_id,
                                            reason: blocker.to_string(),
                                        }
                                    })?;
                                TemporalGpuStepOperation::Point { input, plan: Arc::new(plan) }
                            }
                        }
                        EffectGraphNodeKind::Blend { base, overlay, blend_mode, opacity } => {
                            TemporalGpuStepOperation::Blend {
                                base: gpu_value(program, node_input(program, context, *base)?)?,
                                overlay: gpu_value(
                                    program,
                                    node_input(program, context, *overlay)?,
                                )?,
                                opacity: *opacity,
                                blend_mode: *blend_mode,
                            }
                        }
                        EffectGraphNodeKind::AuxiliaryEffect {
                            input,
                            auxiliary,
                            op,
                            role,
                            sampling,
                        } => {
                            let pass = crate::gpu_pipeline::lower_auxiliary_spatial_pass(
                                op, *role, *sampling, None,
                            )
                            .ok_or(
                                EffectTemporalGpuPlanBlocker::UnsupportedOperation {
                                    node_id,
                                    op: crate::gpu_plan::operation_name(op),
                                },
                            )?;
                            TemporalGpuStepOperation::Auxiliary {
                                input: gpu_value(program, node_input(program, context, *input)?)?,
                                auxiliary: gpu_value(
                                    program,
                                    node_input(program, context, *auxiliary)?,
                                )?,
                                pass,
                            }
                        }
                        kind => {
                            return Err(EffectTemporalGpuPlanBlocker::UnsupportedNode {
                                node_id,
                                kind: crate::gpu_plan::node_kind_name(kind),
                            });
                        }
                    };
                    steps.push(TemporalGpuStep { value, operation });
                }
            }
        }
        let root = gpu_value(program, program.root())?;
        Ok(PreparedEffectTemporalGpuExecution {
            leaves: leaves.into(),
            steps: steps.into(),
            root,
            request: request.clone(),
            fingerprint: program.fingerprint(),
            cache_identity: device_cache_identity(program.fingerprint(), request),
        })
    }
}

fn gpu_value(
    program: &PreparedTemporalValueProgram,
    address: TemporalValueAddress,
) -> Result<TemporalGpuValue, EffectTemporalGpuPlanBlocker> {
    Ok(match address {
        TemporalValueAddress::Source(time) => TemporalGpuValue::Source(time),
        TemporalValueAddress::Auxiliary { context, slot } => TemporalGpuValue::Auxiliary {
            context_time: program
                .context(context)
                .map_err(|_| EffectTemporalGpuPlanBlocker::MissingContext)?
                .time,
            slot,
        },
        TemporalValueAddress::Graph { context, node_id } => {
            program
                .context(context)
                .map_err(|_| EffectTemporalGpuPlanBlocker::MissingContext)?;
            TemporalGpuValue::Graph { context, node_id }
        }
    })
}

/// Resolve one node input inside its own time-expanded context.
fn node_input(
    program: &PreparedTemporalValueProgram,
    context: usize,
    node_id: EffectGraphNodeId,
) -> Result<TemporalValueAddress, EffectTemporalGpuPlanBlocker> {
    program
        .address_for_input(context, node_id)
        .map_err(|_| EffectTemporalGpuPlanBlocker::MissingGraphNode { node_id })
}

/// Stable identity of a device-completed temporal tile.
fn device_cache_identity(
    fingerprint: [u8; 32],
    request: &EffectTemporalExecutionRequest,
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"mondrian.effect-temporal-device-output.v1");
    digest.update(fingerprint);
    digest.update(request.generation().to_le_bytes());
    digest.update(request.output_time().numerator().to_le_bytes());
    digest.update(request.output_time().denominator().to_le_bytes());
    digest.update(request.output_frame_seed().to_le_bytes());
    let finalized = digest.finalize();
    let mut identity = [0u8; 32];
    identity.copy_from_slice(&finalized);
    identity
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::EffectType;
    use mondrian_core::{AssetId, PropertyValue, VisualSourceReference, WorkingColorSpace};

    /// Echo over an optional Set Matte, exactly as production compiles it.
    fn echo_program(with_matte: bool) -> PreparedEffectProgram {
        let mut echo = crate::instantiate_effect_node(EffectType::Echo).unwrap();
        for (key, value) in [
            ("echo_time", PropertyValue::Float(-0.25)),
            ("operator", PropertyValue::Enum("normal".to_owned())),
            ("starting_intensity", PropertyValue::Float(0.5)),
        ] {
            echo.set_static_value_by_parameter(&EffectType::Echo.parameter_id(key).unwrap(), value)
                .unwrap();
        }
        let mut effects = Vec::new();
        if with_matte {
            let mut matte = crate::instantiate_effect_node(EffectType::SetMatte).unwrap();
            matte
                .set_static_value_by_parameter(
                    &EffectType::SetMatte.parameter_id("matte_source").unwrap(),
                    PropertyValue::VisualSource(VisualSourceReference::Asset {
                        asset_id: AssetId::new(),
                        retime: Default::default(),
                    }),
                )
                .unwrap();
            effects.push(matte);
        }
        effects.push(echo);
        PreparedEffectProgram::prepare(&effects, &[], WorkingColorSpace::LinearRec709).unwrap()
    }

    fn extent() -> EffectFrameExtent {
        EffectFrameExtent::new(3, 2)
    }

    fn frozen(
        program: &PreparedEffectProgram,
        now: TimelineTime,
        roi: EffectPixelRoi,
    ) -> PreparedEffectTemporalExecution {
        let request = EffectTemporalExecutionRequest::new(
            1,
            EffectExecutionContinuity::Discontinuous,
            now,
            extent(),
            mondrian_core::ImageSampling::UNIT_SQUARE,
            roi,
            ExecutionCancellationToken::new(),
        );
        prepare_temporal_frame_execution(program, &request, |_| Ok(1)).unwrap()
    }

    fn step_inputs(operation: &TemporalGpuStepOperation) -> Vec<TemporalGpuValue> {
        match operation {
            TemporalGpuStepOperation::Point { input, .. }
            | TemporalGpuStepOperation::Spatial { input, .. } => vec![*input],
            TemporalGpuStepOperation::TemporalBlend { input, sample, .. } => {
                vec![*input, *sample]
            }
            TemporalGpuStepOperation::Blend { base, overlay, .. } => vec![*base, *overlay],
            TemporalGpuStepOperation::Auxiliary { input, auxiliary, .. } => {
                vec![*input, *auxiliary]
            }
        }
    }

    #[test]
    fn echo_with_set_matte_lowers_to_one_ordered_device_schedule() {
        let program = echo_program(true);
        let now = TimelineTime::new(1, 2).unwrap();
        let plan = frozen(&program, now, extent().full_frame_roi()).plan_gpu_execution().unwrap();

        // Two evaluation contexts, each with its own raw source and its own
        // auxiliary raster on the same compiled slot.
        let leaves = plan.leaves();
        assert_eq!(leaves.len(), 4, "{leaves:?}");
        let source_times = leaves
            .iter()
            .filter_map(|leaf| match leaf.request() {
                TemporalGpuLeafRequest::Source(request) => Some(request.time()),
                TemporalGpuLeafRequest::Auxiliary(_) => None,
            })
            .collect::<Vec<_>>();
        let auxiliary = leaves
            .iter()
            .filter_map(|leaf| match leaf.request() {
                TemporalGpuLeafRequest::Source(_) => None,
                TemporalGpuLeafRequest::Auxiliary(demand) => {
                    Some((demand.context_time(), demand.slot()))
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(source_times.len(), 2, "{source_times:?}");
        assert!(source_times.contains(&now));
        assert_eq!(auxiliary.len(), 2, "{auxiliary:?}");
        assert!(auxiliary.contains(&(now, 0)));
        assert!(
            auxiliary.iter().all(|(time, slot)| *slot == 0 && *time <= now),
            "{auxiliary:?}"
        );

        // Set Matte is a spatial pass with no host slot: the auxiliary raster is
        // a resident value inside the schedule.
        assert!(
            plan.steps().iter().any(|step| matches!(
                step.operation(),
                TemporalGpuStepOperation::Auxiliary {
                    pass: EffectGpuSpatialPass::SetMatte { external_slot: None, .. },
                    ..
                }
            )),
            "{:?}",
            plan.steps()
        );

        // Every pass reads only values that already exist, so a backend may
        // record them in array order.
        let mut available = leaves.iter().map(|leaf| leaf.value()).collect::<HashSet<_>>();
        for step in plan.steps() {
            for input in step_inputs(step.operation()) {
                assert!(
                    available.contains(&input),
                    "step {step:?} reads an earlier value"
                );
            }
            available.insert(step.value());
        }
        assert!(available.contains(&plan.root()));
        assert_eq!(plan.root(), plan.steps().last().unwrap().value());
    }

    #[test]
    fn echo_without_a_second_input_schedules_only_source_leaves() {
        let program = echo_program(false);
        let now = TimelineTime::new(1, 2).unwrap();
        let plan = frozen(&program, now, extent().full_frame_roi()).plan_gpu_execution().unwrap();
        assert!(!plan.leaves().is_empty());
        assert!(
            plan.leaves()
                .iter()
                .all(|leaf| matches!(leaf.request(), TemporalGpuLeafRequest::Source(_))),
            "{:?}",
            plan.leaves()
        );
        assert!(plan.steps().iter().any(|step| matches!(
            step.operation(),
            TemporalGpuStepOperation::TemporalBlend { .. }
        )));
    }

    #[test]
    fn device_schedule_refuses_a_partial_output_roi() {
        let program = echo_program(true);
        let now = TimelineTime::new(1, 2).unwrap();
        let execution = frozen(&program, now, EffectPixelRoi::new(0, 0, 2, 2));
        assert_eq!(
            execution.plan_gpu_execution().unwrap_err(),
            EffectTemporalGpuPlanBlocker::PartialRoi
        );
    }

    #[test]
    fn device_identity_follows_the_frozen_instant() {
        let program = echo_program(true);
        let now = TimelineTime::new(1, 2).unwrap();
        let later = TimelineTime::new(3, 4).unwrap();
        let first = frozen(&program, now, extent().full_frame_roi()).plan_gpu_execution().unwrap();
        let repeat = frozen(&program, now, extent().full_frame_roi()).plan_gpu_execution().unwrap();
        let other =
            frozen(&program, later, extent().full_frame_roi()).plan_gpu_execution().unwrap();
        assert_eq!(first.cache_identity(), repeat.cache_identity());
        assert_eq!(first.fingerprint(), repeat.fingerprint());
        assert_ne!(first.cache_identity(), other.cache_identity());
    }

    #[test]
    fn admitting_gpu_f32_does_not_open_a_single_frame_route() {
        // The contract widening above only makes the device schedule legal. A
        // single-frame executor must still refuse the cross-time edge by name,
        // so an echo layer can never be recorded by the point or spatial
        // compositor route.
        let program = echo_program(false);
        let now = TimelineTime::new(1, 2).unwrap();
        let execution = frozen(&program, now, extent().full_frame_roi());
        let graph = execution.graph();

        // No spatial pass claims the edge, so the spatial lowerer falls through
        // to the point lowerer and refuses it by name.
        assert!(
            crate::spatial_pass_for(&crate::EffectRenderOp::TemporalFrameBlend {
                sample_offset: TimelineTime::new(-1, 4).unwrap(),
                mix: 1.0,
            })
            .is_none()
        );

        let spatial = crate::lower_effect_graph_to_gpu_pipeline(graph)
            .expect_err("an echo graph has no single-frame spatial pipeline");
        assert!(
            spatial.to_string().contains("temporal_frame_blend")
                || spatial.to_string().contains("disconnected_chain"),
            "{spatial}"
        );
    }
}
