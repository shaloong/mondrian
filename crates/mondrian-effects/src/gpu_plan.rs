//! Backend-neutral lowering for GPU working-space effect execution.
//!
//! The plan deliberately contains no graphics API objects. It is the contract
//! between effect graph compilation and renderer backends that execute fused
//! point operations over linear floating-point working-space pixels.

use crate::{CompiledEffectGraph, EffectGraphNodeId, EffectGraphNodeKind, EffectRenderOp};

/// Maximum number of point operations fused into one GPU pass.
pub const MAX_FUSED_GPU_EFFECT_OPS: usize = 8;

/// One pointwise operation executable by a working-space GPU backend.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EffectGpuPointOp {
    /// Exposure, contrast, and saturation adjustment.
    ColorAdjust {
        exposure: f32,
        contrast: f32,
        saturation: f32,
    },
    /// Temperature and tint adjustment.
    WhiteBalance { temperature: f32, tint: f32 },
    /// Source-relative radial vignette.
    Vignette { intensity: f32, feather: f32 },
    /// Deterministic monochromatic grain.
    Grain { amount: f32 },
}

/// Immutable renderer-facing GPU plan for a compiled effect graph.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledEffectGpuPlan {
    operations: Vec<EffectGpuPointOp>,
    graph_signature: u64,
}

impl CompiledEffectGpuPlan {
    /// Ordered point operations to execute for each source pixel.
    pub fn operations(&self) -> &[EffectGpuPointOp] {
        &self.operations
    }

    /// Signature of the compiled source graph represented by this plan.
    pub fn graph_signature(&self) -> u64 {
        self.graph_signature
    }

    /// Whether this plan performs no pixel changes.
    pub fn is_identity(&self) -> bool {
        self.operations.is_empty()
    }
}

/// Stable reason a compiled graph cannot use the fused GPU point path.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EffectGpuPlanBlocker {
    /// A graph node is not part of a single-source unary chain.
    #[error("effect node {node_id:?} has unsupported GPU topology {kind}")]
    UnsupportedTopology {
        /// Node that introduced the unsupported topology.
        node_id: EffectGraphNodeId,
        /// Stable topology label for diagnostics.
        kind: &'static str,
    },
    /// A unary render operation has no fused working-space implementation.
    #[error("effect node {node_id:?} uses unsupported GPU operation {op}")]
    UnsupportedOperation {
        /// Node containing the operation.
        node_id: EffectGraphNodeId,
        /// Stable operation label for diagnostics.
        op: &'static str,
    },
    /// The graph chain is malformed or does not follow its compiled schedule.
    #[error("effect node {node_id:?} does not consume the preceding GPU chain output")]
    DisconnectedChain {
        /// First disconnected node.
        node_id: EffectGraphNodeId,
    },
    /// The fused pass capacity would be exceeded.
    #[error("effect graph requires {actual} fused operations; maximum is {maximum}")]
    TooManyOperations {
        /// Requested operation count.
        actual: usize,
        /// Backend contract limit.
        maximum: usize,
    },
    /// The graph has no declared output.
    #[error("effect graph has no output node")]
    MissingOutput,
}

/// Lower a compiled graph into a backend-neutral fused GPU point plan.
pub fn lower_effect_graph_to_gpu_plan(
    compiled: &CompiledEffectGraph,
) -> Result<CompiledEffectGpuPlan, EffectGpuPlanBlocker> {
    let output = compiled.graph.output.ok_or(EffectGpuPlanBlocker::MissingOutput)?;
    let mut previous = None;
    let mut operations = Vec::new();
    for node_id in &compiled.schedule.ordered_nodes {
        let Some(node) = compiled.graph.node(*node_id) else {
            return Err(EffectGpuPlanBlocker::DisconnectedChain { node_id: *node_id });
        };
        match &node.kind {
            EffectGraphNodeKind::Source => {
                if previous.is_some() {
                    return Err(EffectGpuPlanBlocker::UnsupportedTopology {
                        node_id: *node_id,
                        kind: "multiple_source",
                    });
                }
            }
            EffectGraphNodeKind::UnaryEffect { input, op } => {
                if Some(*input) != previous {
                    return Err(EffectGpuPlanBlocker::DisconnectedChain { node_id: *node_id });
                }
                operations.push(lower_point_op(*node_id, op)?);
                if operations.len() > MAX_FUSED_GPU_EFFECT_OPS {
                    return Err(EffectGpuPlanBlocker::TooManyOperations {
                        actual: operations.len(),
                        maximum: MAX_FUSED_GPU_EFFECT_OPS,
                    });
                }
            }
            kind => {
                return Err(EffectGpuPlanBlocker::UnsupportedTopology {
                    node_id: *node_id,
                    kind: node_kind_name(kind),
                });
            }
        }
        previous = Some(*node_id);
    }
    if previous != Some(output) {
        return Err(EffectGpuPlanBlocker::DisconnectedChain { node_id: output });
    }
    Ok(CompiledEffectGpuPlan {
        operations,
        graph_signature: compiled.signature_hash,
    })
}

fn lower_point_op(
    node_id: EffectGraphNodeId,
    op: &EffectRenderOp,
) -> Result<EffectGpuPointOp, EffectGpuPlanBlocker> {
    match op {
        EffectRenderOp::ColorAdjust { exposure, contrast, saturation } => {
            Ok(EffectGpuPointOp::ColorAdjust {
                exposure: *exposure,
                contrast: *contrast,
                saturation: *saturation,
            })
        }
        EffectRenderOp::WhiteBalance { temperature, tint } => {
            Ok(EffectGpuPointOp::WhiteBalance { temperature: *temperature, tint: *tint })
        }
        EffectRenderOp::Vignette { intensity, feather } => {
            Ok(EffectGpuPointOp::Vignette { intensity: *intensity, feather: *feather })
        }
        EffectRenderOp::Grain { amount } => Ok(EffectGpuPointOp::Grain { amount: *amount }),
        unsupported => Err(EffectGpuPlanBlocker::UnsupportedOperation {
            node_id,
            op: operation_name(unsupported),
        }),
    }
}

fn node_kind_name(kind: &EffectGraphNodeKind) -> &'static str {
    match kind {
        EffectGraphNodeKind::Source => "source",
        EffectGraphNodeKind::UnaryEffect { .. } => "unary",
        EffectGraphNodeKind::Blend { .. } => "blend",
        EffectGraphNodeKind::Mask { .. } => "mask",
        EffectGraphNodeKind::MaskSource { .. } => "mask_source",
        EffectGraphNodeKind::MultiInput { .. } => "multi_input",
    }
}

fn operation_name(op: &EffectRenderOp) -> &'static str {
    match op {
        EffectRenderOp::ColorAdjust { .. } => "color_adjust",
        EffectRenderOp::WhiteBalance { .. } => "white_balance",
        EffectRenderOp::GaussianBlur { .. } => "gaussian_blur",
        EffectRenderOp::Sharpen { .. } => "sharpen",
        EffectRenderOp::Vignette { .. } => "vignette",
        EffectRenderOp::ChromaticAberration { .. } => "chromatic_aberration",
        EffectRenderOp::Grain { .. } => "grain",
        EffectRenderOp::Lut3D { .. } => "lut_3d",
        EffectRenderOp::Custom { .. } => "custom",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{get_or_compile_scheduled_render_graph, EffectGraphBuilderState};

    #[test]
    fn lowers_ordered_point_chain() {
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(EffectRenderOp::ColorAdjust {
            exposure: 1.0,
            contrast: 1.2,
            saturation: 0.8,
        });
        builder.append_unary(EffectRenderOp::Grain { amount: 0.25 });
        let compiled =
            get_or_compile_scheduled_render_graph(builder.finish()).expect("valid graph");
        let plan = lower_effect_graph_to_gpu_plan(&compiled).expect("supported point chain");
        assert_eq!(plan.operations().len(), 2);
        assert_eq!(plan.graph_signature(), compiled.signature_hash);
    }

    #[test]
    fn rejects_spatial_operation_explicitly() {
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(EffectRenderOp::GaussianBlur { radius: 3.0 });
        let compiled =
            get_or_compile_scheduled_render_graph(builder.finish()).expect("valid graph");
        assert!(matches!(
            lower_effect_graph_to_gpu_plan(&compiled),
            Err(EffectGpuPlanBlocker::UnsupportedOperation { op: "gaussian_blur", .. })
        ));
    }

    #[test]
    fn identity_graph_lowers_to_empty_plan() {
        let compiled = get_or_compile_scheduled_render_graph(crate::EffectRenderGraph::identity())
            .expect("valid identity graph");
        let plan = lower_effect_graph_to_gpu_plan(&compiled).expect("identity is GPU safe");
        assert!(plan.is_identity());
    }

    #[test]
    fn rejects_chain_beyond_fused_capacity() {
        let mut builder = EffectGraphBuilderState::new();
        for _ in 0..=MAX_FUSED_GPU_EFFECT_OPS {
            builder.append_unary(EffectRenderOp::Grain { amount: 0.1 });
        }
        let compiled =
            get_or_compile_scheduled_render_graph(builder.finish()).expect("valid graph");
        assert_eq!(
            lower_effect_graph_to_gpu_plan(&compiled),
            Err(EffectGpuPlanBlocker::TooManyOperations {
                actual: MAX_FUSED_GPU_EFFECT_OPS + 1,
                maximum: MAX_FUSED_GPU_EFFECT_OPS,
            })
        );
    }
}
