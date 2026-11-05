//! CPU reference for effects that read a second input raster.
//!
//! [`sample_auxiliary`] is the single source of truth for the auxiliary
//! coordinate and filter conventions. The GPU spatial pass planned for M4 must
//! mirror the pixel-centre offset, the clamp ranges and the bilinear tap order
//! defined here; keep those constants in this module instead of repeating the
//! arithmetic in another sampler.

use crate::execution_planning::EffectFrameExtent;
use crate::graph::{
    AuxiliaryCoordinateMapping, AuxiliaryFilter, AuxiliaryOutOfBounds, AuxiliarySampling,
};
use crate::{EffectExecutionError, EffectRenderOp};

/// Distance from a pixel edge to its centre, in pixels.
///
/// A normalised coordinate `u` maps to raster position
/// `u * extent - PIXEL_CENTRE`, so `u = 0.5 / extent` lands exactly on the
/// centre of the first texel.
pub const PIXEL_CENTRE: f32 = 0.5;

/// Bilinear tap order: top-left, top-right, bottom-left, bottom-right.
///
/// The GPU mirror in M4 must accumulate the same taps with the same weights.
pub const BILINEAR_TAPS: [[i32; 2]; 4] = [[0, 0], [1, 0], [0, 1], [1, 1]];

/// Identity affine transform in the `[a, b, c, d, e, f]` layout.
pub const IDENTITY_AFFINE: [f32; 6] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0];

/// How the host delivered the auxiliary raster.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AuxiliaryResolution {
    /// The raster is laid out on the primary frame's pixel grid.
    MatchPrimary,
    /// The raster keeps its authored resolution; sampling maps by `uv`.
    Authored,
}

impl AuxiliaryResolution {
    /// Stable identity code used by graph fingerprints.
    pub const fn identity_code(self) -> u8 {
        match self {
            Self::MatchPrimary => 0,
            Self::Authored => 1,
        }
    }
}

/// Host-side delivery policy for an auxiliary raster.
///
/// The graph-declared [`AuxiliarySampling`] owns the filter and out-of-bounds
/// choice because those affect the graph identity; this policy only describes
/// how the host prepared the pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AuxiliaryPolicy {
    resolution: AuxiliaryResolution,
}

impl AuxiliaryPolicy {
    /// Construct a delivery policy.
    pub const fn new(resolution: AuxiliaryResolution) -> Self {
        Self { resolution }
    }

    /// Raster laid out on the primary frame's pixel grid.
    pub const MATCH_PRIMARY: Self = Self::new(AuxiliaryResolution::MatchPrimary);

    /// How the host delivered this raster.
    pub const fn resolution(self) -> AuxiliaryResolution {
        self.resolution
    }
}

impl Default for AuxiliaryPolicy {
    fn default() -> Self {
        Self::MATCH_PRIMARY
    }
}

/// One host-supplied auxiliary raster, encoded as straight RGBA8.
///
/// The buffer must hold `extent.width * extent.height * 4` bytes in row-major
/// order. [`Self::is_valid`] checks that invariant before execution samples it.
#[derive(Debug, Clone, Copy)]
pub struct AuxiliaryFrame<'a> {
    /// Encoded RGBA8 pixels, straight alpha.
    pub pixels: &'a [u8],
    /// Pixel extent of `pixels`.
    pub extent: EffectFrameExtent,
    /// Affine map applied to `uv` when sampling [`AuxiliaryCoordinateMapping::FrameTransform`].
    ///
    /// Layout is `[a, b, c, d, e, f]`, mapping `(u, v)` to
    /// `(a*u + b*v + c, d*u + e*v + f)`.
    pub transform: [f32; 6],
    /// Host delivery policy.
    pub policy: AuxiliaryPolicy,
}

/// A host-supplied Float32 raster with its exact color/alpha metadata.
#[derive(Debug, Clone, Copy)]
pub struct AuxiliaryFloatFrame<'a> {
    /// Complete borrowed raster. No quantization is performed by the executor.
    pub frame: crate::EffectFloatFrameView<'a>,
    /// Normalized affine mapping, in `[a, b, tx, c, d, ty]` order.
    pub transform: [f32; 6],
    /// Host delivery policy.
    pub policy: AuxiliaryPolicy,
}

impl<'a> AuxiliaryFloatFrame<'a> {
    /// Bind an auxiliary raster with an identity coordinate mapping.
    pub const fn new(frame: crate::EffectFloatFrameView<'a>, policy: AuxiliaryPolicy) -> Self {
        Self { frame, transform: IDENTITY_AFFINE, policy }
    }
}

/// Bind one compiled slot to a complete Float32 raster.
#[derive(Debug, Clone, Copy)]
pub struct AuxiliaryFloatInput<'a> {
    /// Graph-declared external slot.
    pub slot: u32,
    /// Exact raster and sampling transform.
    pub raster: AuxiliaryFloatFrame<'a>,
}

/// Sample Float32 data without clamping HDR color or rounding through RGBA8.
pub fn sample_auxiliary_f32(
    frame: AuxiliaryFloatFrame<'_>,
    uv: [f32; 2],
    sampling: AuxiliarySampling,
) -> [f32; 4] {
    let extent = frame.frame.extent();
    if extent.is_empty()
        || (extent.width() as usize).checked_mul(extent.height() as usize)
            != Some(frame.frame.pixels().len())
    {
        return [0.0; 4];
    }
    sample_raster(extent, frame.transform, uv, sampling, |x, y, policy| {
        let Some((x, y)) = texel_position(extent, x, y, policy) else {
            return [0.0; 4];
        };
        frame.frame.pixels()[y as usize * extent.width() as usize + x as usize]
    })
}

impl<'a> AuxiliaryFrame<'a> {
    /// Construct a frame on the primary grid with an identity transform.
    pub const fn new(pixels: &'a [u8], extent: EffectFrameExtent, policy: AuxiliaryPolicy) -> Self {
        Self { pixels, extent, transform: IDENTITY_AFFINE, policy }
    }

    /// Replace the affine transform.
    pub const fn with_transform(mut self, transform: [f32; 6]) -> Self {
        self.transform = transform;
        self
    }

    /// Whether `pixels` has exactly the encoded size of `extent`.
    pub fn is_valid(&self) -> bool {
        let Some(texels) =
            (self.extent.width() as usize).checked_mul(self.extent.height() as usize)
        else {
            return false;
        };
        texels.checked_mul(4) == Some(self.pixels.len())
    }

    /// Whether the raster matches the primary frame's pixel grid.
    pub const fn matches_extent(self, width: u32, height: u32) -> bool {
        self.extent.width() == width && self.extent.height() == height
    }
}

/// Sample the auxiliary raster at normalised `uv` over the primary frame.
///
/// Coordinates are transformed by [`AuxiliaryCoordinateMapping`], converted to
/// raster space with the pixel-centre offset, then filtered with
/// [`AuxiliaryFilter`]. Out-of-bounds taps follow
/// [`AuxiliaryOutOfBounds::Clamp`] or [`AuxiliaryOutOfBounds::Transparent`].
pub fn sample_auxiliary(
    frame: &AuxiliaryFrame<'_>,
    uv: [f32; 2],
    sampling: AuxiliarySampling,
) -> [f32; 4] {
    if frame.extent.is_empty() || !frame.is_valid() {
        return [0.0; 4];
    }
    sample_raster(
        frame.extent,
        frame.transform,
        uv,
        sampling,
        |x, y, policy| texel(frame, x, y, policy),
    )
}

fn sample_raster(
    extent: EffectFrameExtent,
    transform: [f32; 6],
    uv: [f32; 2],
    sampling: AuxiliarySampling,
    mut load: impl FnMut(i32, i32, AuxiliaryOutOfBounds) -> [f32; 4],
) -> [f32; 4] {
    let uv = match sampling.mapping {
        AuxiliaryCoordinateMapping::PrimaryGrid => uv,
        AuxiliaryCoordinateMapping::FrameTransform => transform_uv(transform, uv),
    };
    if !uv[0].is_finite() || !uv[1].is_finite() {
        return [0.0; 4];
    }

    let width = extent.width() as f32;
    let height = extent.height() as f32;
    // Bound coordinates before multiplying by extent, including finite values
    // whose product would overflow. Keep the linear transparent border taps.
    let uv = match sampling.out_of_bounds {
        AuxiliaryOutOfBounds::Clamp => [uv[0].clamp(0.0, 1.0), uv[1].clamp(0.0, 1.0)],
        AuxiliaryOutOfBounds::Transparent => {
            if uv[0] < -PIXEL_CENTRE / width
                || uv[0] > 1.0 + PIXEL_CENTRE / width
                || uv[1] < -PIXEL_CENTRE / height
                || uv[1] > 1.0 + PIXEL_CENTRE / height
            {
                return [0.0; 4];
            }
            uv
        }
    };
    let x = uv[0] * width - PIXEL_CENTRE;
    let y = uv[1] * height - PIXEL_CENTRE;
    match sampling.filter {
        AuxiliaryFilter::Nearest => {
            let x = (uv[0] * width).floor() as i32;
            let y = (uv[1] * height).floor() as i32;
            load(x, y, sampling.out_of_bounds)
        }
        AuxiliaryFilter::Linear => {
            let x0 = x.floor();
            let y0 = y.floor();
            let fx = x - x0;
            let fy = y - y0;
            let mut out = [0.0f32; 4];
            for (tap, offset) in BILINEAR_TAPS.iter().enumerate() {
                let weight = match tap {
                    0 => (1.0 - fx) * (1.0 - fy),
                    1 => fx * (1.0 - fy),
                    2 => (1.0 - fx) * fy,
                    _ => fx * fy,
                };
                if weight == 0.0 {
                    continue;
                }
                let texel = load(
                    (x0 as i32).saturating_add(offset[0]),
                    (y0 as i32).saturating_add(offset[1]),
                    sampling.out_of_bounds,
                );
                for channel in 0..4 {
                    out[channel] += texel[channel] * weight;
                }
            }
            out
        }
    }
}

/// Apply `uv`'s affine transform. See [`AuxiliaryFrame::transform`].
fn transform_uv(transform: [f32; 6], uv: [f32; 2]) -> [f32; 2] {
    [
        transform[0] * uv[0] + transform[1] * uv[1] + transform[2],
        transform[3] * uv[0] + transform[4] * uv[1] + transform[5],
    ]
}

/// Fetch one texel, honouring the out-of-bounds policy.
fn texel(
    frame: &AuxiliaryFrame<'_>,
    x: i32,
    y: i32,
    out_of_bounds: AuxiliaryOutOfBounds,
) -> [f32; 4] {
    let Some((x, y)) = texel_position(frame.extent, x, y, out_of_bounds) else {
        return [0.0; 4];
    };
    let offset = (y as usize * frame.extent.width() as usize + x as usize) * 4;
    let pixel = &frame.pixels[offset..offset + 4];
    pixel
        .try_into()
        .map(|pixel: [u8; 4]| pixel.map(|value| value as f32 / 255.0))
        .unwrap()
}

fn texel_position(
    extent: EffectFrameExtent,
    x: i32,
    y: i32,
    policy: AuxiliaryOutOfBounds,
) -> Option<(i32, i32)> {
    let width = i32::try_from(extent.width()).ok()?;
    let height = i32::try_from(extent.height()).ok()?;
    if width == 0 || height == 0 {
        return None;
    }
    Some(match policy {
        AuxiliaryOutOfBounds::Clamp => (x.clamp(0, width - 1), y.clamp(0, height - 1)),
        AuxiliaryOutOfBounds::Transparent => {
            if x < 0 || y < 0 || x >= width || y >= height {
                return None;
            }
            (x, y)
        }
    })
}

/// Scratch frames an auxiliary effect needs beyond its primary input.
///
/// Direct sampling needs no budget. Any operation that needs scratch at all is
/// charged one auxiliary scratch frame; the exact per-operation budget is
/// refined together with the M4 spatial pass.
pub const fn render_op_auxiliary_scratch_frames(op: &EffectRenderOp) -> usize {
    if crate::adjustment::render_op_f32_scratch_frames(op) == 0 {
        0
    } else {
        1
    }
}

/// Whether an operation has an explicit Float32 auxiliary kernel.
pub(crate) fn supports_auxiliary_f32(op: &EffectRenderOp) -> bool {
    matches!(op, EffectRenderOp::Fill { .. })
}

/// Apply the operation's own auxiliary formula; never infer it from the role.
pub(crate) fn apply_auxiliary_effect_f32<E>(
    primary: &mut [[f32; 4]],
    width: u32,
    height: u32,
    op: &EffectRenderOp,
    frame: AuxiliaryFloatFrame<'_>,
    sampling: AuxiliarySampling,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let EffectRenderOp::Fill { grade } = op else {
        return Ok(false);
    };
    let unmasked = crate::FillGrade::new(
        grade.feather_x(),
        grade.feather_y(),
        false,
        grade.color(),
        grade.opacity(),
    )
    .expect("validated fill grade");
    checkpoint()?;
    for (index, pixel) in primary.iter_mut().enumerate() {
        if index % 4096 == 0 {
            checkpoint()?;
        }
        let x = index as u32 % width;
        let y = index as u32 / width;
        let uv = [
            (x as f32 + PIXEL_CENTRE) / width as f32,
            (y as f32 + PIXEL_CENTRE) / height as f32,
        ];
        let mask = sample_auxiliary_f32(frame, uv, sampling)[3].clamp(0.0, 1.0);
        let coverage = mask * unmasked.coverage(x, y, width, height);
        let coverage = if grade.invert() {
            1.0 - coverage
        } else {
            coverage
        };
        let color = grade.color();
        *pixel = crate::adjustment::blend_rgba_f32_pixel_seeded(
            *pixel,
            [color[0], color[1], color[2], coverage * grade.opacity()],
            1.0,
            mondrian_core::BlendMode::Normal,
            0,
        );
    }
    checkpoint()?;
    Ok(true)
}

/// Apply an auxiliary-reading operation to the primary RGBA8 frame.
///
/// The encoded reference executor refuses auxiliary operations. Their exact
/// pixel formulas run through the Float32 executor without quantization.
pub(crate) fn apply_auxiliary_effect(
    _primary: &mut Vec<u8>,
    _width: u32,
    _height: u32,
    _op: &EffectRenderOp,
    _frame: &AuxiliaryFrame<'_>,
    _sampling: AuxiliarySampling,
    _frame_seed: i64,
) -> Result<(), EffectExecutionError> {
    Err(EffectExecutionError::AuxiliaryRequiresFloat32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{AuxiliaryFilter, AuxiliaryOutOfBounds, AuxiliarySampling};

    fn float_view(pixels: &[[f32; 4]], width: u32, height: u32) -> crate::EffectFloatFrameView<'_> {
        crate::EffectFloatFrameView::new(
            EffectFrameExtent::new(width, height),
            crate::EffectValueFormat::new(
                crate::EffectWorkingPrecision::Float32,
                crate::EffectColorDomain::SceneLinearRgb,
                mondrian_core::ColorFrameAlpha::StraightCoverage,
            ),
            mondrian_core::ImageSampling::UNIT_SQUARE,
            pixels,
        )
    }

    #[test]
    fn auxiliary_float_sampling_preserves_hdr_and_negative_values() {
        let pixels = [[-2.0, 4.0, 0.25, 1.0], [6.0, -4.0, 1.0, 0.0]];
        let frame =
            AuxiliaryFloatFrame::new(float_view(&pixels, 2, 1), AuxiliaryPolicy::MATCH_PRIMARY);
        assert_eq!(
            sample_auxiliary_f32(frame, [0.25, 0.5], sampling(AuxiliaryFilter::Linear)),
            pixels[0]
        );
        assert_eq!(
            sample_auxiliary_f32(frame, [0.5, 0.5], sampling(AuxiliaryFilter::Linear)),
            [2.0, 0.0, 0.625, 0.5]
        );
        assert_eq!(
            sample_auxiliary_f32(frame, [f32::MAX, 0.5], sampling(AuxiliaryFilter::Linear)),
            pixels[1]
        );
        let transparent = AuxiliarySampling {
            out_of_bounds: AuxiliaryOutOfBounds::Transparent,
            ..sampling(AuxiliaryFilter::Linear)
        };
        assert_eq!(
            sample_auxiliary_f32(frame, [f32::MAX, 0.5], transparent),
            [0.0; 4]
        );
    }

    fn auxiliary_fill_graph() -> std::sync::Arc<crate::CompiledEffectGraph> {
        let mut builder = crate::EffectGraphBuilderState::new();
        // A nontrivial input chain exercises the real cross-call output cache.
        for _ in 0..4 {
            builder.append_unary(EffectRenderOp::GaussianBlur { radius: 1.0 });
        }
        let input = builder.current_output();
        let format = float_view(&[], 0, 0).format();
        let auxiliary = builder.append_external_input(
            0,
            format,
            crate::EffectColorDomainContract::SCENE_LINEAR,
        );
        let output = builder.add_auxiliary_effect_with_role(
            input,
            auxiliary,
            EffectRenderOp::Fill {
                grade: crate::FillGrade::new(0.0, 0.0, false, [1.0, 0.0, 0.0], 1.0).unwrap(),
            },
            crate::AuxiliaryRole::Matte,
        );
        builder.set_current_output(output);
        crate::compile_reference_render_graph(builder.finish()).expect("Float32 auxiliary graph")
    }

    #[test]
    fn auxiliary_float_cache_identity_includes_the_second_raster() {
        let graph = auxiliary_fill_graph();
        assert!(graph.output_cache_enabled());
        let primary = [[0.0, 0.0, 2.0, 1.0]; 2];
        let opaque = [[0.0, 0.0, 0.0, 1.0]; 2];
        let transparent = [[0.0; 4]; 2];
        let mut session = crate::EffectExecutionSession::default();
        for pixels in [&opaque, &transparent, &opaque] {
            let binding = AuxiliaryFloatInput {
                slot: 0,
                raster: AuxiliaryFloatFrame::new(
                    float_view(pixels, 2, 1),
                    AuxiliaryPolicy::MATCH_PRIMARY,
                ),
            };
            let output = session
                .apply_compiled_rgba_f32_with_auxiliary(
                    float_view(&primary, 2, 1),
                    &graph,
                    0,
                    &[binding],
                    &mut crate::EffectProcessorControl::uncancelled(),
                )
                .expect("bound Float32 matte");
            let expected = if pixels[0][3] == 1.0 {
                [1.0, 0.0, 0.0, 1.0]
            } else {
                primary[0]
            };
            for pixel in output.pixels() {
                for channel in 0..4 {
                    assert!((pixel[channel] - expected[channel]).abs() < 1e-5);
                }
            }
        }
        // Missing and duplicate bindings must fail even after a cached success.
        assert!(matches!(
            session.apply_compiled_rgba_f32_with_auxiliary(
                float_view(&primary, 2, 1),
                &graph,
                0,
                &[],
                &mut crate::EffectProcessorControl::uncancelled(),
            ),
            Err(crate::EffectFloatExecutionError::AuxiliaryInput(
                EffectExecutionError::AuxiliaryFrameMissing { slot: 0 }
            ))
        ));
        let binding = AuxiliaryFloatInput {
            slot: 0,
            raster: AuxiliaryFloatFrame::new(
                float_view(&opaque, 2, 1),
                AuxiliaryPolicy::MATCH_PRIMARY,
            ),
        };
        assert!(session
            .apply_compiled_rgba_f32_with_auxiliary(
                float_view(&primary, 2, 1),
                &graph,
                0,
                &[binding, binding],
                &mut crate::EffectProcessorControl::uncancelled(),
            )
            .is_err());
    }

    #[test]
    fn fill_honours_a_graph_mask_source() {
        let mut builder = crate::EffectGraphBuilderState::new();
        let mask = builder.add_mask_source(
            crate::MaskShape::Rectangle {
                x: 0.0,
                y: 0.0,
                width: 0.5,
                height: 1.0,
                corner_radius: 0.0,
            },
            0.0,
            0.0,
            1.0,
        );
        let output = builder.add_auxiliary_effect_with_role(
            builder.source(),
            mask,
            EffectRenderOp::Fill {
                grade: crate::FillGrade::new(0.0, 0.0, false, [1.0, 0.0, 0.0], 1.0).unwrap(),
            },
            crate::AuxiliaryRole::Matte,
        );
        builder.set_current_output(output);
        let graph =
            crate::compile_reference_render_graph(builder.finish()).expect("graph-local matte");
        assert!(graph.auxiliary_inputs().is_empty());
        let primary = [[0.0, 0.0, 2.0, 1.0]; 4];
        let result =
            crate::apply_compiled_effect_graph_rgba_f32(float_view(&primary, 4, 1), &graph, 0)
                .expect("masked fill");
        assert_eq!(
            result.pixels(),
            &[
                [1.0, 0.0, 0.0, 1.0],
                [1.0, 0.0, 0.0, 1.0],
                primary[2],
                primary[3]
            ]
        );
    }

    #[test]
    fn auxiliary_float_binding_validation_precedes_cached_output() {
        let graph = auxiliary_fill_graph();
        let primary = [[0.0, 0.0, 2.0, 1.0]; 2];
        let pixels = [[0.0, 0.0, 0.0, 1.0]];
        let mut session = crate::EffectExecutionSession::default();
        let mut binding = AuxiliaryFloatInput {
            slot: 0,
            raster: AuxiliaryFloatFrame::new(
                float_view(&pixels, 1, 1),
                AuxiliaryPolicy::MATCH_PRIMARY,
            ),
        };
        let mut execute = |binding: AuxiliaryFloatInput<'_>| {
            session.apply_compiled_rgba_f32_with_auxiliary(
                float_view(&primary, 2, 1),
                &graph,
                0,
                &[binding],
                &mut crate::EffectProcessorControl::uncancelled(),
            )
        };
        assert!(matches!(
            execute(binding),
            Err(crate::EffectFloatExecutionError::AuxiliaryInput(
                EffectExecutionError::InvalidAuxiliaryFrame { reason: "resolution_mismatch", .. }
            ))
        ));
        binding.raster.policy = AuxiliaryPolicy::new(AuxiliaryResolution::Authored);
        assert_eq!(
            execute(binding).unwrap().pixels(),
            &[[1.0, 0.0, 0.0, 1.0]; 2]
        );
        binding.raster.transform[0] = f32::NAN;
        assert!(matches!(
            execute(binding),
            Err(crate::EffectFloatExecutionError::AuxiliaryInput(
                EffectExecutionError::InvalidAuxiliaryFrame { reason: "nonfinite_transform", .. }
            ))
        ));
        binding.raster.transform = IDENTITY_AFFINE;
        binding.raster.frame = crate::EffectFloatFrameView::new(
            binding.raster.frame.extent(),
            binding.raster.frame.format().with_domain(crate::EffectColorDomain::AlphaMask),
            mondrian_core::ImageSampling::UNIT_SQUARE,
            &pixels,
        );
        assert!(matches!(
            execute(binding),
            Err(crate::EffectFloatExecutionError::AuxiliaryInput(
                EffectExecutionError::InvalidAuxiliaryFrame { reason: "format_mismatch", .. }
            ))
        ));
    }

    #[test]
    fn inconsistent_auxiliary_domain_metadata_is_refused_at_compile_time() {
        let compiled = auxiliary_fill_graph();
        let mut graph = compiled.graph().clone();
        let producer = graph
            .nodes
            .iter_mut()
            .find(|node| matches!(node.kind, crate::EffectGraphNodeKind::ExternalInput { .. }))
            .unwrap();
        let crate::EffectGraphNodeKind::ExternalInput { domain, .. } = &mut producer.kind else {
            unreachable!()
        };
        domain.output = crate::EffectColorDomain::AlphaMask;
        assert!(matches!(
            crate::validate_auxiliary_graph(&graph),
            Err(crate::EffectGraphError::InvalidAuxiliaryFormat { .. })
        ));
        assert!(crate::compile_reference_render_graph(graph).is_none());
    }

    fn frame(pixels: &[u8], width: u32, height: u32) -> AuxiliaryFrame<'_> {
        AuxiliaryFrame::new(
            pixels,
            EffectFrameExtent::new(width, height),
            AuxiliaryPolicy::MATCH_PRIMARY,
        )
    }

    fn sampling(filter: AuxiliaryFilter) -> AuxiliarySampling {
        AuxiliarySampling::new(
            AuxiliaryCoordinateMapping::PrimaryGrid,
            filter,
            AuxiliaryOutOfBounds::Clamp,
        )
    }

    #[test]
    fn the_pixel_centre_lands_on_the_first_texel() {
        let pixels = [10, 20, 30, 40, 50, 60, 70, 80];
        let frame = frame(&pixels, 2, 1);
        // u = 0.5 / 2 = 0.25, v = 0.5 / 1 = 0.5.
        let linear = sample_auxiliary(&frame, [0.25, 0.5], sampling(AuxiliaryFilter::Linear));
        assert_eq!(
            linear,
            [10.0 / 255.0, 20.0 / 255.0, 30.0 / 255.0, 40.0 / 255.0]
        );
        let nearest = sample_auxiliary(&frame, [0.25, 0.5], sampling(AuxiliaryFilter::Nearest));
        assert_eq!(nearest, linear);
    }

    #[test]
    fn clamp_edges_repeat_the_border_texel() {
        let pixels = [255, 0, 0, 255, 0, 0, 255, 255];
        let frame = frame(&pixels, 2, 1);
        let at_left = sample_auxiliary(&frame, [0.0, 0.5], sampling(AuxiliaryFilter::Linear));
        assert_eq!(at_left, [1.0, 0.0, 0.0, 1.0]);
        let at_right = sample_auxiliary(&frame, [1.0, 0.5], sampling(AuxiliaryFilter::Linear));
        assert_eq!(at_right, [0.0, 0.0, 1.0, 1.0]);
    }

    #[test]
    fn transparent_out_of_bounds_samples_zero() {
        let pixels = [255, 255, 255, 255];
        let frame = frame(&pixels, 1, 1);
        let sampling = AuxiliarySampling::new(
            AuxiliaryCoordinateMapping::PrimaryGrid,
            AuxiliaryFilter::Nearest,
            AuxiliaryOutOfBounds::Transparent,
        );
        assert_eq!(sample_auxiliary(&frame, [-0.5, 0.5], sampling), [0.0; 4]);
        assert_eq!(sample_auxiliary(&frame, [0.5, 1.5], sampling), [0.0; 4]);
    }

    #[test]
    fn linear_and_nearest_differ_between_texels() {
        let pixels = [0, 0, 0, 255, 200, 200, 200, 255];
        let frame = frame(&pixels, 2, 1);
        let uv = [0.5, 0.5];
        let linear = sample_auxiliary(&frame, uv, sampling(AuxiliaryFilter::Linear));
        let nearest = sample_auxiliary(&frame, uv, sampling(AuxiliaryFilter::Nearest));
        assert!((linear[0] - 100.0 / 255.0).abs() < 1e-6);
        assert!((nearest[0] - 200.0 / 255.0).abs() < 1e-6);
        assert!((nearest[2] - 200.0 / 255.0).abs() < 1e-6);
    }
}
