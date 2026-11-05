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
    let uv = match sampling.mapping {
        AuxiliaryCoordinateMapping::PrimaryGrid => uv,
        AuxiliaryCoordinateMapping::FrameTransform => transform_uv(frame.transform, uv),
    };
    if !uv[0].is_finite() || !uv[1].is_finite() {
        return [0.0; 4];
    }

    let width = frame.extent.width() as f32;
    let height = frame.extent.height() as f32;
    let x = uv[0] * width - PIXEL_CENTRE;
    let y = uv[1] * height - PIXEL_CENTRE;
    match sampling.filter {
        AuxiliaryFilter::Nearest => {
            let x = (uv[0] * width).floor() as i32;
            let y = (uv[1] * height).floor() as i32;
            texel(frame, x, y, sampling.out_of_bounds)
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
                let texel = texel(
                    frame,
                    x0 as i32 + offset[0],
                    y0 as i32 + offset[1],
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
    let width = frame.extent.width() as i32;
    let height = frame.extent.height() as i32;
    let (x, y) = match out_of_bounds {
        AuxiliaryOutOfBounds::Clamp => (x.clamp(0, width - 1), y.clamp(0, height - 1)),
        AuxiliaryOutOfBounds::Transparent => {
            if x < 0 || y < 0 || x >= width || y >= height {
                return [0.0; 4];
            }
            (x, y)
        }
    };
    let offset = ((y as usize * width as usize) + x as usize) * 4;
    let pixel = &frame.pixels[offset..offset + 4];
    [
        pixel[0] as f32 / 255.0,
        pixel[1] as f32 / 255.0,
        pixel[2] as f32 / 255.0,
        pixel[3] as f32 / 255.0,
    ]
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

/// Apply an auxiliary-reading operation to the primary RGBA8 frame.
///
/// M1 has no operation that consumes the second input semantically, so the
/// fallback keeps the existing single-input result and modulates primary alpha
/// by the sampled auxiliary alpha. Role-specific pixel formulas arrive with the
/// first real auxiliary operation; this function exists so the graph, cache and
/// liveness plumbing is exercised end to end.
pub(crate) fn apply_auxiliary_effect(
    primary: &mut Vec<u8>,
    width: u32,
    height: u32,
    op: &EffectRenderOp,
    frame: &AuxiliaryFrame<'_>,
    sampling: AuxiliarySampling,
    frame_seed: i64,
) -> Result<(), EffectExecutionError> {
    crate::adjustment::apply_render_op(primary, width, height, op, frame_seed)?;
    let Some(texels) = (width as usize).checked_mul(height as usize) else {
        return Ok(());
    };
    if texels == 0 || primary.len() < texels * 4 {
        return Ok(());
    }
    let width_scale = width as f32;
    let height_scale = height as f32;
    for y in 0..height {
        for x in 0..width {
            let uv = [
                (x as f32 + PIXEL_CENTRE) / width_scale,
                (y as f32 + PIXEL_CENTRE) / height_scale,
            ];
            let auxiliary = sample_auxiliary(frame, uv, sampling);
            let offset = ((y as usize * width as usize) + x as usize) * 4;
            let alpha = primary[offset + 3] as f32 / 255.0;
            let modulated = (alpha * auxiliary[3]).clamp(0.0, 1.0);
            primary[offset + 3] = (modulated * 255.0).round() as u8;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{AuxiliaryFilter, AuxiliaryOutOfBounds, AuxiliarySampling};

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
