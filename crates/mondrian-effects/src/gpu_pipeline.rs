//! Ordered GPU execution pipeline for a compiled effect graph.
//!
//! The fused point path ([`crate::gpu_plan::CompiledEffectGpuPlan`]) can only
//! run kernels that read a single pixel. Spatial kernels read a neighbourhood,
//! so they cannot share that fragment pass. This module lowers a graph into an
//! ordered list of stages instead:
//!
//! * adjacent point kernels become one [`EffectGpuStage::PointOps`] stage (the
//!   renderer still executes it as one fused pass, up to
//!   [`MAX_FUSED_GPU_EFFECT_OPS`]),
//! * a spatial kernel becomes an [`EffectGpuStage::Spatial`] stage that the
//!   renderer runs as its own multi-pass compute job, and
//! * kernels after the spatial stage start a fresh point stage.
//!
//! Graph order is therefore preserved exactly, and two spatial kernels with
//! point kernels in between still fuse into the minimum number of passes for
//! the given graph. The plan never reorders stages: correctness first, fusing
//! second.

use std::sync::Arc;

use crate::effect::{EffectColorDomain, EffectRenderOp};
use crate::execution_contract::{EffectProcessingBackend, EffectWorkingPrecision};
use crate::gpu_plan::{
    lower_point_op, merge_processing_domain, node_kind_name, EffectGpuPlanBlocker,
    EffectGpuPointOp, MAX_FUSED_GPU_EFFECT_OPS,
};
use crate::graph::{CompiledEffectGraph, EffectGraphNodeId, EffectGraphNodeKind};
use crate::{BoxBlurDirection, EffectExecutionModes};

/// One spatial kernel the renderer can execute as a multi-pass compute job.
///
/// The variants carry exactly the CPU kernel's authored parameters, so the GPU
/// and CPU paths evaluate the same math for the same graph.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EffectGpuSpatialPass {
    /// Separable box blur; `iterations` passes per blurred axis.
    BoxBlur {
        /// Fractional blur radius in pixels.
        radius: f32,
        /// Passes applied along each blurred axis.
        iterations: u32,
        /// Which axes are blurred.
        direction: BoxBlurDirection,
        /// Whether out-of-frame samples repeat the edge pixel.
        repeat_edge_pixels: bool,
    },
    /// Directional (linear motion) blur.
    DirectionalBlur {
        /// Length of the blur streak in pixels.
        length: f32,
        /// Compass direction of the streak in degrees.
        direction_degrees: f32,
    },
    /// Edge magnitude from a Sobel operator.
    FindEdges {
        /// Authored edge parameters.
        grade: crate::FindEdgesGrade,
    },
    /// Directional luminance relief.
    Emboss {
        /// Authored relief parameters.
        grade: crate::EmbossGrade,
    },
    /// Per-channel directional relief.
    ColorEmboss {
        /// Authored relief parameters.
        grade: crate::ColorEmbossGrade,
    },
    /// Spin or zoom radial blur.
    RadialBlur {
        /// Authored radial blur parameters.
        grade: crate::RadialBlurGrade,
    },
    /// Wrapping translation.
    Offset {
        /// Authored offset parameters.
        grade: crate::OffsetGrade,
    },
    /// Radial twist around a centre.
    Twirl {
        /// Authored twirl parameters.
        grade: crate::TwirlGrade,
    },
    /// Spherical lens remap.
    Spherize {
        /// Authored spherize parameters.
        grade: crate::SpherizeGrade,
    },
    /// Arbitrary three by three convolution.
    Convolution {
        /// Authored convolution parameters.
        grade: crate::ConvolutionGrade,
    },
    /// Angular mirror reflection.
    Mirror {
        /// Authored mirror parameters.
        grade: crate::MirrorGrade,
    },
    /// Cartesian to polar conversion and its inverse.
    PolarCoordinates {
        /// Authored conversion parameters.
        grade: crate::PolarCoordinatesGrade,
    },
    /// Camera lens flare added over the source frame.
    LensFlare {
        /// Authored flare parameters.
        grade: crate::LensFlareGrade,
    },
}

impl EffectGpuSpatialPass {
    /// Stable name used by diagnostics and renderer dispatch.
    pub const fn name(self) -> &'static str {
        match self {
            Self::BoxBlur { .. } => "box_blur",
            Self::DirectionalBlur { .. } => "directional_blur",
            Self::FindEdges { .. } => "find_edges",
            Self::Emboss { .. } => "emboss",
            Self::ColorEmboss { .. } => "color_emboss",
            Self::RadialBlur { .. } => "radial_blur",
            Self::Offset { .. } => "offset",
            Self::Twirl { .. } => "twirl",
            Self::Spherize { .. } => "spherize",
            Self::Convolution { .. } => "convolution",
            Self::Mirror { .. } => "mirror",
            Self::PolarCoordinates { .. } => "polar_coordinates",
            Self::LensFlare { .. } => "lens_flare",
        }
    }

    /// Horizontal reach of the kernel in source pixels.
    ///
    /// The renderer sizes its intermediate rasters from this: a spatial stage
    /// reads nothing beyond the halo, so a full-frame layer raster is always
    /// sufficient.
    pub fn horizontal_halo(self) -> u32 {
        match self {
            Self::BoxBlur { radius, iterations, direction, .. } => {
                crate::box_blur::box_blur_axis_halo(radius, iterations, direction)
                    .map(|(horizontal, _)| horizontal)
                    .unwrap_or(u32::MAX)
            }
            Self::DirectionalBlur { length, .. } => {
                crate::directional_blur::directional_blur_axis_halo(length)
                    .map(|(horizontal, _)| horizontal)
                    .unwrap_or(u32::MAX)
            }
            Self::FindEdges { .. } => crate::find_edges::FIND_EDGES_HALO_PIXELS,
            Self::Emboss { .. } => crate::emboss::EMBOSS_HALO_PIXELS,
            Self::ColorEmboss { .. } => crate::color_emboss::COLOR_EMBOSS_HALO_PIXELS,
            Self::Convolution { .. } => crate::convolution::CONVOLUTION_HALO_PIXELS,
            // Remapping kernels may read any pixel of the frame, so no local
            // halo can bound them; a full-frame raster is always required.
            Self::RadialBlur { .. }
            | Self::Offset { .. }
            | Self::Twirl { .. }
            | Self::Spherize { .. }
            | Self::Mirror { .. }
            | Self::PolarCoordinates { .. } => u32::MAX,
            // A flare reads only the pixel it writes, so it needs no neighbour.
            Self::LensFlare { .. } => 0,
        }
    }

    /// Vertical reach of the kernel in source pixels.
    pub fn vertical_halo(self) -> u32 {
        match self {
            Self::BoxBlur { radius, iterations, direction, .. } => {
                crate::box_blur::box_blur_axis_halo(radius, iterations, direction)
                    .map(|(_, vertical)| vertical)
                    .unwrap_or(u32::MAX)
            }
            Self::DirectionalBlur { length, .. } => {
                crate::directional_blur::directional_blur_axis_halo(length)
                    .map(|(_, vertical)| vertical)
                    .unwrap_or(u32::MAX)
            }
            Self::FindEdges { .. } => crate::find_edges::FIND_EDGES_HALO_PIXELS,
            Self::Emboss { .. } => crate::emboss::EMBOSS_HALO_PIXELS,
            Self::ColorEmboss { .. } => crate::color_emboss::COLOR_EMBOSS_HALO_PIXELS,
            Self::Convolution { .. } => crate::convolution::CONVOLUTION_HALO_PIXELS,
            Self::RadialBlur { .. }
            | Self::Offset { .. }
            | Self::Twirl { .. }
            | Self::Spherize { .. }
            | Self::Mirror { .. }
            | Self::PolarCoordinates { .. } => u32::MAX,
            Self::LensFlare { .. } => 0,
        }
    }
}

/// One stage of the GPU pipeline, in graph order.
#[derive(Debug, Clone, PartialEq)]
pub enum EffectGpuStage {
    /// Point kernels fused into one fragment pass.
    PointOps {
        /// Operations applied in order to each source pixel.
        operations: Vec<EffectGpuPointOp>,
        /// Graph nodes represented by this stage, in execution order.
        node_ids: Arc<[EffectGraphNodeId]>,
    },
    /// One spatial kernel executed as its own multi-pass compute job.
    Spatial {
        /// Kernel and its authored parameters.
        pass: EffectGpuSpatialPass,
        /// Graph node that produced this pass.
        node_id: EffectGraphNodeId,
    },
}

impl EffectGpuStage {
    /// Number of point operations fused into this stage (zero for spatial).
    pub fn point_operation_count(&self) -> usize {
        match self {
            Self::PointOps { operations, .. } => operations.len(),
            Self::Spatial { .. } => 0,
        }
    }

    /// Whether the stage is a spatial kernel.
    pub const fn is_spatial(&self) -> bool {
        matches!(self, Self::Spatial { .. })
    }
}

/// Immutable renderer-facing stages for one compiled effect graph.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledEffectGpuPipeline {
    stages: Vec<EffectGpuStage>,
    graph_signature: u64,
    graph_fingerprint: [u8; 32],
    source_value: EffectGraphNodeId,
    output_value: EffectGraphNodeId,
    processing_domain: EffectColorDomain,
}

impl CompiledEffectGpuPipeline {
    /// Stages in execution order.
    pub fn stages(&self) -> &[EffectGpuStage] {
        &self.stages
    }

    /// Signature of the compiled source graph represented by this pipeline.
    pub const fn graph_signature(&self) -> u64 {
        self.graph_signature
    }

    /// Complete pixel-semantic graph fingerprint represented by this pipeline.
    pub const fn graph_fingerprint(&self) -> [u8; 32] {
        self.graph_fingerprint
    }

    /// Existing graph value consumed by the first stage.
    pub const fn source_value(&self) -> EffectGraphNodeId {
        self.source_value
    }

    /// Graph value produced by the final stage.
    pub const fn output_value(&self) -> EffectGraphNodeId {
        self.output_value
    }

    /// Exact color domain every stage executes in.
    pub fn processing_domain(&self) -> EffectColorDomain {
        self.processing_domain
    }

    /// Whether the pipeline performs no pixel changes.
    pub fn is_identity(&self) -> bool {
        self.stages.is_empty()
    }

    /// Number of spatial compute jobs the renderer must run.
    pub fn spatial_stage_count(&self) -> usize {
        self.stages.iter().filter(|stage| stage.is_spatial()).count()
    }

    /// Total point operations across every fused stage.
    pub fn point_operation_count(&self) -> usize {
        self.stages.iter().map(EffectGpuStage::point_operation_count).sum()
    }
}

/// Lower one compiled graph into ordered GPU stages.
///
/// Fails closed with the same blockers as the fused point path: a graph that
/// cannot be represented as one linear GPU chain (branching, masks, external
/// processors, non-preserving domains) is rejected instead of being
/// approximated.
pub fn lower_effect_graph_to_gpu_pipeline(
    compiled: &CompiledEffectGraph,
) -> Result<CompiledEffectGpuPipeline, EffectGpuPlanBlocker> {
    let output = compiled.graph().output.ok_or(EffectGpuPlanBlocker::MissingOutput)?;
    if !compiled.domain_plan().blockers.is_empty() {
        return Err(EffectGpuPlanBlocker::ColorDomainBlocked {
            blockers: compiled.domain_plan().blockers.len(),
        });
    }
    if !compiled.domain_plan().transitions.is_empty() {
        return Err(EffectGpuPlanBlocker::UnsupportedColorDomainPlan {
            transitions: compiled.domain_plan().transitions.len(),
        });
    }

    let scheduled = compiled
        .schedule()
        .ordered_nodes
        .iter()
        .copied()
        .filter(|node_id| {
            !matches!(
                compiled.graph().node(*node_id).map(|node| &node.kind),
                Some(EffectGraphNodeKind::Source)
            )
        })
        .collect::<Vec<_>>();

    let mut stages = Vec::new();
    let mut pending_operations = Vec::new();
    let mut pending_ids = Vec::new();
    let mut source_value = output;
    let mut previous = None;
    let mut processing_domain = None;

    for (index, node_id) in scheduled.iter().copied().enumerate() {
        let Some(node) = compiled.graph().node(node_id) else {
            return Err(EffectGpuPlanBlocker::DisconnectedChain { node_id });
        };
        let (input, op, domain) = match &node.kind {
            EffectGraphNodeKind::UnaryEffect { input, op } => {
                (*input, op, EffectColorDomain::SceneLinearRgb)
            }
            EffectGraphNodeKind::DomainEffect { input, op, domain_contract } => {
                if domain_contract.input != domain_contract.output {
                    return Err(EffectGpuPlanBlocker::NonPreservingColorDomain {
                        node_id,
                        input: domain_contract.input,
                        output: domain_contract.output,
                    });
                }
                (*input, op, domain_contract.input)
            }
            kind => {
                return Err(EffectGpuPlanBlocker::UnsupportedTopology {
                    node_id,
                    kind: node_kind_name(kind),
                });
            }
        };
        if index == 0 {
            source_value = input;
            previous = Some(input);
        }
        if Some(input) != previous {
            return Err(EffectGpuPlanBlocker::DisconnectedChain { node_id });
        }
        merge_processing_domain(&mut processing_domain, domain)?;

        match spatial_pass_for(op) {
            Some(pass) => {
                flush_point_stage(&mut stages, &mut pending_operations, &mut pending_ids);
                stages.push(EffectGpuStage::Spatial { pass, node_id });
            }
            None => {
                let admitted =
                    compiled.node_execution_modes(node_id).unwrap_or(EffectExecutionModes::NONE);
                if !admitted.contains(
                    EffectProcessingBackend::Gpu,
                    EffectWorkingPrecision::Float32,
                ) {
                    return Err(EffectGpuPlanBlocker::NodeExecutionModeNotAdmitted {
                        node_id,
                        admitted,
                    });
                }
                pending_operations.push(lower_point_op(node_id, op)?);
                pending_ids.push(node_id);
                if pending_operations.len() >= MAX_FUSED_GPU_EFFECT_OPS {
                    flush_point_stage(&mut stages, &mut pending_operations, &mut pending_ids);
                }
            }
        }
        previous = Some(node_id);
    }
    flush_point_stage(&mut stages, &mut pending_operations, &mut pending_ids);

    Ok(CompiledEffectGpuPipeline {
        stages,
        graph_signature: compiled.signature_hash(),
        graph_fingerprint: compiled.semantic_fingerprint(),
        source_value,
        output_value: output,
        processing_domain: processing_domain.unwrap_or(EffectColorDomain::SceneLinearRgb),
    })
}

fn flush_point_stage(
    stages: &mut Vec<EffectGpuStage>,
    operations: &mut Vec<EffectGpuPointOp>,
    node_ids: &mut Vec<EffectGraphNodeId>,
) {
    if operations.is_empty() {
        return;
    }
    stages.push(EffectGpuStage::PointOps {
        operations: std::mem::take(operations),
        node_ids: Arc::from(std::mem::take(node_ids)),
    });
}

/// The spatial pass for one render operation, if the renderer can execute it.
///
/// Deliberately conservative: a kernel is only listed here once the GPU math is
/// defined for it. Everything else keeps travelling through the CPU path.
pub fn spatial_pass_for(op: &EffectRenderOp) -> Option<EffectGpuSpatialPass> {
    match op {
        EffectRenderOp::BoxBlur { radius, iterations, direction, repeat_edge_pixels } => {
            Some(EffectGpuSpatialPass::BoxBlur {
                radius: *radius,
                iterations: *iterations,
                direction: *direction,
                repeat_edge_pixels: *repeat_edge_pixels,
            })
        }
        EffectRenderOp::DirectionalBlur { length, direction_degrees } => {
            Some(EffectGpuSpatialPass::DirectionalBlur {
                length: *length,
                direction_degrees: *direction_degrees,
            })
        }
        EffectRenderOp::FindEdges { grade } => {
            Some(EffectGpuSpatialPass::FindEdges { grade: *grade })
        }
        EffectRenderOp::Emboss { grade } => Some(EffectGpuSpatialPass::Emboss { grade: *grade }),
        EffectRenderOp::ColorEmboss { grade } => {
            Some(EffectGpuSpatialPass::ColorEmboss { grade: *grade })
        }
        EffectRenderOp::RadialBlur { grade } => {
            Some(EffectGpuSpatialPass::RadialBlur { grade: *grade })
        }
        EffectRenderOp::Offset { grade } => Some(EffectGpuSpatialPass::Offset { grade: *grade }),
        EffectRenderOp::Twirl { grade } => Some(EffectGpuSpatialPass::Twirl { grade: *grade }),
        EffectRenderOp::Spherize { grade } => {
            Some(EffectGpuSpatialPass::Spherize { grade: *grade })
        }
        EffectRenderOp::Convolution { grade } => {
            Some(EffectGpuSpatialPass::Convolution { grade: *grade })
        }
        EffectRenderOp::Mirror { grade } => Some(EffectGpuSpatialPass::Mirror { grade: *grade }),
        EffectRenderOp::PolarCoordinates { grade } => {
            Some(EffectGpuSpatialPass::PolarCoordinates { grade: *grade })
        }
        EffectRenderOp::LensFlare { grade } => {
            Some(EffectGpuSpatialPass::LensFlare { grade: *grade })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{compile_reference_render_graph, EffectGraphBuilderState};
    use crate::InvertChannels;

    fn invert() -> EffectRenderOp {
        EffectRenderOp::Invert {
            channels: InvertChannels::Rgb,
            blend_with_original: 0.0,
        }
    }

    fn compile(builder: EffectGraphBuilderState) -> CompiledEffectGraph {
        let compiled =
            compile_reference_render_graph(builder.finish()).expect("valid reference graph");
        Arc::unwrap_or_clone(compiled)
    }

    #[test]
    fn point_only_graph_lowers_to_one_fused_stage() {
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(EffectRenderOp::Vignette { intensity: 0.5, feather: 0.4 });
        builder.append_unary(invert());
        let pipeline =
            lower_effect_graph_to_gpu_pipeline(&compile(builder)).expect("point-only pipeline");

        assert_eq!(pipeline.stages().len(), 1);
        assert_eq!(pipeline.point_operation_count(), 2);
        assert_eq!(pipeline.spatial_stage_count(), 0);
        assert_eq!(
            pipeline.processing_domain(),
            EffectColorDomain::SceneLinearRgb
        );
    }

    #[test]
    fn spatial_pass_splits_the_pipeline_and_keeps_graph_order() {
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(invert());
        builder.append_unary(EffectRenderOp::BoxBlur {
            radius: 4.0,
            iterations: 2,
            direction: BoxBlurDirection::Horizontal,
            repeat_edge_pixels: false,
        });
        builder.append_unary(invert());
        let graph = compile(builder);
        let pipeline = lower_effect_graph_to_gpu_pipeline(&graph).expect("spatial pipeline");

        let stages = pipeline.stages();
        assert_eq!(stages.len(), 3);
        assert!(matches!(stages[0], EffectGpuStage::PointOps { .. }));
        assert_eq!(
            stages[1],
            EffectGpuStage::Spatial {
                pass: EffectGpuSpatialPass::BoxBlur {
                    radius: 4.0,
                    iterations: 2,
                    direction: BoxBlurDirection::Horizontal,
                    repeat_edge_pixels: false,
                },
                node_id: graph.schedule().ordered_nodes[2],
            }
        );
        assert!(matches!(stages[2], EffectGpuStage::PointOps { .. }));
        assert_eq!(pipeline.spatial_stage_count(), 1);
        assert_eq!(pipeline.point_operation_count(), 2);
    }

    #[test]
    fn directional_blur_lowers_to_one_spatial_stage() {
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(EffectRenderOp::DirectionalBlur {
            length: 12.5,
            direction_degrees: 135.0,
        });
        let pipeline =
            lower_effect_graph_to_gpu_pipeline(&compile(builder)).expect("directional pipeline");

        assert_eq!(pipeline.stages().len(), 1);
        assert_eq!(
            pipeline.stages()[0],
            EffectGpuStage::Spatial {
                pass: EffectGpuSpatialPass::DirectionalBlur {
                    length: 12.5,
                    direction_degrees: 135.0,
                },
                node_id: pipeline.output_value(),
            }
        );
    }

    #[test]
    fn long_point_runs_open_additional_fused_stages() {
        let mut builder = EffectGraphBuilderState::new();
        for _ in 0..(MAX_FUSED_GPU_EFFECT_OPS + 1) {
            builder.append_unary(invert());
        }
        let pipeline =
            lower_effect_graph_to_gpu_pipeline(&compile(builder)).expect("long point pipeline");

        assert_eq!(pipeline.stages().len(), 2);
        assert_eq!(
            pipeline.stages()[0].point_operation_count(),
            MAX_FUSED_GPU_EFFECT_OPS
        );
        assert_eq!(pipeline.stages()[1].point_operation_count(), 1);
    }

    #[test]
    fn kernels_without_gpu_math_still_fail_closed() {
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(EffectRenderOp::GaussianBlur { radius: 4.0 });
        let error = lower_effect_graph_to_gpu_pipeline(&compile(builder))
            .expect_err("gaussian blur has no GPU math yet");

        // Gaussian blur is still declared CPU-only, so the pipeline rejects it
        // at admission. Once its contract grows a GPU mode this becomes an
        // `UnsupportedOperation` until a compute pass exists for it.
        assert!(
            matches!(
                error,
                EffectGpuPlanBlocker::NodeExecutionModeNotAdmitted { .. }
                    | EffectGpuPlanBlocker::UnsupportedOperation { op: "gaussian_blur", .. }
            ),
            "unexpected blocker: {error:?}"
        );
    }

    #[test]
    fn remapping_kernels_lower_to_their_own_spatial_stages() {
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(invert());
        builder.append_unary(EffectRenderOp::Twirl {
            grade: crate::TwirlGrade::new(180.0, 50.0, [8.0, 6.0]).expect("valid twirl"),
        });
        builder.append_unary(EffectRenderOp::PolarCoordinates {
            grade: crate::PolarCoordinatesGrade::new(
                crate::PolarConversion::RectToPolar,
                crate::PolarInterpolation::Bilinear,
            ),
        });
        let pipeline =
            lower_effect_graph_to_gpu_pipeline(&compile(builder)).expect("remap pipeline");

        assert_eq!(pipeline.stages().len(), 3);
        assert_eq!(pipeline.spatial_stage_count(), 2);
        let names: Vec<&'static str> = pipeline
            .stages()
            .iter()
            .filter_map(|stage| match stage {
                EffectGpuStage::Spatial { pass, .. } => Some(pass.name()),
                EffectGpuStage::PointOps { .. } => None,
            })
            .collect();
        assert_eq!(names, ["twirl", "polar_coordinates"]);
        for stage in pipeline.stages().iter().filter(|stage| stage.is_spatial()) {
            let EffectGpuStage::Spatial { pass, .. } = stage else {
                unreachable!("filtered to spatial stages");
            };
            // Remapping kernels read anywhere in the frame, so no local halo
            // can describe them.
            assert_eq!(pass.horizontal_halo(), u32::MAX);
            assert_eq!(pass.vertical_halo(), u32::MAX);
        }
    }
    #[test]
    fn spatial_halo_reports_the_kernel_reach() {
        let pass = EffectGpuSpatialPass::BoxBlur {
            radius: 4.0,
            iterations: 2,
            direction: BoxBlurDirection::Horizontal,
            repeat_edge_pixels: true,
        };
        assert_eq!(pass.horizontal_halo(), 8);
        assert_eq!(pass.vertical_halo(), 0);

        let pass = EffectGpuSpatialPass::DirectionalBlur { length: 10.0, direction_degrees: 90.0 };
        assert_eq!(pass.name(), "directional_blur");
        assert!(pass.vertical_halo() >= 5);
    }
}
