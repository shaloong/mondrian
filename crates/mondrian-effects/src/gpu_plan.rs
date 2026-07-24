//! Backend-neutral lowering for GPU working-space effect execution.
//!
//! The plan deliberately contains no graphics API objects. It is the contract
//! between effect graph compilation and renderer backends that execute fused
//! point operations over typed floating-point color-domain pixels.

use crate::{
    CompiledEffectGraph, EffectColorDomain, EffectGraphNodeId, EffectGraphNodeKind, EffectRenderOp,
};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex, OnceLock},
};

/// Maximum number of point operations fused into one GPU pass.
pub const MAX_FUSED_GPU_EFFECT_OPS: usize = 8;
const GPU_PLAN_CACHE_CAPACITY: usize = 256;

/// One pointwise operation executable by a working-space GPU backend.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EffectGpuPointOp {
    /// Exposure, contrast, and saturation adjustment.
    ColorAdjust {
        exposure: f32,
        contrast: f32,
        saturation: f32,
        /// CIE Y coefficients for the authored Sequence working space.
        luminance_coefficients: [f32; 3],
    },
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
    processing_domain: EffectColorDomain,
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

    /// Exact color domain in which the point operations must execute.
    pub fn processing_domain(&self) -> EffectColorDomain {
        self.processing_domain
    }

    /// Whether this plan performs no pixel changes.
    pub fn is_identity(&self) -> bool {
        self.operations.is_empty()
    }
}

/// Stable reason a compiled graph cannot use the fused GPU point path.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EffectGpuPlanBlocker {
    /// The compiled transition graph is not one supported preserving RGB domain.
    #[error("effect graph has unsupported GPU color-domain plan with {transitions} transitions")]
    UnsupportedColorDomainPlan {
        /// Number of explicit RGB-domain transitions in the compiled graph.
        transitions: usize,
    },
    /// A GPU point operation changes its processing domain.
    #[error("effect node {node_id:?} changes GPU processing domain from {input:?} to {output:?}")]
    NonPreservingColorDomain {
        /// Node containing the non-preserving operation.
        node_id: EffectGraphNodeId,
        /// Required input domain.
        input: EffectColorDomain,
        /// Produced output domain.
        output: EffectColorDomain,
    },
    /// One fused point pass cannot execute operations declared in different domains.
    #[error("effect graph mixes GPU processing domains {first:?} and {next:?}")]
    MixedProcessingDomains {
        /// First processing domain in the chain.
        first: EffectColorDomain,
        /// Conflicting later processing domain.
        next: EffectColorDomain,
    },
    /// Data or alpha domains crossed a color edge and cannot be converted.
    #[error("effect graph contains {blockers} invalid color-domain edges")]
    ColorDomainBlocked {
        /// Number of fail-closed domain blockers.
        blockers: usize,
    },
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
    if !compiled.domain_plan.blockers.is_empty() {
        return Err(EffectGpuPlanBlocker::ColorDomainBlocked {
            blockers: compiled.domain_plan.blockers.len(),
        });
    }
    let mut previous = None;
    let mut operations = Vec::new();
    let mut processing_domain = None;
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
                merge_processing_domain(&mut processing_domain, EffectColorDomain::SceneLinearRgb)?;
                operations.push(lower_point_op(*node_id, op)?);
                if operations.len() > MAX_FUSED_GPU_EFFECT_OPS {
                    return Err(EffectGpuPlanBlocker::TooManyOperations {
                        actual: operations.len(),
                        maximum: MAX_FUSED_GPU_EFFECT_OPS,
                    });
                }
            }
            EffectGraphNodeKind::DomainEffect { input, op, domain_contract } => {
                if Some(*input) != previous {
                    return Err(EffectGpuPlanBlocker::DisconnectedChain { node_id: *node_id });
                }
                if domain_contract.input != domain_contract.output {
                    return Err(EffectGpuPlanBlocker::NonPreservingColorDomain {
                        node_id: *node_id,
                        input: domain_contract.input,
                        output: domain_contract.output,
                    });
                }
                merge_processing_domain(&mut processing_domain, domain_contract.input)?;
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
    let processing_domain = processing_domain.unwrap_or(EffectColorDomain::SceneLinearRgb);
    let transitions_match = if processing_domain == EffectColorDomain::SceneLinearRgb {
        compiled.domain_plan.transitions.is_empty()
    } else {
        compiled.domain_plan.transitions.len() == 2
            && compiled.domain_plan.transitions.iter().any(|transition| {
                transition.from == EffectColorDomain::SceneLinearRgb
                    && transition.to == processing_domain
            })
            && compiled.domain_plan.transitions.iter().any(|transition| {
                transition.from == processing_domain
                    && transition.to == EffectColorDomain::SceneLinearRgb
            })
    };
    if !transitions_match {
        return Err(EffectGpuPlanBlocker::UnsupportedColorDomainPlan {
            transitions: compiled.domain_plan.transitions.len(),
        });
    }
    Ok(CompiledEffectGpuPlan {
        operations,
        graph_signature: compiled.signature_hash,
        processing_domain,
    })
}

fn merge_processing_domain(
    current: &mut Option<EffectColorDomain>,
    next: EffectColorDomain,
) -> Result<(), EffectGpuPlanBlocker> {
    match *current {
        Some(first) if first != next => {
            Err(EffectGpuPlanBlocker::MixedProcessingDomains { first, next })
        }
        Some(_) => Ok(()),
        None => {
            *current = Some(next);
            Ok(())
        }
    }
}

/// Return a shared cached GPU plan for a compiled effect graph.
///
/// Both successful plans and deterministic lowering blockers are cached by the
/// compiled graph signature, preventing repeated plan allocation and repeated
/// blocker traversal during playback.
pub fn get_or_lower_effect_graph_to_gpu_plan(
    compiled: &CompiledEffectGraph,
) -> Result<Arc<CompiledEffectGpuPlan>, EffectGpuPlanBlocker> {
    let signature = compiled.signature_hash;
    if let Ok(mut cache) = gpu_plan_cache().lock() {
        if let Some(result) = cache.get(signature) {
            return result;
        }
    }

    let lowered = lower_effect_graph_to_gpu_plan(compiled).map(Arc::new);
    if let Ok(mut cache) = gpu_plan_cache().lock() {
        cache.insert(signature, lowered.clone());
    }
    lowered
}

struct EffectGpuPlanCache {
    entries: HashMap<u64, Result<Arc<CompiledEffectGpuPlan>, EffectGpuPlanBlocker>>,
    lru: VecDeque<u64>,
}

impl EffectGpuPlanCache {
    fn new() -> Self {
        Self { entries: HashMap::new(), lru: VecDeque::new() }
    }

    fn get(
        &mut self,
        signature: u64,
    ) -> Option<Result<Arc<CompiledEffectGpuPlan>, EffectGpuPlanBlocker>> {
        let result = self.entries.get(&signature)?.clone();
        self.touch(signature);
        Some(result)
    }

    fn insert(
        &mut self,
        signature: u64,
        result: Result<Arc<CompiledEffectGpuPlan>, EffectGpuPlanBlocker>,
    ) {
        self.entries.insert(signature, result);
        self.touch(signature);
        while self.entries.len() > GPU_PLAN_CACHE_CAPACITY {
            if let Some(evicted) = self.lru.pop_front() {
                self.entries.remove(&evicted);
            }
        }
    }

    fn touch(&mut self, signature: u64) {
        if let Some(index) = self.lru.iter().position(|entry| *entry == signature) {
            self.lru.remove(index);
        }
        self.lru.push_back(signature);
    }
}

fn gpu_plan_cache() -> &'static Mutex<EffectGpuPlanCache> {
    static CACHE: OnceLock<Mutex<EffectGpuPlanCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(EffectGpuPlanCache::new()))
}

fn lower_point_op(
    node_id: EffectGraphNodeId,
    op: &EffectRenderOp,
) -> Result<EffectGpuPointOp, EffectGpuPlanBlocker> {
    match op {
        EffectRenderOp::ColorAdjust {
            exposure,
            contrast,
            saturation,
            working_color_space,
        } => Ok(EffectGpuPointOp::ColorAdjust {
            exposure: *exposure,
            contrast: *contrast,
            saturation: *saturation,
            luminance_coefficients: working_color_space.luminance_coefficients(),
        }),
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
        EffectGraphNodeKind::DomainEffect { .. } => "domain_effect",
        EffectGraphNodeKind::Blend { .. } => "blend",
        EffectGraphNodeKind::Mask { .. } => "mask",
        EffectGraphNodeKind::MaskSource { .. } => "mask_source",
        EffectGraphNodeKind::MultiInput { .. } => "multi_input",
    }
}

fn operation_name(op: &EffectRenderOp) -> &'static str {
    match op {
        EffectRenderOp::ColorAdjust { .. } => "color_adjust",
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
            working_color_space: mondrian_core::WorkingColorSpace::LinearRec709,
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
    fn lowers_preserving_display_domain_with_explicit_processing_identity() {
        let display_domain = crate::EffectColorDomain::DisplayEncodedRgb {
            color_space: mondrian_core::ColorSpace::Rec709,
        };
        let compiled = crate::compile_scheduled_effect_graph_in_domain(
            &crate::EffectRenderPlan {
                ops: vec![EffectRenderOp::ColorAdjust {
                    exposure: 0.25,
                    contrast: 1.0,
                    saturation: 1.0,
                    working_color_space: mondrian_core::WorkingColorSpace::LinearRec709,
                }],
            },
            crate::EffectColorDomainContract::preserving(display_domain),
        )
        .expect("valid display-domain graph");

        let plan = lower_effect_graph_to_gpu_plan(&compiled)
            .expect("renderer can schedule preserving RGB domain transitions");

        assert_eq!(plan.processing_domain(), display_domain);
        assert_eq!(plan.operations().len(), 1);
    }

    #[test]
    fn rejects_non_preserving_gpu_processing_domain() {
        let input = crate::EffectColorDomain::DisplayEncodedRgb {
            color_space: mondrian_core::ColorSpace::Rec709,
        };
        let output = crate::EffectColorDomain::DisplayLinearRgb {
            color_space: mondrian_core::ColorSpace::LinearRec709,
        };
        let compiled = crate::compile_scheduled_effect_graph_in_domain(
            &crate::EffectRenderPlan {
                ops: vec![EffectRenderOp::ColorAdjust {
                    exposure: 0.25,
                    contrast: 1.0,
                    saturation: 1.0,
                    working_color_space: mondrian_core::WorkingColorSpace::LinearRec709,
                }],
            },
            crate::EffectColorDomainContract { input, output },
        )
        .expect("valid domain graph");

        assert!(matches!(
            lower_effect_graph_to_gpu_plan(&compiled),
            Err(EffectGpuPlanBlocker::NonPreservingColorDomain {
                input: actual_input,
                output: actual_output,
                ..
            }) if actual_input == input && actual_output == output
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

    #[test]
    fn cached_lowering_reuses_shared_plan() {
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(EffectRenderOp::Grain { amount: 0.2 });
        let compiled =
            get_or_compile_scheduled_render_graph(builder.finish()).expect("valid graph");

        let first = get_or_lower_effect_graph_to_gpu_plan(&compiled).expect("supported graph");
        let second = get_or_lower_effect_graph_to_gpu_plan(&compiled).expect("cached graph");

        assert!(Arc::ptr_eq(&first, &second));
    }
}
