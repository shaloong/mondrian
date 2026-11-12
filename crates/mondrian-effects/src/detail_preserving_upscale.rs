//! Edge-directed Float32 upsampling on a checked, centered raster pyramid.

use crate::{
    EffectAlphaConsumer, EffectFloatExecutionError, EffectFloatFrame, EffectFloatFrameError,
    EffectFrameExtent, EffectProcessorControl, EffectRasterAxisSample, EffectRasterScale,
    EffectRasterUpscaleStage,
};

type Pixel = [f32; 4];

/// Coverage interpolation for a Detail-preserving Upscale operation.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UpscaleAlpha {
    /// Interpolate the four texels around the source position.
    Bilinear,
    /// Interpolate a bounded Catmull–Rom neighborhood.
    #[default]
    Bicubic,
}

/// Validated author controls for one upscale operation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DetailPreservingUpscaleGrade {
    scale: EffectRasterScale,
    reduce_noise: f32,
    detail: f32,
    alpha: UpscaleAlpha,
}

/// Refusal of an authored upscale control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DetailPreservingUpscaleError {
    /// The requested scale is outside the supported interval.
    #[error("upscale percentage must be finite and in [100, 30000]")]
    InvalidScale,
    /// The noise or detail percentage is outside the supported interval.
    #[error("upscale noise and detail percentages must be finite and in [0, 100]")]
    InvalidFilterControls,
}

impl DetailPreservingUpscaleGrade {
    /// Validate scale, noise reduction and detail percentages.
    pub fn new(
        scale_percent: f32,
        noise_percent: f32,
        detail_percent: f32,
        alpha: UpscaleAlpha,
    ) -> Result<Self, DetailPreservingUpscaleError> {
        let scale = EffectRasterScale::from_percent(scale_percent)
            .ok_or(DetailPreservingUpscaleError::InvalidScale)?;
        if !(0.0..=100.0).contains(&noise_percent) || !(0.0..=100.0).contains(&detail_percent) {
            return Err(DetailPreservingUpscaleError::InvalidFilterControls);
        }
        Ok(Self {
            scale,
            reduce_noise: noise_percent / 100.0,
            detail: detail_percent.abs() / 100.0,
            alpha,
        })
    }

    /// Exact scale used by raster admission.
    pub const fn scale(self) -> EffectRasterScale {
        self.scale
    }

    /// Whether the original pixels and dimensions can be reused.
    pub const fn is_identity(self) -> bool {
        self.scale.is_identity() && self.reduce_noise == 0.0
    }

    /// Detail bits, Alpha mode (bilinear 0, bicubic 1), and Reduce Noise bits.
    pub fn gpu_controls(self) -> [u32; 3] {
        [
            self.detail.to_bits(),
            u32::from(self.alpha == UpscaleAlpha::Bicubic),
            if self.reduce_noise > 0.0 {
                self.reduce_noise.to_bits()
            } else {
                0
            },
        ]
    }

    pub(crate) fn hash_state(self, state: &mut impl std::hash::Hasher) {
        use std::hash::Hash;
        self.scale.hash(state);
        self.gpu_controls().hash(state);
    }

    pub(crate) fn resize_scratch_bytes(self, input: EffectFrameExtent) -> Option<usize> {
        let levels = self.scale.stages(input)?;
        let Some(final_level) = levels.last() else {
            return Some(0);
        };
        let mut bytes = 0usize;
        let pixel_bytes =
            |extent| raster_length(extent).ok()?.checked_mul(std::mem::size_of::<Pixel>());
        if self.reduce_noise > 0.0 {
            bytes = pixel_bytes(input)?;
        }
        for level in &levels[..levels.len() - 1] {
            bytes = bytes.checked_add(pixel_bytes(level.output_extent())?)?;
        }
        bytes.checked_add(
            (final_level.output_extent().width() as usize)
                .checked_mul(std::mem::size_of::<([usize; 4], f32)>())?,
        )
    }

    pub(crate) fn apply_unity_controlled<E>(
        self,
        pixels: &mut Vec<Pixel>,
        extent: EffectFrameExtent,
        checkpoint: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<bool, E> {
        if !self.scale.is_identity() {
            return Ok(false);
        }
        if self.reduce_noise == 0.0 || extent.is_empty() {
            return Ok(true);
        }
        let Ok(mut output) = reserve(pixels.len()) else {
            return Ok(false);
        };
        denoise(pixels, extent, self.reduce_noise, &mut output, checkpoint)?;
        *pixels = output;
        Ok(true)
    }
}

/// Resize a complete working Float32 raster.
pub fn detail_preserving_upscale_frame_f32(
    frame: EffectFloatFrame,
    grade: DetailPreservingUpscaleGrade,
) -> Result<EffectFloatFrame, EffectFloatExecutionError> {
    apply_detail_preserving_upscale_controlled(
        frame,
        grade,
        &mut EffectProcessorControl::uncancelled(),
    )
}

/// Resize a complete working Float32 raster under the owner's stop checks.
pub fn apply_detail_preserving_upscale_controlled(
    mut frame: EffectFloatFrame,
    grade: DetailPreservingUpscaleGrade,
    control: &mut EffectProcessorControl<'_>,
) -> Result<EffectFloatFrame, EffectFloatExecutionError> {
    let mut checkpoint = || {
        control
            .checkpoint()
            .map_err(|reason| EffectFloatExecutionError::Stopped { reason })
    };
    checkpoint()?;
    if grade.is_identity() || frame.is_empty() {
        return Ok(frame);
    }
    let stages = grade
        .scale
        .stages(frame.extent())
        .ok_or(EffectFloatFrameError::ExtentOverflow)?;
    frame = crate::materialize_straight_coverage(
        frame,
        EffectAlphaConsumer::StraightCoverageOperation,
    )?;
    if grade.reduce_noise > 0.0 {
        let extent = frame.extent();
        let mut pixels = reserve(frame.len())?;
        denoise(
            frame.pixels(),
            extent,
            grade.reduce_noise,
            &mut pixels,
            &mut checkpoint,
        )?;
        frame = EffectFloatFrame::from_validated_parts(
            extent,
            frame.format(),
            frame.sampling(),
            pixels,
        );
    }
    for stage in stages {
        let pixels = upscale_level(&frame, stage, grade, &mut checkpoint)?;
        frame = EffectFloatFrame::from_validated_parts(
            stage.output_extent(),
            frame.format(),
            frame.sampling(),
            pixels,
        );
    }
    checkpoint()?;
    Ok(frame)
}

fn reserve<T>(length: usize) -> Result<Vec<T>, EffectFloatFrameError> {
    let required_bytes = length
        .checked_mul(std::mem::size_of::<T>())
        .ok_or(EffectFloatFrameError::ExtentOverflow)?;
    let mut values = Vec::new();
    values
        .try_reserve_exact(length)
        .map_err(|_| EffectFloatFrameError::AllocationUnavailable { required_bytes })?;
    Ok(values)
}

fn raster_length(extent: EffectFrameExtent) -> Result<usize, EffectFloatFrameError> {
    (extent.width() as usize)
        .checked_mul(extent.height() as usize)
        .ok_or(EffectFloatFrameError::ExtentOverflow)
}

fn upscale_level(
    frame: &EffectFloatFrame,
    stage: EffectRasterUpscaleStage,
    grade: DetailPreservingUpscaleGrade,
    checkpoint: &mut impl FnMut() -> Result<(), EffectFloatExecutionError>,
) -> Result<Vec<Pixel>, EffectFloatExecutionError> {
    let mut pixels = reserve(raster_length(stage.output_extent())?)?;
    let mut columns = reserve(stage.output_extent().width() as usize)?;
    let width = frame.extent().width() as usize;
    let height = frame.extent().height() as usize;
    for (index, sample) in stage.column_samples().enumerate() {
        if index.is_multiple_of(4096) {
            checkpoint()?;
        }
        columns.push((indices(sample, width), sample.fraction));
    }
    let input = frame.pixels();
    for row in stage.row_samples() {
        checkpoint()?;
        let rows = indices(row, height).map(|index| index * width);
        for block in columns.chunks(4096) {
            checkpoint()?;
            for (columns, fx) in block {
                let taps = std::array::from_fn(|i| input[rows[i / 4] + columns[i % 4]]);
                pixels.push(reconstruct(taps, *fx, row.fraction, grade));
            }
        }
    }
    Ok(pixels)
}

fn indices(sample: EffectRasterAxisSample, size: usize) -> [usize; 4] {
    let base = sample.base as usize;
    [
        base.saturating_sub(1),
        base,
        (base + 1).min(size - 1),
        (base + 2).min(size - 1),
    ]
}

fn denoise<E>(
    input: &[Pixel],
    extent: EffectFrameExtent,
    strength: f32,
    pixels: &mut Vec<Pixel>,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), E> {
    let width = extent.width() as usize;
    let height = extent.height() as usize;
    for y in 0..height {
        checkpoint()?;
        let rows = [y.saturating_sub(1), y, (y + 1).min(height - 1)].map(|row| row * width);
        for x in 0..width {
            let center = input[rows[1] + x];
            if center[3] <= 0.0 {
                pixels.push([0.0; 4]);
                continue;
            }
            let columns = [x.saturating_sub(1), x, (x + 1).min(width - 1)];
            let taps: [Pixel; 9] = std::array::from_fn(|i| input[rows[i / 3] + columns[i % 3]]);
            let weights = std::array::from_fn(|i| {
                const SPATIAL: [f32; 9] = [1.0, 2.0, 1.0, 2.0, 4.0, 2.0, 1.0, 2.0, 1.0];
                let range = (1.0 - 4.0 * contrast(center, taps[i])).max(0.0);
                SPATIAL[i] * range * range
            });
            let filtered = covered_color(&taps, &weights).expect("the covered center has a weight");
            let rgb = std::array::from_fn::<_, 3, _>(|i| {
                let a = center[i];
                let b = filtered[i];
                (a * (1.0 - strength) + b * strength).clamp(a.min(b), a.max(b))
            });
            pixels.push([rgb[0], rgb[1], rgb[2], center[3]]);
        }
    }
    Ok(())
}

fn reconstruct(taps: [Pixel; 16], x: f32, y: f32, grade: DetailPreservingUpscaleGrade) -> Pixel {
    let central = [taps[5], taps[6], taps[9], taps[10]];
    let bilinear = [(1.0 - x) * (1.0 - y), x * (1.0 - y), (1.0 - x) * y, x * y];
    let min = central.iter().map(|tap| tap[3]).fold(1.0, f32::min);
    let max = central.iter().map(|tap| tap[3]).fold(0.0, f32::max);
    let alpha = match grade.alpha {
        UpscaleAlpha::Bilinear => weighted_alpha(&central, &bilinear),
        UpscaleAlpha::Bicubic => weighted_alpha(&taps, &tensor(catmull_rom(x), catmull_rom(y))),
    }
    .clamp(min, max);
    if alpha == 0.0 {
        return [0.0; 4];
    }
    let [nw, ne, sw, se] = central;
    let descending = contrast(nw, se);
    let ascending = contrast(ne, sw);
    let confidence = (descending - ascending).abs() / (descending + ascending + 1e-5);
    let guided = if descending < ascending {
        [1.0 - x.max(y), (x - y).max(0.0), (y - x).max(0.0), x.min(y)]
    } else {
        [
            (1.0 - x - y).max(0.0),
            x.min(1.0 - y),
            y.min(1.0 - x),
            (x + y - 1.0).max(0.0),
        ]
    };
    let guide = grade.detail * confidence;
    let smooth = (1.0 - grade.detail) * (1.0 - grade.detail);
    let mut weights = tensor(b_spline(x), b_spline(y)).map(|weight| weight * smooth);
    for (i, tap) in [5, 6, 9, 10].into_iter().enumerate() {
        weights[tap] += (1.0 - smooth) * (bilinear[i] * (1.0 - guide) + guided[i] * guide);
    }
    let [r, g, b] = covered_color(&taps, &weights)
        .or_else(|| covered_color(&central, &bilinear))
        .unwrap_or([0.0; 3]);
    [r, g, b, alpha]
}

fn tensor(x: [f32; 4], y: [f32; 4]) -> [f32; 16] {
    std::array::from_fn(|i| x[i % 4] * y[i / 4])
}

fn catmull_rom(t: f32) -> [f32; 4] {
    let t2 = t * t;
    let t3 = t2 * t;
    [
        -0.5 * t + t2 - 0.5 * t3,
        1.0 - 2.5 * t2 + 1.5 * t3,
        0.5 * t + 2.0 * t2 - 1.5 * t3,
        -0.5 * t2 + 0.5 * t3,
    ]
}

fn b_spline(t: f32) -> [f32; 4] {
    let u = 1.0 - t;
    let t2 = t * t;
    let t3 = t2 * t;
    [
        u * u * u / 6.0,
        (4.0 - 6.0 * t2 + 3.0 * t3) / 6.0,
        (1.0 + 3.0 * t + 3.0 * t2 - 3.0 * t3) / 6.0,
        t3 / 6.0,
    ]
}

fn contrast(a: Pixel, b: Pixel) -> f32 {
    let signal = |p: Pixel| {
        let compressed = |v: f32| v / (1.0 + v.abs());
        (0.2126 * compressed(p[0]) + 0.7152 * compressed(p[1]) + 0.0722 * compressed(p[2])) * p[3]
    };
    (signal(a) - signal(b)).abs().max((a[3] - b[3]).abs())
}

fn weighted_alpha<const N: usize>(taps: &[Pixel; N], weights: &[f32; N]) -> f32 {
    taps.iter().zip(weights).map(|(tap, weight)| tap[3] * weight).sum()
}

fn covered_color<const N: usize>(taps: &[Pixel; N], weights: &[f32; N]) -> Option<[f32; 3]> {
    let mut max_alpha = 0.0_f32;
    let mut magnitude = [0.0_f32; 3];
    for (tap, &weight) in taps.iter().zip(weights) {
        if weight > 0.0 && tap[3] > 0.0 {
            max_alpha = max_alpha.max(tap[3]);
            for (peak, channel) in magnitude.iter_mut().zip(tap) {
                *peak = peak.max(channel.abs());
            }
        }
    }
    if max_alpha == 0.0 {
        return None;
    }
    // Normalize before accumulating so HDR colors and tiny Alpha remain finite.
    let divisor = magnitude.map(|peak| if peak > 0.0 { peak } else { 1.0 });
    let mut coverage = 0.0_f32;
    let mut color = [0.0_f32; 3];
    for (tap, &weight) in taps.iter().zip(weights) {
        if weight > 0.0 && tap[3] > 0.0 {
            let covered = (tap[3] / max_alpha) * weight;
            coverage += covered;
            for channel in 0..3 {
                color[channel] += (tap[channel] / divisor[channel]) * covered;
            }
        }
    }
    Some(std::array::from_fn(|i| {
        (color[i] / coverage).clamp(-1.0, 1.0) * magnitude[i]
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EffectColorDomain, EffectValueFormat, EffectWorkingPrecision};
    use mondrian_core::{ColorFrameAlpha, ImageSampling};

    fn grade(
        scale: f32,
        noise: f32,
        detail: f32,
        alpha: UpscaleAlpha,
    ) -> DetailPreservingUpscaleGrade {
        DetailPreservingUpscaleGrade::new(scale, noise, detail, alpha).unwrap()
    }

    fn frame(
        width: u32,
        height: u32,
        alpha: ColorFrameAlpha,
        pixels: Vec<Pixel>,
    ) -> EffectFloatFrame {
        EffectFloatFrame::from_pixels(
            EffectFrameExtent::new(width, height),
            EffectValueFormat::new(
                EffectWorkingPrecision::Float32,
                EffectColorDomain::SceneLinearRgb,
                alpha,
            ),
            ImageSampling::UNIT_SQUARE,
            pixels,
        )
        .unwrap()
    }

    #[test]
    fn unity_without_noise_reduction_preserves_original_samples() {
        let input = frame(
            2,
            1,
            ColorFrameAlpha::StraightCoverage,
            vec![[17.0, 3.0, -4.0, 0.0], [1.25, -0.25, 0.5, 0.75]],
        );
        let controls = grade(100.0, 0.0, 100.0, UpscaleAlpha::Bicubic);
        let actual = detail_preserving_upscale_frame_f32(input.clone(), controls).unwrap();
        assert_eq!(actual.pixels(), input.pixels());
        assert_eq!(actual.extent(), input.extent());
    }

    #[test]
    fn the_pyramid_preserves_constant_hdr_and_opaque_coverage() {
        let color = [4.0, -0.75, 0.125, 1.0];
        for alpha in [UpscaleAlpha::Bilinear, UpscaleAlpha::Bicubic] {
            let input = frame(3, 5, ColorFrameAlpha::Opaque, vec![color; 15]);
            let actual =
                detail_preserving_upscale_frame_f32(input, grade(350.0, 25.0, 42.0, alpha))
                    .unwrap();
            assert_eq!(actual.extent(), EffectFrameExtent::new(11, 18));
            assert!(actual.pixels().iter().all(|pixel| *pixel == color));
            actual.validate().unwrap();
        }
    }

    #[test]
    fn detail_guides_interpolation_along_the_lower_contrast_diagonal() {
        let mut taps = [[0.0, 0.0, 0.0, 1.0]; 16];
        taps[6] = [1.0, 1.0, 1.0, 1.0];
        taps[9] = [0.25, 0.25, 0.25, 1.0];
        let detailed = reconstruct(
            taps,
            0.5,
            0.5,
            grade(200.0, 0.0, 100.0, UpscaleAlpha::Bicubic),
        );
        let smooth = reconstruct(
            taps,
            0.5,
            0.5,
            grade(200.0, 0.0, 0.0, UpscaleAlpha::Bicubic),
        );
        assert!(detailed[0] < 0.001);
        assert!(smooth[0] > 0.2);
        assert_eq!(detailed[3], 1.0);
    }

    #[test]
    fn high_detail_keeps_a_slanted_edge_sharper_at_two_four_and_eight_times() {
        let (width, height) = (31, 23);
        let distance = |x: f32, y: f32| y - 0.62 * x - 4.5;
        let input = frame(
            width,
            height,
            ColorFrameAlpha::Opaque,
            (0..width * height)
                .map(|i| {
                    let x = (i % width) as f32 + 0.5;
                    let y = (i / width) as f32 + 0.5;
                    let value = f32::from(u8::from(distance(x, y) >= 0.0));
                    [value, value, value, 1.0]
                })
                .collect(),
        );
        for factor in [2.0, 4.0, 8.0] {
            let error = |detail| {
                let rendered = detail_preserving_upscale_frame_f32(
                    input.clone(),
                    grade(factor * 100.0, 0.0, detail, UpscaleAlpha::Bicubic),
                )
                .unwrap();
                let width = rendered.extent().width() as usize;
                let mut error = 0.0;
                let mut compared = 0_u32;
                for (i, pixel) in rendered.pixels().iter().enumerate() {
                    let x = ((i % width) as f32 + 0.5) / factor;
                    let y = ((i / width) as f32 + 0.5) / factor;
                    let d = distance(x, y);
                    assert!((0.0..=1.0).contains(&pixel[0]));
                    if d.abs() <= 2.0 {
                        let ideal = f32::from(u8::from(d >= 0.0));
                        error += (pixel[0] - ideal).abs();
                        compared += 1;
                    }
                }
                error / compared as f32
            };
            let smooth = error(0.0);
            let detailed = error(100.0);
            eprintln!(
                "RES-09 {factor}x slanted-edge MAE, Detail 0={smooth:.6}, Detail 100={detailed:.6}"
            );
            assert!(detailed < smooth, "{factor}x: {detailed} vs {smooth}");
        }
    }

    #[test]
    fn independent_alpha_modes_do_not_expose_hidden_color() {
        let taps: [Pixel; 16] = std::array::from_fn(|i| {
            let coverage = [1.0, 0.8, 0.2, 0.0][i % 4];
            if coverage > 0.0 {
                [2.0, -1.0, 0.5, coverage]
            } else {
                [1000.0, 1000.0, 1000.0, 0.0]
            }
        });
        let linear = reconstruct(
            taps,
            0.25,
            0.5,
            grade(200.0, 0.0, 50.0, UpscaleAlpha::Bilinear),
        );
        let cubic = reconstruct(
            taps,
            0.25,
            0.5,
            grade(200.0, 0.0, 50.0, UpscaleAlpha::Bicubic),
        );
        assert_eq!(&linear[..3], &[2.0, -1.0, 0.5]);
        assert_eq!(&linear[..3], &cubic[..3]);
        assert!((linear[3] - cubic[3]).abs() > 0.01);
    }

    #[test]
    fn source_noise_reduction_smooths_small_variation_without_crossing_a_strong_edge() {
        let input = frame(
            6,
            1,
            ColorFrameAlpha::Opaque,
            vec![
                [0.02, 0.02, 0.02, 1.0],
                [0.04, 0.04, 0.04, 1.0],
                [0.02, 0.02, 0.02, 1.0],
                [2.0, 2.0, 2.0, 1.0],
                [2.0, 2.0, 2.0, 1.0],
                [2.0, 2.0, 2.0, 1.0],
            ],
        );
        let output = detail_preserving_upscale_frame_f32(
            input,
            grade(100.0, 100.0, 50.0, UpscaleAlpha::Bicubic),
        )
        .unwrap();
        assert!(output.pixels()[1][0] < 0.033);
        assert!(output.pixels()[2][0] < 0.04);
        assert_eq!(output.pixels()[3..], [[2.0, 2.0, 2.0, 1.0]; 3]);
        output.validate().unwrap();
    }

    #[test]
    fn author_graph_lowers_and_executes_with_the_enlarged_grid() {
        use crate::{EffectGpuSpatialPass, EffectGpuStage, EffectType, PreparedEffectStack};
        use mondrian_core::{PropertyValue, TimelineTime, WorkingColorSpace};

        let kind = EffectType::DetailPreservingUpscale;
        let mut node = crate::instantiate_effect_node(kind.clone()).unwrap();
        let compile = |node: &crate::EffectNode| {
            let prepared = PreparedEffectStack::prepare(
                std::slice::from_ref(node),
                WorkingColorSpace::LinearRec709,
            )
            .unwrap();
            crate::compile_reference_render_graph(
                prepared.evaluate_graph(TimelineTime::ZERO).unwrap(),
            )
            .unwrap()
        };
        assert!(compile(&node).graph().is_identity());
        for (key, value) in [
            ("scale", PropertyValue::Float(350.0)),
            ("reduce_noise", PropertyValue::Float(25.0)),
            ("detail", PropertyValue::Float(90.0)),
            ("alpha", PropertyValue::Enum("bilinear".into())),
        ] {
            node.set_static_value_by_parameter(&kind.parameter_id(key).unwrap(), value)
                .unwrap();
        }
        let compiled = compile(&node);
        let input = frame(
            3,
            5,
            ColorFrameAlpha::StraightCoverage,
            (0..15).map(|i| [i as f32 / 4.0, -0.25, 0.5, (i % 5) as f32 / 4.0]).collect(),
        );
        let bounds = compiled.output_raster_bounds(input.extent()).unwrap();
        assert_eq!(bounds.extent(), EffectFrameExtent::new(11, 18));
        assert_eq!(bounds.origin_half_pixels(), [-8, -13]);
        let expected_grade = grade(350.0, 25.0, 90.0, UpscaleAlpha::Bilinear);
        let lowered = crate::lower_effect_graph_to_gpu_pipeline(&compiled).unwrap();
        assert!(matches!(lowered.stages(), [
            EffectGpuStage::Spatial { pass: EffectGpuSpatialPass::DetailPreservingUpscale { grade }, .. }
        ] if *grade == expected_grade));
        let actual =
            crate::apply_compiled_effect_graph_rgba_f32(input.view(), &compiled, 0).unwrap();
        let expected = detail_preserving_upscale_frame_f32(input, expected_grade).unwrap();
        assert_eq!(actual.extent(), bounds.extent());
        assert_eq!(actual.pixels(), expected.pixels());
    }
}
