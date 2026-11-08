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
    /// Lens-shaped magnification around a centre.
    Magnify {
        /// Authored magnify parameters.
        grade: crate::MagnifyGrade,
    },
    /// Circular dent or bulge remap.
    Dent {
        /// Authored dent parameters.
        grade: crate::DentGrade,
    },
    /// Arc bend with separable along-axis and across-axis distortion.
    Warp {
        /// Authored warp parameters.
        grade: crate::WarpGrade,
    },
    /// Checkerboard cell-corner displacement.
    GridWarp {
        /// Authored grid warp parameters.
        grade: crate::GridWarpGrade,
    },
    /// Coons-patch bezier remap.
    BezierWarp {
        /// Authored bezier warp control points and quality.
        grade: crate::BezierWarpGrade,
    },
    /// Virtual-reality equirectangular sphere rotation.
    VrRotateSphere {
        /// Authored tilt, pan, roll and projection controls.
        grade: crate::VrRotateSphereGrade,
    },
    /// Virtual-reality equirectangular sphere to perspective plane.
    VrSphereToPlane {
        /// Authored field of view, projection tilt and direction switch.
        grade: crate::VrSphereToPlaneGrade,
    },
    /// Virtual-reality flat card projected onto an equirectangular sphere.
    VrProject2D {
        /// Authored card scale, poses, disparity and feather.
        grade: crate::VrProject2DGrade,
    },
    /// Parallel displacement clone across an authored line.
    WireRemoval {
        /// Authored wire removal line and controls.
        grade: crate::WireRemovalGrade,
    },
    /// Fractal growth of the opaque silhouette.
    RoughenEdges {
        /// Authored roughen edges parameters.
        grade: crate::RoughenEdgesGrade,
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
    Bulge {
        /// The authored bulge controls.
        grade: crate::BulgeGrade,
    },
    Split {
        /// The authored split controls.
        grade: crate::SplitGrade,
    },
    LinearColorKey {
        /// The authored key controls.
        grade: crate::LinearColorKeyGrade,
    },
    LensDistortion {
        /// Resolved authoring state for this stage.
        grade: crate::LensDistortionGrade,
    },
    Flip {
        /// Authored flip axis, mirror centre, and blend weight.
        grade: crate::FlipGrade,
    },
    Ripple {
        /// Authored centre, radius, wave shape, and folded phase.
        grade: crate::RippleGrade,
    },
    WaveWarp {
        /// Authored wave shape, geometry, pin, quality, and folded phase.
        grade: crate::WaveWarpGrade,
    },
    Transform {
        /// Authored anchor, position, scale, skew, rotation, and sampling.
        grade: crate::TransformGrade,
    },
    CornerPin {
        /// Authored destination quad in normalized frame coordinates.
        grade: crate::CornerPinGrade,
    },
    TurbulentDisplace {
        /// Authored fractal-noise displacement parameters.
        grade: crate::TurbulentDisplaceGrade,
    },
    Deband {
        /// Authored edge threshold, blur radius, and dither amplitude.
        grade: crate::DebandGrade,
    },
    Glow {
        /// Authored threshold, blur radius, blend operation, and colour pair.
        grade: crate::GlowGrade,
    },
    FractalNoise {
        /// Authored fractal shape, noise interpolation, domain, and compositing.
        grade: crate::FractalNoiseGrade,
    },
    Keylight {
        /// Authored screen colour, matte remap, despill, and matte morphology.
        grade: crate::KeylightGrade,
    },
    MatteRefine {
        /// Authored matte cleanup, smoothing, morphology, feather, and views.
        grade: crate::MatteRefineGrade,
    },
    Choker {
        /// Authored alpha erosion (positive) or dilation (negative) radius.
        grade: crate::ChokerGrade,
    },
    Minimax {
        /// Authored radius, operation, channel selection, and direction.
        grade: crate::MinimaxGrade,
    },
    LensFlare {
        /// Authored flare parameters.
        grade: crate::LensFlareGrade,
    },
    ColorOffset {
        /// Authored channel phases and overflow mode.
        grade: crate::ColorOffsetGrade,
    },
    ChannelBlur {
        /// Authored per-channel amounts, edge behaviour, and direction.
        grade: crate::ChannelBlurGrade,
    },
    Median {
        /// Authored window radius and the alpha toggle.
        grade: crate::MedianGrade,
    },
    SelectiveColor {
        /// Authored colour range, method, and the folded CMYK delta table.
        grade: crate::SelectiveColorGrade,
    },
    Stroke {
        /// Authored paint style, ink, brush profile, and opacity.
        grade: crate::StrokeGrade,
    },
    DustScratches {
        /// Authored window radius, repair threshold and the alpha toggle.
        grade: crate::DustScratchesGrade,
    },
    KeyCleaner {
        /// Authored edge radius, chatter fraction, alpha contrast and strength.
        grade: crate::KeyCleanerGrade,
    },
    SmartBlur {
        /// Authored radius, threshold and the output mode.
        grade: crate::SmartBlurGrade,
    },
    /// Radial correction around the green reference channel.
    ChromaticAberrationRemoval {
        /// Authored amounts, edge scale, and normalized centre.
        grade: crate::ChromaticAberrationRemovalGrade,
    },
    /// One perspective remap with optional specular lighting.
    Basic3D {
        /// Validated camera and lighting controls.
        grade: crate::Basic3DGrade,
    },
    /// Fill composited through an independently sampled alpha matte.
    FillMatte {
        /// Authored feather, inversion, RGB and opacity.
        grade: crate::FillGrade,
        /// Exact reconstruction and boundary behavior of the second edge.
        sampling: crate::AuxiliarySampling,
        /// Host slot for a linear chain. A graph dispatcher supplies its own value.
        external_slot: Option<u32>,
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
            Self::Magnify { .. } => "magnify",
            Self::Dent { .. } => "dent",
            Self::Warp { .. } => "warp",
            Self::GridWarp { .. } => "grid_warp",
            Self::BezierWarp { .. } => "bezier_warp",
            Self::VrRotateSphere { .. } => "vr_rotate_sphere",
            Self::VrSphereToPlane { .. } => "vr_sphere_to_plane",
            Self::VrProject2D { .. } => "vr_project_2d",
            Self::WireRemoval { .. } => "wire_removal",
            Self::RoughenEdges { .. } => "roughen_edges",
            Self::Convolution { .. } => "convolution",
            Self::Mirror { .. } => "mirror",
            Self::PolarCoordinates { .. } => "polar_coordinates",
            Self::LensDistortion { .. } => "lens_distortion",
            Self::Flip { .. } => "flip",
            Self::Ripple { .. } => "ripple",
            Self::WaveWarp { .. } => "wave_warp",
            Self::Transform { .. } => "transform",
            Self::CornerPin { .. } => "corner_pin",
            Self::TurbulentDisplace { .. } => "turbulent_displace",
            Self::Deband { .. } => "deband",
            Self::Glow { .. } => "glow",
            Self::FractalNoise { .. } => "fractal_noise",
            Self::Keylight { .. } => "keylight",
            Self::MatteRefine { .. } => "matte_refine",
            Self::Choker { .. } => "choker",
            Self::Minimax { .. } => "minimax",
            Self::LensFlare { .. } => "lens_flare",
            Self::Bulge { .. } => "bulge",
            Self::Split { .. } => "split",
            Self::LinearColorKey { .. } => "linear_color_key",
            Self::ColorOffset { .. } => "color_offset",
            Self::ChannelBlur { .. } => "channel_blur",
            Self::Median { .. } => "median",
            Self::SelectiveColor { .. } => "selective_color",
            Self::Stroke { .. } => "stroke",
            Self::DustScratches { .. } => "dust_scratches",
            Self::KeyCleaner { .. } => "key_cleaner",
            Self::SmartBlur { .. } => "smart_blur",
            Self::ChromaticAberrationRemoval { .. } => "chromatic_aberration_removal",
            Self::Basic3D { .. } => "basic_3d",
            Self::FillMatte { .. } => "fill_matte",
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
            | Self::Dent { .. }
            | Self::Warp { .. }
            | Self::GridWarp { .. }
            | Self::BezierWarp { .. }
            | Self::VrRotateSphere { .. }
            | Self::VrSphereToPlane { .. }
            | Self::VrProject2D { .. }
            | Self::WireRemoval { .. }
            | Self::Mirror { .. }
            | Self::PolarCoordinates { .. } => u32::MAX,
            // The lens contracts the inverse map, so unlike the group above its
            // reach is the authored radius plus the bilinear tap.
            Self::Magnify { grade } => magnify_halo(grade),
            // A flare reads only the pixel it writes, so it needs no neighbour.
            Self::LensDistortion { .. } => u32::MAX,
            Self::Flip { .. } => u32::MAX,
            Self::Ripple { .. } => u32::MAX,
            Self::WaveWarp { .. } => u32::MAX,
            Self::Transform { .. } => u32::MAX,
            Self::CornerPin { .. } => u32::MAX,
            Self::TurbulentDisplace { .. } => u32::MAX,
            Self::Deband { .. } => u32::MAX,
            Self::Glow { .. } => u32::MAX,
            Self::FractalNoise { .. } => u32::MAX,
            Self::Keylight { .. } => u32::MAX,
            Self::MatteRefine { .. } => u32::MAX,
            Self::Choker { .. } => u32::MAX,
            Self::Minimax { .. } => u32::MAX,
            Self::LensFlare { .. } => 0,
            Self::Bulge { .. } => u32::MAX,
            Self::Split { .. } => u32::MAX,
            Self::LinearColorKey { .. } => 0,
            Self::ColorOffset { grade } => grade.halo_pixels(),
            // The sample can move at most the border away from the pixel.
            Self::RoughenEdges { grade } => grade.halo_pixels(),
            // The blur window can be as wide as the frame.
            Self::ChannelBlur { .. } => u32::MAX,
            // The window reaches at most the largest authored radius, which the
            // GPU gate already caps at three pixels.
            Self::Median { .. } => crate::median::MEDIAN_MAX_RADIUS,
            // The kernel is a per-pixel point that reads no neighbour.
            Self::SelectiveColor { .. } => 0,
            // The band reaches at most half the brush plus the two-pixel
            // sampling overshoot; the GPU gate caps the brush at 200.
            Self::Stroke { grade } => stroke_halo(grade),
            // The window reaches at most the largest authored radius. This is
            // the "may influence" reach, not the GPU admission cap: a radius
            // past three still widens the halo before the pass is refused.
            Self::DustScratches { .. } => crate::dust_scratches::DUST_SCRATCHES_MAX_RADIUS,
            // The erosion window can reach the frame boundary in either axis,
            // and the kernel is clipped rather than tiled, so the reach is
            // unbounded in both directions.
            Self::KeyCleaner { .. } => u32::MAX,
            // The blur window is the authored radius plus the one fractional
            // tap, so it is bounded by the authoring ceiling of 100.
            Self::SmartBlur { grade } => smart_blur_halo(grade),
            Self::ChromaticAberrationRemoval { grade } => chromatic_aberration_removal_halo(grade),
            Self::Basic3D { .. } => u32::MAX,
            Self::FillMatte { .. } => 0,
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
            | Self::Dent { .. }
            | Self::Warp { .. }
            | Self::GridWarp { .. }
            | Self::BezierWarp { .. }
            | Self::VrRotateSphere { .. }
            | Self::VrSphereToPlane { .. }
            | Self::VrProject2D { .. }
            | Self::WireRemoval { .. }
            | Self::Mirror { .. }
            | Self::PolarCoordinates { .. } => u32::MAX,
            // Same reach as the horizontal halo.
            Self::Magnify { grade } => magnify_halo(grade),
            Self::LensDistortion { .. } => u32::MAX,
            Self::Flip { .. } => u32::MAX,
            Self::Ripple { .. } => u32::MAX,
            Self::WaveWarp { .. } => u32::MAX,
            Self::Transform { .. } => u32::MAX,
            Self::CornerPin { .. } => u32::MAX,
            Self::TurbulentDisplace { .. } => u32::MAX,
            Self::Deband { .. } => u32::MAX,
            Self::Glow { .. } => u32::MAX,
            Self::FractalNoise { .. } => u32::MAX,
            Self::Keylight { .. } => u32::MAX,
            Self::MatteRefine { .. } => u32::MAX,
            Self::Choker { .. } => u32::MAX,
            Self::Minimax { .. } => u32::MAX,
            Self::LensFlare { .. } => 0,
            Self::Bulge { .. } => u32::MAX,
            Self::Split { .. } => u32::MAX,
            Self::LinearColorKey { .. } => 0,
            Self::ColorOffset { grade } => grade.halo_pixels(),
            // Same reach as the horizontal halo.
            Self::RoughenEdges { grade } => grade.halo_pixels(),
            // The blur window can be as wide as the frame.
            Self::ChannelBlur { .. } => u32::MAX,
            // Same reach as the horizontal halo.
            Self::Median { .. } => crate::median::MEDIAN_MAX_RADIUS,
            // The kernel is a per-pixel point that reads no neighbour.
            Self::SelectiveColor { .. } => 0,
            // Same reach as the horizontal halo.
            Self::Stroke { grade } => stroke_halo(grade),
            // Same reach as the horizontal halo: the authored radius, not the
            // GPU admission cap.
            Self::DustScratches { .. } => crate::dust_scratches::DUST_SCRATCHES_MAX_RADIUS,
            // Same reach as the horizontal halo.
            Self::KeyCleaner { .. } => u32::MAX,
            // Same reach as the horizontal halo.
            Self::SmartBlur { grade } => smart_blur_halo(grade),
            Self::ChromaticAberrationRemoval { grade } => chromatic_aberration_removal_halo(grade),
            Self::Basic3D { .. } => u32::MAX,
            Self::FillMatte { .. } => 0,
        }
    }
}

pub(crate) fn lower_auxiliary_spatial_pass(
    op: &EffectRenderOp,
    role: crate::AuxiliaryRole,
    sampling: crate::AuxiliarySampling,
    external_slot: Option<u32>,
) -> Option<EffectGpuSpatialPass> {
    let (EffectRenderOp::Fill { grade }, crate::AuxiliaryRole::Matte) = (op, role) else {
        return None;
    };
    Some(EffectGpuSpatialPass::FillMatte { grade: *grade, sampling, external_slot })
}

/// Reach of the stroke kernel in source pixels.
///
/// The band is centred on the contour and is at most `brush_size` wide, plus
/// the two pixels the doubling search can overshoot the profile by.
fn stroke_halo(grade: crate::StrokeGrade) -> u32 {
    ((grade.brush_size() * 0.5).ceil() as u32 + 2).min(102)
}

/// Reach of the magnify kernel in source pixels.
///
/// The inverse map is a contraction by `magnification >= 1`, so the sample of
/// any pixel inside the lens stays within the authored radius, and the
/// bilinear tap reaches one further texel. Unlike the other remapping kernels
/// this halo is therefore finite, and it is capped at the largest raster the
/// spatial driver admits so a huge authored diameter cannot overflow it.
fn magnify_halo(grade: crate::MagnifyGrade) -> u32 {
    (grade.radius_pixels().ceil() as u32).saturating_add(2).min(8192)
}

/// Reach of the smart blur window in source pixels.
///
/// The kernel averages every tap inside `radius`, plus the one fractional tap at
/// `ceil(radius)`, so the authored ceiling of 100 bounds the halo at 101.
fn chromatic_aberration_removal_halo(grade: crate::ChromaticAberrationRemovalGrade) -> u32 {
    let axis_amount = grade.lateral().abs().max(grade.longitudinal().abs());
    let edge_weight =
        1.0 + grade.edge_scale().abs() / crate::CHROMATIC_ABERRATION_REMOVAL_MAX_EDGE_SCALE;
    (std::f32::consts::SQRT_2 * edge_weight * axis_amount).ceil() as u32 + 1
}

fn smart_blur_halo(grade: crate::SmartBlurGrade) -> u32 {
    (grade.radius().ceil() as u32).saturating_add(1).min(101)
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
/// cannot be represented as one linear primary chain with direct auxiliary
/// bindings is rejected. Graph-local masks use the heterogeneous dispatcher.
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
            let kind = compiled.graph().node(*node_id).map(|node| &node.kind);
            !matches!(kind, Some(EffectGraphNodeKind::Source))
                && !(matches!(kind, Some(EffectGraphNodeKind::ExternalInput { .. }))
                    && compiled.external_auxiliary_is_read_only(*node_id))
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
        let (input, op, domain, auxiliary_pass) = match &node.kind {
            EffectGraphNodeKind::UnaryEffect { input, op } => {
                (*input, op, EffectColorDomain::SceneLinearRgb, None)
            }
            EffectGraphNodeKind::DomainEffect { input, op, domain_contract } => {
                if domain_contract.input != domain_contract.output {
                    return Err(EffectGpuPlanBlocker::NonPreservingColorDomain {
                        node_id,
                        input: domain_contract.input,
                        output: domain_contract.output,
                    });
                }
                (*input, op, domain_contract.input, None)
            }
            EffectGraphNodeKind::AuxiliaryEffect { input, auxiliary, op, role, sampling } => {
                let Some(EffectGraphNodeKind::ExternalInput { slot, .. }) =
                    compiled.graph().node(*auxiliary).map(|node| &node.kind)
                else {
                    return Err(EffectGpuPlanBlocker::UnsupportedTopology {
                        node_id,
                        kind: "graph_local_auxiliary",
                    });
                };
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
                let pass = lower_auxiliary_spatial_pass(op, *role, *sampling, Some(*slot)).ok_or(
                    EffectGpuPlanBlocker::UnsupportedTopology { node_id, kind: "auxiliary_kernel" },
                )?;
                (*input, op, EffectColorDomain::SceneLinearRgb, Some(pass))
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

        match auxiliary_pass.or_else(|| spatial_pass_for(op)) {
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
        EffectRenderOp::Magnify { grade } => Some(EffectGpuSpatialPass::Magnify { grade: *grade }),
        EffectRenderOp::Dent { grade } => Some(EffectGpuSpatialPass::Dent { grade: *grade }),
        EffectRenderOp::Warp { grade } => Some(EffectGpuSpatialPass::Warp { grade: *grade }),
        EffectRenderOp::GridWarp { grade } => {
            Some(EffectGpuSpatialPass::GridWarp { grade: *grade })
        }
        EffectRenderOp::BezierWarp { grade } => {
            Some(EffectGpuSpatialPass::BezierWarp { grade: *grade })
        }
        EffectRenderOp::VrRotateSphere { grade } => {
            Some(EffectGpuSpatialPass::VrRotateSphere { grade: *grade })
        }
        EffectRenderOp::VrSphereToPlane { grade } => {
            Some(EffectGpuSpatialPass::VrSphereToPlane { grade: *grade })
        }
        EffectRenderOp::VrProject2D { grade } => {
            Some(EffectGpuSpatialPass::VrProject2D { grade: *grade })
        }
        EffectRenderOp::WireRemoval { grade } => {
            Some(EffectGpuSpatialPass::WireRemoval { grade: *grade })
        }
        EffectRenderOp::RoughenEdges { grade } => {
            Some(EffectGpuSpatialPass::RoughenEdges { grade: *grade })
        }
        EffectRenderOp::Convolution { grade } => {
            Some(EffectGpuSpatialPass::Convolution { grade: *grade })
        }
        EffectRenderOp::Mirror { grade } => Some(EffectGpuSpatialPass::Mirror { grade: *grade }),
        EffectRenderOp::PolarCoordinates { grade } => {
            Some(EffectGpuSpatialPass::PolarCoordinates { grade: *grade })
        }
        EffectRenderOp::LensDistortion { grade } => {
            Some(EffectGpuSpatialPass::LensDistortion { grade: *grade })
        }
        EffectRenderOp::Flip { grade } => Some(EffectGpuSpatialPass::Flip { grade: *grade }),
        EffectRenderOp::Ripple { grade } => Some(EffectGpuSpatialPass::Ripple { grade: *grade }),
        EffectRenderOp::WaveWarp { grade } => {
            Some(EffectGpuSpatialPass::WaveWarp { grade: *grade })
        }
        EffectRenderOp::Transform { grade } => {
            Some(EffectGpuSpatialPass::Transform { grade: *grade })
        }
        EffectRenderOp::CornerPin { grade } => {
            Some(EffectGpuSpatialPass::CornerPin { grade: *grade })
        }
        EffectRenderOp::TurbulentDisplace { grade } => {
            Some(EffectGpuSpatialPass::TurbulentDisplace { grade: *grade })
        }
        EffectRenderOp::Deband { grade } => Some(EffectGpuSpatialPass::Deband { grade: *grade }),
        EffectRenderOp::Glow { grade } => Some(EffectGpuSpatialPass::Glow { grade: *grade }),
        EffectRenderOp::FractalNoise { grade } => {
            Some(EffectGpuSpatialPass::FractalNoise { grade: *grade })
        }
        EffectRenderOp::Keylight { grade } => {
            Some(EffectGpuSpatialPass::Keylight { grade: *grade })
        }
        EffectRenderOp::MatteRefine { grade } => {
            Some(EffectGpuSpatialPass::MatteRefine { grade: *grade })
        }
        EffectRenderOp::Choker { grade } => Some(EffectGpuSpatialPass::Choker { grade: *grade }),
        EffectRenderOp::Minimax { grade } => Some(EffectGpuSpatialPass::Minimax { grade: *grade }),
        EffectRenderOp::LensFlare { grade } => {
            Some(EffectGpuSpatialPass::LensFlare { grade: *grade })
        }
        EffectRenderOp::Split { grade } => Some(EffectGpuSpatialPass::Split { grade: *grade }),
        EffectRenderOp::LinearColorKey { grade } => {
            Some(EffectGpuSpatialPass::LinearColorKey { grade: *grade })
        }
        EffectRenderOp::ColorOffset { grade } => {
            Some(EffectGpuSpatialPass::ColorOffset { grade: *grade })
        }
        EffectRenderOp::ChannelBlur { grade } => {
            Some(EffectGpuSpatialPass::ChannelBlur { grade: *grade })
        }
        // Admission is deliberately radius-agnostic: the renderer rejects radii
        // above the GPU cap while the chain is still being planned.
        EffectRenderOp::Median { grade } => Some(EffectGpuSpatialPass::Median { grade: *grade }),
        EffectRenderOp::SelectiveColor { grade } => {
            Some(EffectGpuSpatialPass::SelectiveColor { grade: *grade })
        }
        EffectRenderOp::Stroke { grade } => Some(EffectGpuSpatialPass::Stroke { grade: *grade }),
        // Admission is deliberately radius-agnostic: the renderer rejects radii
        // above the GPU cap while the chain is still being planned.
        EffectRenderOp::DustScratches { grade } => {
            Some(EffectGpuSpatialPass::DustScratches { grade: *grade })
        }
        // Admission is deliberately radius-agnostic: the renderer validates the
        // three-scratch geometry while the chain is still being planned.
        EffectRenderOp::KeyCleaner { grade } => {
            Some(EffectGpuSpatialPass::KeyCleaner { grade: *grade })
        }
        EffectRenderOp::SmartBlur { grade } => {
            Some(EffectGpuSpatialPass::SmartBlur { grade: *grade })
        }
        EffectRenderOp::ChromaticAberrationRemoval { grade } => {
            Some(EffectGpuSpatialPass::ChromaticAberrationRemoval { grade: *grade })
        }
        EffectRenderOp::Basic3D { grade } => Some(EffectGpuSpatialPass::Basic3D { grade: *grade }),
        EffectRenderOp::DisplacementMap { .. } => None,
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
    fn dust_scratches_maps_to_the_dedicated_spatial_pass() {
        let grade = crate::DustScratchesGrade::new(2.0, 0.1, 0.0).expect("valid grade");
        let op = EffectRenderOp::DustScratches { grade };
        let pass = spatial_pass_for(&op).expect("dust & scratches runs as a spatial pass");
        assert_eq!(pass, EffectGpuSpatialPass::DustScratches { grade });
        assert_eq!(pass.name(), "dust_scratches");
        // The halo is the authored reach, not the GPU admission cap: a radius
        // of four still widens the tile before the renderer refuses the pass.
        assert_eq!(pass.horizontal_halo(), crate::DUST_SCRATCHES_MAX_RADIUS);
        assert_eq!(pass.vertical_halo(), crate::DUST_SCRATCHES_MAX_RADIUS);

        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(op);
        let pipeline =
            lower_effect_graph_to_gpu_pipeline(&compile(builder)).expect("repair pipeline");
        assert_eq!(
            pipeline.stages(),
            &[EffectGpuStage::Spatial {
                pass: EffectGpuSpatialPass::DustScratches { grade },
                node_id: pipeline.output_value(),
            }]
        );
    }

    #[test]
    fn dust_scratches_wide_radius_stays_in_the_gpu_plan_until_admission() {
        // The effects plan is radius-agnostic: every authored radius lowers to the
        // same single spatial stage, and `gpu_supported` is the flag the
        // renderer's admission gate reads. Dropping the node here would hide the
        // refusal from `texture_pass_count` and turn it into a missing pass.
        for (radius, supported) in [(1.0_f32, true), (3.0, true), (4.0, false), (255.0, false)] {
            let grade = crate::DustScratchesGrade::new(radius, 0.0, 0.0).expect("valid grade");
            assert_eq!(grade.gpu_supported(), supported, "radius {radius}");
            let mut builder = EffectGraphBuilderState::new();
            builder.append_unary(EffectRenderOp::DustScratches { grade });
            let pipeline =
                lower_effect_graph_to_gpu_pipeline(&compile(builder)).expect("repair pipeline");
            assert_eq!(
                pipeline.stages(),
                &[EffectGpuStage::Spatial {
                    pass: EffectGpuSpatialPass::DustScratches { grade },
                    node_id: pipeline.output_value(),
                }],
                "radius {radius}"
            );
        }
    }

    #[test]
    fn key_cleaner_maps_to_the_dedicated_spatial_pass() {
        let grade = crate::KeyCleanerGrade::new(3.0, 0.5, 40.0, 100.0).expect("valid grade");
        let op = EffectRenderOp::KeyCleaner { grade };
        let pass = spatial_pass_for(&op).expect("the key cleaner runs as a spatial pass");
        assert_eq!(pass, EffectGpuSpatialPass::KeyCleaner { grade });
        assert_eq!(pass.name(), "key_cleaner");
        // The erosion window is clipped, not tiled, so the reach is unbounded in
        // both directions and the renderer has to keep the whole frame.
        assert_eq!(pass.horizontal_halo(), u32::MAX);
        assert_eq!(pass.vertical_halo(), u32::MAX);

        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(op);
        let pipeline =
            lower_effect_graph_to_gpu_pipeline(&compile(builder)).expect("key cleaner pipeline");
        assert_eq!(
            pipeline.stages(),
            &[EffectGpuStage::Spatial {
                pass: EffectGpuSpatialPass::KeyCleaner { grade },
                node_id: pipeline.output_value(),
            }]
        );
    }

    #[test]
    fn key_cleaner_between_point_ops_is_three_stages() {
        let grade = crate::KeyCleanerGrade::new(3.0, 0.0, 0.0, 100.0).expect("valid grade");
        let colorama = || {
            crate::ColoramaGrade::new(crate::ColoramaSettings::default())
                .expect("the documented colorama settings are valid")
        };
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(EffectRenderOp::Colorama { grade: colorama() });
        builder.append_unary(EffectRenderOp::KeyCleaner { grade });
        builder.append_unary(EffectRenderOp::Colorama { grade: colorama() });
        let graph = compile(builder);
        let pipeline = lower_effect_graph_to_gpu_pipeline(&graph).expect("mixed pipeline");

        let stages = pipeline.stages();
        assert_eq!(stages.len(), 3);
        assert!(matches!(stages[0], EffectGpuStage::PointOps { .. }));
        assert_eq!(
            stages[1],
            EffectGpuStage::Spatial {
                pass: EffectGpuSpatialPass::KeyCleaner { grade },
                node_id: graph.schedule().ordered_nodes[2],
            }
        );
        assert!(matches!(stages[2], EffectGpuStage::PointOps { .. }));
        assert_eq!(pipeline.spatial_stage_count(), 1);
    }

    #[test]
    fn smart_blur_maps_to_the_dedicated_spatial_pass() {
        let grade = crate::SmartBlurGrade::new(3.0, 25.0, crate::SmartBlurMode::Normal)
            .expect("valid grade");
        let op = EffectRenderOp::SmartBlur { grade };
        let pass = spatial_pass_for(&op).expect("smart blur runs as a spatial pass");
        assert_eq!(pass, EffectGpuSpatialPass::SmartBlur { grade });
        assert_eq!(pass.name(), "smart_blur");
        // The window is the authored radius plus the fractional tap, and the
        // authoring ceiling of 100 bounds it at 101.
        assert_eq!(pass.horizontal_halo(), 4);
        assert_eq!(pass.vertical_halo(), 4);
        let wide = crate::SmartBlurGrade::new(100.0, 25.0, crate::SmartBlurMode::Normal)
            .expect("valid grade");
        assert_eq!(
            EffectGpuSpatialPass::SmartBlur { grade: wide }.horizontal_halo(),
            101
        );

        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(op);
        let pipeline =
            lower_effect_graph_to_gpu_pipeline(&compile(builder)).expect("smart blur pipeline");
        assert_eq!(
            pipeline.stages(),
            &[EffectGpuStage::Spatial {
                pass: EffectGpuSpatialPass::SmartBlur { grade },
                node_id: pipeline.output_value(),
            }]
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
    fn eyedropper_fill_stays_cpu_only_and_has_no_spatial_pass() {
        let grade = crate::EyedropperFillGrade::new(
            [128.0, 128.0],
            10.0,
            crate::AverageMode::Mean,
            false,
            0.5,
        )
        .expect("valid eyedropper fill");
        let op = EffectRenderOp::EyedropperFill { grade };
        assert!(spatial_pass_for(&op).is_none());

        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(op);
        let error = lower_effect_graph_to_gpu_pipeline(&compile(builder))
            .expect_err("eyedropper fill has no GPU math");

        // Eyedropper fill is CPU-only by design (`.work/gen13-design.md`), so
        // the pipeline rejects it at admission.
        assert!(
            matches!(
                error,
                EffectGpuPlanBlocker::NodeExecutionModeNotAdmitted { .. }
                    | EffectGpuPlanBlocker::UnsupportedOperation { op: "eyedropper_fill", .. }
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
    fn choker_lowers_to_a_full_frame_spatial_stage() {
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(invert());
        builder.append_unary(EffectRenderOp::Choker {
            grade: crate::ChokerGrade::new(2.5).expect("valid choker"),
        });
        let pipeline =
            lower_effect_graph_to_gpu_pipeline(&compile(builder)).expect("choker pipeline");

        assert_eq!(pipeline.spatial_stage_count(), 1);
        let pass = pipeline
            .stages()
            .iter()
            .find_map(|stage| match stage {
                EffectGpuStage::Spatial { pass, .. } => Some(*pass),
                EffectGpuStage::PointOps { .. } => None,
            })
            .expect("choker emits a spatial stage");
        assert_eq!(pass.name(), "choker");
        // Alpha morphology reads the whole frame, so no local halo bounds it.
        assert_eq!(pass.horizontal_halo(), u32::MAX);
        assert_eq!(pass.vertical_halo(), u32::MAX);
    }

    #[test]
    fn minimax_lowers_to_a_full_frame_spatial_stage() {
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(invert());
        builder.append_unary(EffectRenderOp::Minimax {
            grade: crate::MinimaxGrade::new(
                3.0,
                crate::MinimaxOperation::MinimumThenMaximum,
                crate::MinimaxChannel::Alpha,
                crate::MinimaxDirection::Both,
                1.0,
            )
            .expect("valid minimax"),
        });
        let pipeline =
            lower_effect_graph_to_gpu_pipeline(&compile(builder)).expect("minimax pipeline");

        assert_eq!(pipeline.spatial_stage_count(), 1);
        let pass = pipeline
            .stages()
            .iter()
            .find_map(|stage| match stage {
                EffectGpuStage::Spatial { pass, .. } => Some(*pass),
                EffectGpuStage::PointOps { .. } => None,
            })
            .expect("minimax emits a spatial stage");
        assert_eq!(pass.name(), "minimax");
        // The window is bounded only by the frame, so no local halo applies.
        assert_eq!(pass.horizontal_halo(), u32::MAX);
        assert_eq!(pass.vertical_halo(), u32::MAX);
    }

    #[test]
    fn channel_blur_lowers_to_a_full_frame_spatial_stage() {
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(invert());
        builder.append_unary(EffectRenderOp::ChannelBlur {
            grade: crate::ChannelBlurGrade::new(
                2.0,
                4.0,
                6.0,
                8.0,
                1.0,
                crate::ChannelBlurDirection::Both,
            )
            .expect("valid channel blur"),
        });
        let pipeline =
            lower_effect_graph_to_gpu_pipeline(&compile(builder)).expect("channel blur pipeline");

        assert_eq!(pipeline.spatial_stage_count(), 1);
        let pass = pipeline
            .stages()
            .iter()
            .find_map(|stage| match stage {
                EffectGpuStage::Spatial { pass, .. } => Some(*pass),
                EffectGpuStage::PointOps { .. } => None,
            })
            .expect("channel blur emits a spatial stage");
        assert_eq!(pass.name(), "channel_blur");
        // The blur window can be as wide as the frame, so no local halo applies.
        assert_eq!(pass.horizontal_halo(), u32::MAX);
        assert_eq!(pass.vertical_halo(), u32::MAX);
    }

    #[test]
    fn dent_lowers_to_a_full_frame_spatial_stage() {
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(invert());
        builder.append_unary(EffectRenderOp::Dent {
            grade: crate::DentGrade::new(
                0.4,
                0.5,
                [8.0, 6.0],
                crate::DentKind::Dent,
                crate::DentEdge::Smooth,
            )
            .expect("valid dent"),
        });
        let pipeline =
            lower_effect_graph_to_gpu_pipeline(&compile(builder)).expect("dent pipeline");

        assert_eq!(pipeline.spatial_stage_count(), 1);
        let pass = pipeline
            .stages()
            .iter()
            .find_map(|stage| match stage {
                EffectGpuStage::Spatial { pass, .. } => Some(*pass),
                EffectGpuStage::PointOps { .. } => None,
            })
            .expect("dent emits a spatial stage");
        assert_eq!(pass.name(), "dent");
        // The remap kernel reads anywhere in the frame, so no local halo bounds it.
        assert_eq!(pass.horizontal_halo(), u32::MAX);
        assert_eq!(pass.vertical_halo(), u32::MAX);
    }

    #[test]
    fn warp_lowers_to_a_full_frame_spatial_stage() {
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(invert());
        builder.append_unary(EffectRenderOp::Warp {
            grade: crate::WarpGrade::new(
                50.0,
                0.0,
                0.0,
                crate::WarpStyle::Arc,
                crate::WarpAxis::Horizontal,
            )
            .expect("valid warp"),
        });
        let pipeline =
            lower_effect_graph_to_gpu_pipeline(&compile(builder)).expect("warp pipeline");

        assert_eq!(pipeline.spatial_stage_count(), 1);
        let pass = pipeline
            .stages()
            .iter()
            .find_map(|stage| match stage {
                EffectGpuStage::Spatial { pass, .. } => Some(*pass),
                EffectGpuStage::PointOps { .. } => None,
            })
            .expect("warp emits a spatial stage");
        assert_eq!(pass.name(), "warp");
        // The arc displacement can reach six half-frames, so no local halo bounds it.
        assert_eq!(pass.horizontal_halo(), u32::MAX);
        assert_eq!(pass.vertical_halo(), u32::MAX);
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

        let pass = EffectGpuSpatialPass::RoughenEdges {
            grade: crate::RoughenEdgesGrade::new(
                1,
                [0.6, 0.2, 0.0],
                8.0,
                1.0,
                1.0,
                100.0,
                0.0,
                [0.0, 0.0],
                2,
                0.0,
                false,
                1,
                0,
            )
            .expect("valid roughen edges grade"),
        };
        assert_eq!(pass.name(), "roughen_edges");
        assert_eq!(pass.horizontal_halo(), 9);
        assert_eq!(pass.vertical_halo(), 9);
    }
}
