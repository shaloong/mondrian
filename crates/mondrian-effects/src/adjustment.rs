use crate::{
    coverage::{has_positive_coverage, straight_rgba_from_premultiplied},
    EffectExecutionError, EffectRenderOp,
};
use mondrian_core::types::{BlendMode, WorkingColorSpace};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::panic::{catch_unwind, AssertUnwindSafe};

const GAUSSIAN_BOX_PASS_COUNT: usize = 3;

pub use crate::execution::{
    apply_compiled_effect_graph, apply_compiled_effect_graph_pass, CustomEffectRenderProcessor,
};
#[cfg(test)]
use crate::execution::{apply_effect_render_graph, apply_effect_render_plan};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AdjustmentLayerParams {
    pub exposure: f32,
    pub contrast: f32,
    pub saturation: f32,
    pub working_color_space: WorkingColorSpace,
    pub blur_radius: f32,
    pub sharpen_amount: f32,
    pub vignette_intensity: f32,
    pub vignette_feather: f32,
    pub chromatic_aberration: f32,
    pub grain_amount: f32,
}

impl Default for AdjustmentLayerParams {
    fn default() -> Self {
        Self {
            exposure: 0.0,
            contrast: 1.0,
            saturation: 1.0,
            working_color_space: WorkingColorSpace::LinearRec709,
            blur_radius: 0.0,
            sharpen_amount: 0.0,
            vignette_intensity: 0.0,
            vignette_feather: 0.65,
            chromatic_aberration: 0.0,
            grain_amount: 0.0,
        }
    }
}

impl AdjustmentLayerParams {
    pub fn is_identity(&self) -> bool {
        (self.exposure.abs() <= 1.0e-4)
            && ((self.contrast - 1.0).abs() <= 1.0e-4)
            && ((self.saturation - 1.0).abs() <= 1.0e-4)
            && (self.blur_radius.abs() <= 1.0e-4)
            && (self.sharpen_amount.abs() <= 1.0e-4)
            && (self.vignette_intensity.abs() <= 1.0e-4)
            && (self.chromatic_aberration.abs() <= 1.0e-4)
            && (self.grain_amount.abs() <= 1.0e-4)
    }

    pub fn signature_words(&self) -> [u32; 10] {
        [
            self.exposure.to_bits(),
            self.contrast.to_bits(),
            self.saturation.to_bits(),
            self.working_color_space as u32,
            self.blur_radius.to_bits(),
            self.sharpen_amount.to_bits(),
            self.vignette_intensity.to_bits(),
            self.vignette_feather.to_bits(),
            self.chromatic_aberration.to_bits(),
            self.grain_amount.to_bits(),
        ]
    }
}

pub fn apply_adjustment_layer(
    input: &[u8],
    width: u32,
    height: u32,
    params: AdjustmentLayerParams,
    frame_seed: i64,
) -> Result<Vec<u8>, EffectExecutionError> {
    if params.is_identity() || input.is_empty() || width == 0 || height == 0 {
        return Ok(input.to_vec());
    }

    let mut working = input.to_vec();
    apply_primary_color_adjustments(&mut working, params);

    let Some(blur_radius) = valid_gaussian_radius(params.blur_radius) else {
        return Err(EffectExecutionError::InvalidRenderParameter {
            op: "adjustment_layer",
            parameter: "blur_radius",
        });
    };
    if blur_radius > 1.0e-4 {
        working = gaussian_blur_rgba8(&working, width as usize, height as usize, blur_radius);
    }

    if params.sharpen_amount > 1.0e-4 {
        let blurred = box_blur_rgb(&working, width as usize, height as usize, 1);
        apply_unsharp_mask(
            &mut working,
            &blurred,
            params.sharpen_amount.clamp(0.0, 2.0),
        );
    }

    if params.chromatic_aberration > 1.0e-4 {
        working = apply_chromatic_aberration(
            &working,
            width as usize,
            height as usize,
            params.chromatic_aberration.clamp(0.0, 1.0),
        );
    }

    if params.vignette_intensity > 1.0e-4 {
        apply_vignette(&mut working, width as usize, height as usize, params);
    }

    if params.grain_amount > 1.0e-4 {
        apply_grain(
            &mut working,
            width as usize,
            height as usize,
            params.grain_amount.clamp(0.0, 1.0),
            frame_seed,
        );
    }

    Ok(working)
}

pub(crate) fn apply_render_op(
    working: &mut Vec<u8>,
    width: u32,
    height: u32,
    op: &EffectRenderOp,
    frame_seed: i64,
) -> Result<(), EffectExecutionError> {
    match op {
        EffectRenderOp::ColorAdjust {
            exposure,
            contrast,
            saturation,
            working_color_space,
        } => {
            apply_primary_color_adjustments(
                working,
                AdjustmentLayerParams {
                    exposure: *exposure,
                    contrast: *contrast,
                    saturation: *saturation,
                    working_color_space: *working_color_space,
                    ..AdjustmentLayerParams::default()
                },
            );
        }
        EffectRenderOp::WhiteBalance { grade } => {
            apply_point_grade_rgba8(working, |rgb| grade.apply(rgb));
        }
        EffectRenderOp::Primaries { grade } => {
            apply_point_grade_rgba8(working, |rgb| grade.apply(rgb));
        }
        EffectRenderOp::HdrGrading { grade } => {
            apply_point_grade_rgba8(working, |rgb| grade.apply(rgb));
        }
        EffectRenderOp::AscCdl { grade } => {
            apply_point_grade_rgba8(working, |rgb| grade.apply(rgb));
        }
        EffectRenderOp::GamutCompression { grade } => {
            apply_point_grade_rgba8(working, |rgb| grade.apply(rgb));
        }
        EffectRenderOp::HighlightRecovery { grade } => {
            apply_point_grade_rgba8(working, |rgb| grade.apply(rgb));
        }
        EffectRenderOp::ColorCurves { curves } => {
            apply_point_grade_rgba8(working, |rgb| curves.apply(rgb));
        }
        EffectRenderOp::Qualifier { .. } | EffectRenderOp::MattePreview { .. } => {
            return Err(EffectExecutionError::InvalidRenderParameter {
                op: "qualifier",
                parameter: "requires_float32",
            });
        }
        EffectRenderOp::GaussianBlur { radius } => {
            let Some(radius) = valid_gaussian_radius(*radius) else {
                return Err(EffectExecutionError::InvalidRenderParameter {
                    op: "gaussian_blur",
                    parameter: "radius",
                });
            };
            if radius > 1.0e-4 {
                *working = gaussian_blur_rgba8(working, width as usize, height as usize, radius);
            }
        }
        EffectRenderOp::Sharpen { amount } => {
            if *amount > 1.0e-4 {
                let blurred = box_blur_rgb(working, width as usize, height as usize, 1);
                apply_unsharp_mask(working, &blurred, amount.clamp(0.0, 2.0));
            }
        }
        EffectRenderOp::Vignette { intensity, feather } => {
            if *intensity > 1.0e-4 {
                apply_vignette(
                    working,
                    width as usize,
                    height as usize,
                    AdjustmentLayerParams {
                        vignette_intensity: *intensity,
                        vignette_feather: *feather,
                        ..AdjustmentLayerParams::default()
                    },
                );
            }
        }
        EffectRenderOp::ChromaticAberration { amount } => {
            if *amount > 1.0e-4 {
                *working = apply_chromatic_aberration(
                    working,
                    width as usize,
                    height as usize,
                    amount.clamp(0.0, 1.0),
                );
            }
        }
        EffectRenderOp::Grain { amount } => {
            if *amount > 1.0e-4 {
                apply_grain(
                    working,
                    width as usize,
                    height as usize,
                    amount.clamp(0.0, 1.0),
                    frame_seed,
                );
            }
        }
        EffectRenderOp::Crop { left, top, right, bottom } => {
            let Some(bounds) = normalized_crop_bounds(width, height, *left, *top, *right, *bottom)
            else {
                return Err(EffectExecutionError::InvalidRenderParameter {
                    op: "crop",
                    parameter: "insets",
                });
            };
            apply_crop_rgba8(working, width, bounds);
        }
        EffectRenderOp::TemporalFrameBlend { .. } => {
            return Err(EffectExecutionError::TemporalFrameProviderRequired);
        }
        EffectRenderOp::Lut3D { lut, intensity } => {
            lut.apply_rgba8_in_place(working, *intensity);
        }
        EffectRenderOp::Custom { key, params, processor, .. } => {
            if let Some(processor) = processor {
                let mut staged = working.clone();
                let result = catch_unwind(AssertUnwindSafe(|| {
                    (processor.processor())(&mut staged, width, height, params, frame_seed)
                }));
                match result {
                    Ok(Ok(())) => *working = staged,
                    Ok(Err(error)) => {
                        let reason = error.to_string();
                        processor.record_runtime_failure(reason.clone());
                        return Err(EffectExecutionError::CustomProcessorFailed {
                            key: key.clone(),
                            reason,
                        });
                    }
                    Err(_) => {
                        let reason = "custom render processor panicked".to_string();
                        processor.record_runtime_failure(reason.clone());
                        return Err(EffectExecutionError::CustomProcessorFailed {
                            key: key.clone(),
                            reason,
                        });
                    }
                }
            } else {
                return Err(EffectExecutionError::CustomProcessorUnavailable { key: key.clone() });
            }
        }
    }
    Ok(())
}

pub(crate) fn apply_render_op_f32(
    working: &mut Vec<[f32; 4]>,
    width: u32,
    height: u32,
    op: &EffectRenderOp,
    frame_seed: i64,
) -> bool {
    match apply_render_op_f32_controlled(working, width, height, op, frame_seed, &mut || {
        Ok::<(), Infallible>(())
    }) {
        Ok(supported) => supported,
        Err(never) => match never {},
    }
}

/// Peak same-sized scratch images owned by the current CPU Float32 kernel in
/// addition to its caller-owned mutable output.
///
/// Keep this beside the dispatcher so every executor admits the concrete
/// implementation rather than maintaining a parallel memory model.
pub(crate) const fn render_op_f32_scratch_frames(op: &EffectRenderOp) -> usize {
    match op {
        EffectRenderOp::GaussianBlur { .. } | EffectRenderOp::Qualifier { .. } => 1,
        EffectRenderOp::Sharpen { .. } => 2,
        EffectRenderOp::ChromaticAberration { .. } => 1,
        EffectRenderOp::ColorAdjust { .. }
        | EffectRenderOp::WhiteBalance { .. }
        | EffectRenderOp::Primaries { .. }
        | EffectRenderOp::HdrGrading { .. }
        | EffectRenderOp::AscCdl { .. }
        | EffectRenderOp::GamutCompression { .. }
        | EffectRenderOp::HighlightRecovery { .. }
        | EffectRenderOp::ColorCurves { .. }
        | EffectRenderOp::MattePreview { .. }
        | EffectRenderOp::Vignette { .. }
        | EffectRenderOp::Grain { .. }
        | EffectRenderOp::Crop { .. }
        | EffectRenderOp::TemporalFrameBlend { .. }
        | EffectRenderOp::Lut3D { .. }
        | EffectRenderOp::Custom { .. } => 0,
    }
}

/// One rectangular pixel buffer positioned in a complete frame coordinate
/// space.
///
/// Spatially local Effect execution may retain only this region, but
/// coordinate-dependent operations must still observe the complete frame's
/// origin and extent. Construction is crate-private because the execution
/// planner is responsible for proving containment before pixels are supplied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EffectRasterRegion {
    frame_width: u32,
    frame_height: u32,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

impl EffectRasterRegion {
    pub(crate) const fn full_frame(width: u32, height: u32) -> Self {
        Self {
            frame_width: width,
            frame_height: height,
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    pub(crate) const fn new(
        frame_width: u32,
        frame_height: u32,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    ) -> Self {
        Self { frame_width, frame_height, x, y, width, height }
    }

    const fn is_full_frame(self) -> bool {
        self.x == 0
            && self.y == 0
            && self.width == self.frame_width
            && self.height == self.frame_height
    }

    pub(crate) const fn row_width(self) -> usize {
        self.width as usize
    }

    pub(crate) fn global_row_start(self, local_y: usize) -> u64 {
        (u64::from(self.y) + local_y as u64) * u64::from(self.frame_width) + u64::from(self.x)
    }

    pub(crate) fn is_valid_for(self, pixel_count: usize) -> bool {
        let right = u64::from(self.x) + u64::from(self.width);
        let bottom = u64::from(self.y) + u64::from(self.height);
        let expected = usize::try_from(self.width).ok().and_then(|width| {
            usize::try_from(self.height).ok().and_then(|height| width.checked_mul(height))
        });
        right <= u64::from(self.frame_width)
            && bottom <= u64::from(self.frame_height)
            && expected == Some(pixel_count)
    }
}

/// Execute one Float32 render operation with caller-owned cooperative
/// checkpoints.
///
/// `checkpoint` is invoked before work and at deterministic row/block
/// boundaries inside long-running scalar kernels. An error aborts the
/// operation immediately; callers own the partially mutated private buffer and
/// must not publish it.
pub(crate) fn apply_render_op_f32_controlled<E>(
    working: &mut Vec<[f32; 4]>,
    width: u32,
    height: u32,
    op: &EffectRenderOp,
    frame_seed: i64,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    apply_render_op_f32_region_controlled(
        working,
        EffectRasterRegion::full_frame(width, height),
        op,
        frame_seed,
        checkpoint,
    )
}

/// Execute one Float32 operation over a retained frame region while preserving
/// complete-frame coordinate semantics.
///
/// Finite-kernel operations may process the admitted halo as a local buffer;
/// the caller is responsible for cropping away halo-edge values. Operations
/// whose implementation requires the complete frame fail closed when supplied
/// a partial region.
pub(crate) fn apply_render_op_f32_region_controlled<E>(
    working: &mut Vec<[f32; 4]>,
    region: EffectRasterRegion,
    op: &EffectRenderOp,
    frame_seed: i64,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    checkpoint()?;
    if !region.is_valid_for(working.len()) {
        return Ok(false);
    }
    let width = region.width;
    let height = region.height;
    match op {
        EffectRenderOp::ColorAdjust {
            exposure,
            contrast,
            saturation,
            working_color_space,
        } => {
            apply_primary_color_adjustments_f32_controlled(
                working.as_mut_slice(),
                AdjustmentLayerParams {
                    exposure: *exposure,
                    contrast: *contrast,
                    saturation: *saturation,
                    working_color_space: *working_color_space,
                    ..AdjustmentLayerParams::default()
                },
                checkpoint,
            )?;
            Ok(true)
        }
        EffectRenderOp::WhiteBalance { grade } => {
            apply_point_grade_rgba_f32_controlled(working, checkpoint, |rgb| grade.apply(rgb))?;
            Ok(true)
        }
        EffectRenderOp::Primaries { grade } => {
            apply_point_grade_rgba_f32_controlled(working, checkpoint, |rgb| grade.apply(rgb))?;
            Ok(true)
        }
        EffectRenderOp::HdrGrading { grade } => {
            apply_point_grade_rgba_f32_controlled(working, checkpoint, |rgb| grade.apply(rgb))?;
            Ok(true)
        }
        EffectRenderOp::AscCdl { grade } => {
            apply_point_grade_rgba_f32_controlled(working, checkpoint, |rgb| grade.apply(rgb))?;
            Ok(true)
        }
        EffectRenderOp::GamutCompression { grade } => {
            apply_point_grade_rgba_f32_controlled(working, checkpoint, |rgb| grade.apply(rgb))?;
            Ok(true)
        }
        EffectRenderOp::HighlightRecovery { grade } => {
            apply_point_grade_rgba_f32_controlled(working, checkpoint, |rgb| grade.apply(rgb))?;
            Ok(true)
        }
        EffectRenderOp::ColorCurves { curves } => {
            apply_point_grade_rgba_f32_controlled(working, checkpoint, |rgb| curves.apply(rgb))?;
            Ok(true)
        }
        EffectRenderOp::Qualifier { qualifier } => {
            crate::qualifier::apply_qualifier_rgba_f32_controlled(
                working.as_mut_slice(),
                width as usize,
                height as usize,
                qualifier,
                checkpoint,
            )?;
            Ok(true)
        }
        EffectRenderOp::MattePreview { invert } => {
            crate::qualifier::apply_matte_preview_rgba_f32_controlled(
                working.as_mut_slice(),
                *invert,
                checkpoint,
            )?;
            Ok(true)
        }
        EffectRenderOp::GaussianBlur { radius } => {
            let Some(radius) = valid_gaussian_radius(*radius) else {
                return Ok(false);
            };
            if radius > 1.0e-4 {
                gaussian_blur_rgba_f32_in_place_controlled(
                    working.as_mut_slice(),
                    width as usize,
                    height as usize,
                    radius,
                    checkpoint,
                )?;
            }
            Ok(true)
        }
        EffectRenderOp::Sharpen { amount } => {
            let amount = amount.clamp(0.0, 2.0);
            if amount > 1.0e-4 {
                let blurred = gaussian_blur_rgba_f32_controlled(
                    working,
                    width as usize,
                    height as usize,
                    crate::effect::SHARPEN_BLUR_RADIUS_PIXELS,
                    checkpoint,
                )?;
                apply_unsharp_mask_f32_controlled(working, &blurred, amount, checkpoint)?;
            }
            Ok(true)
        }
        EffectRenderOp::Vignette { intensity, feather } => {
            let intensity = intensity.clamp(0.0, 1.0);
            if intensity > 1.0e-4 {
                apply_vignette_f32_region_controlled(
                    working, region, intensity, *feather, checkpoint,
                )?;
            }
            Ok(true)
        }
        EffectRenderOp::ChromaticAberration { amount } => {
            if !region.is_full_frame() {
                return Ok(false);
            }
            let amount = amount.clamp(0.0, 1.0);
            if amount > 1.0e-4 {
                *working = apply_chromatic_aberration_f32_controlled(
                    working,
                    width as usize,
                    height as usize,
                    amount,
                    checkpoint,
                )?;
            }
            Ok(true)
        }
        EffectRenderOp::Grain { amount } => {
            let amount = amount.clamp(0.0, 1.0);
            if amount > 1.0e-4 {
                apply_grain_f32_region_controlled(working, region, amount, frame_seed, checkpoint)?;
            }
            Ok(true)
        }
        EffectRenderOp::Crop { left, top, right, bottom } => {
            let Some(bounds) = normalized_crop_bounds(
                region.frame_width,
                region.frame_height,
                *left,
                *top,
                *right,
                *bottom,
            ) else {
                return Ok(false);
            };
            apply_crop_rgba_f32_region_controlled(working, region, bounds, checkpoint)?;
            Ok(true)
        }
        EffectRenderOp::TemporalFrameBlend { .. } => Ok(false),
        EffectRenderOp::Lut3D { lut, intensity } => {
            lut.apply_rgba_f32_in_place_controlled(working, *intensity, checkpoint)?;
            Ok(true)
        }
        EffectRenderOp::Custom { .. } => Ok(false),
    }
}

#[derive(Debug, Clone, Copy)]
struct CropPixelBounds {
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
}

fn normalized_crop_bounds(
    width: u32,
    height: u32,
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
) -> Option<CropPixelBounds> {
    if width == 0 || height == 0 || ![left, top, right, bottom].into_iter().all(f32::is_finite) {
        return None;
    }
    Some(CropPixelBounds {
        left: left.clamp(0.0, 1.0) * width as f32,
        top: top.clamp(0.0, 1.0) * height as f32,
        right: (1.0 - right.clamp(0.0, 1.0)) * width as f32,
        bottom: (1.0 - bottom.clamp(0.0, 1.0)) * height as f32,
    })
}

fn crop_contains(bounds: CropPixelBounds, x: f32, y: f32) -> bool {
    x >= bounds.left && x < bounds.right && y >= bounds.top && y < bounds.bottom
}

fn apply_crop_rgba8(pixels: &mut [u8], width: u32, bounds: CropPixelBounds) {
    let width = width as usize;
    for (index, pixel) in pixels.chunks_exact_mut(4).enumerate() {
        let x = (index % width) as f32 + 0.5;
        let y = (index / width) as f32 + 0.5;
        if !crop_contains(bounds, x, y) {
            pixel.fill(0);
        }
    }
}

fn apply_crop_rgba_f32_region_controlled<E>(
    pixels: &mut [[f32; 4]],
    region: EffectRasterRegion,
    bounds: CropPixelBounds,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), E> {
    let row_width = region.width as usize;
    for (local_y, row) in pixels.chunks_exact_mut(row_width).enumerate() {
        checkpoint()?;
        let y = region.y as f32 + local_y as f32 + 0.5;
        for (local_x, pixel) in row.iter_mut().enumerate() {
            let x = region.x as f32 + local_x as f32 + 0.5;
            if !crop_contains(bounds, x, y) {
                *pixel = [0.0; 4];
            }
        }
    }
    Ok(())
}

pub fn blend_adjustment_result(
    base: &[u8],
    processed: &[u8],
    width: u32,
    height: u32,
    opacity: f32,
    blend_mode: Option<BlendMode>,
    out: &mut Vec<u8>,
) {
    let required_len = width as usize * height as usize * 4;
    if out.len() != required_len {
        out.resize(required_len, 0);
    }

    let opacity = opacity.clamp(0.0, 1.0);
    if !has_positive_coverage(opacity)
        || base.len() != required_len
        || processed.len() != required_len
    {
        out.copy_from_slice(base);
        return;
    }

    let mode = blend_mode.unwrap_or(BlendMode::Normal);
    for (i, ((base_px, processed_px), out_px)) in base
        .chunks_exact(4)
        .zip(processed.chunks_exact(4))
        .zip(out.chunks_exact_mut(4))
        .enumerate()
    {
        let blended = blend_rgba_pixel_seeded(
            [base_px[0], base_px[1], base_px[2], base_px[3]],
            [
                processed_px[0],
                processed_px[1],
                processed_px[2],
                processed_px[3],
            ],
            opacity,
            mode,
            i as u32,
        );
        out_px.copy_from_slice(&blended);
    }
}

pub fn blend_rgba_pixel(
    base_px: [u8; 4],
    blend_px: [u8; 4],
    opacity: f32,
    blend_mode: BlendMode,
) -> [u8; 4] {
    blend_rgba_pixel_seeded(base_px, blend_px, opacity, blend_mode, 0)
}

pub fn blend_rgba_pixel_seeded(
    base_px: [u8; 4],
    blend_px: [u8; 4],
    opacity: f32,
    blend_mode: BlendMode,
    dither_seed: u32,
) -> [u8; 4] {
    let opacity = opacity.clamp(0.0, 1.0);
    if !has_positive_coverage(opacity) {
        return base_px;
    }

    if blend_mode == BlendMode::Dissolve {
        let threshold = hash_u32(dither_seed) as f32 / u32::MAX as f32;
        if threshold > opacity {
            return base_px;
        }
        return blend_rgba_pixel_seeded(base_px, blend_px, 1.0, BlendMode::Normal, dither_seed);
    }

    let base_alpha = base_px[3] as f32 / 255.0;
    let blend_alpha = (blend_px[3] as f32 / 255.0) * opacity;
    if !has_positive_coverage(blend_alpha) {
        return base_px;
    }
    if !has_positive_coverage(base_alpha) {
        return [
            blend_px[0],
            blend_px[1],
            blend_px[2],
            unit_to_u8(blend_alpha),
        ];
    }

    let base_rgb = rgb_to_unit(&base_px);
    let blend_rgb = rgb_to_unit(&blend_px);
    let blended_rgb = blend_mode_rgb(blend_mode, base_rgb, blend_rgb);
    let out_alpha = blend_alpha + base_alpha * (1.0 - blend_alpha);
    if !has_positive_coverage(out_alpha) {
        return [0, 0, 0, 0];
    }

    let mut out = [0u8; 4];
    for channel in 0..3 {
        let premul = blended_rgb[channel] * blend_alpha
            + base_rgb[channel] * base_alpha * (1.0 - blend_alpha);
        out[channel] = unit_to_u8(premul / out_alpha);
    }
    out[3] = unit_to_u8(out_alpha);
    out
}

/// Blend one straight-alpha working-space `f32` RGBA pixel over another.
///
/// This mirrors the legacy RGBA8 blend semantics without quantizing the color
/// channels, so renderer float/linear paths can apply timeline blend modes
/// without crossing a temporary RGBA8 boundary.
pub fn blend_rgba_f32_pixel(
    base_px: [f32; 4],
    blend_px: [f32; 4],
    opacity: f32,
    blend_mode: BlendMode,
) -> [f32; 4] {
    blend_rgba_f32_pixel_seeded(base_px, blend_px, opacity, blend_mode, 0)
}

/// Blend one straight-alpha working-space `f32` RGBA pixel with a stable seed.
///
/// The seed is used by `BlendMode::Dissolve` to keep dither decisions stable
/// across preview/export paths that share the same frame and pixel identity.
pub fn blend_rgba_f32_pixel_seeded(
    base_px: [f32; 4],
    blend_px: [f32; 4],
    opacity: f32,
    blend_mode: BlendMode,
    dither_seed: u32,
) -> [f32; 4] {
    let opacity = opacity.clamp(0.0, 1.0);
    if !has_positive_coverage(opacity) {
        return base_px;
    }

    if blend_mode == BlendMode::Dissolve {
        let threshold = hash_u32(dither_seed) as f32 / u32::MAX as f32;
        if threshold > opacity {
            return base_px;
        }
        return blend_rgba_f32_pixel_seeded(base_px, blend_px, 1.0, BlendMode::Normal, dither_seed);
    }

    let base_alpha = base_px[3].clamp(0.0, 1.0);
    let blend_alpha = (blend_px[3] * opacity).clamp(0.0, 1.0);
    if !has_positive_coverage(blend_alpha) {
        return base_px;
    }
    if !has_positive_coverage(base_alpha) {
        return [blend_px[0], blend_px[1], blend_px[2], blend_alpha];
    }

    let base_rgb = [base_px[0], base_px[1], base_px[2]];
    let blend_rgb = [blend_px[0], blend_px[1], blend_px[2]];
    let blended_rgb = blend_mode_rgb(blend_mode, base_rgb, blend_rgb);
    let out_alpha = blend_alpha + base_alpha * (1.0 - blend_alpha);
    if !has_positive_coverage(out_alpha) {
        return [0.0, 0.0, 0.0, 0.0];
    }

    let mut out = [0.0f32; 4];
    for channel in 0..3 {
        let premul = blended_rgb[channel] * blend_alpha
            + base_rgb[channel] * base_alpha * (1.0 - blend_alpha);
        out[channel] = premul / out_alpha;
    }
    out[3] = out_alpha;
    out
}

fn hash_u32(mut x: u32) -> u32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846ca68b);
    x ^= x >> 16;
    x
}

pub fn apply_adjustment_pass(
    base: &[u8],
    width: u32,
    height: u32,
    params: AdjustmentLayerParams,
    opacity: f32,
    blend_mode: Option<BlendMode>,
    frame_seed: i64,
    out: &mut Vec<u8>,
) -> Result<(), EffectExecutionError> {
    let required_len = width as usize * height as usize * 4;
    if out.len() != required_len {
        out.resize(required_len, 0);
    }

    if required_len == 0 || base.len() != required_len {
        out.clear();
        return Ok(());
    }

    if !has_positive_coverage(opacity) || params.is_identity() {
        out.copy_from_slice(base);
        return Ok(());
    }

    let processed = apply_adjustment_layer(base, width, height, params, frame_seed)?;
    blend_adjustment_result(base, &processed, width, height, opacity, blend_mode, out);
    Ok(())
}

fn apply_primary_color_adjustments(buffer: &mut [u8], params: AdjustmentLayerParams) {
    let exposure_scale = 2.0f32.powf(params.exposure.clamp(-4.0, 4.0));
    let contrast = params.contrast.clamp(0.0, 3.0);
    let saturation = params.saturation.clamp(0.0, 3.0);
    let luma_coefficients = params.working_color_space.luminance_coefficients();
    const CONTRAST_PIVOT: f32 = 0.18;

    for px in buffer.chunks_exact_mut(4) {
        if px[3] == 0 {
            continue;
        }

        let mut rgb = rgb_to_unit(px);
        for channel in &mut rgb {
            *channel = (*channel * exposure_scale).clamp(0.0, 1.0);
            *channel = ((*channel - CONTRAST_PIVOT) * contrast + CONTRAST_PIVOT).clamp(0.0, 1.0);
        }

        let luma = dot3(rgb, luma_coefficients);
        rgb[0] = (luma + (rgb[0] - luma) * saturation).clamp(0.0, 1.0);
        rgb[1] = (luma + (rgb[1] - luma) * saturation).clamp(0.0, 1.0);
        rgb[2] = (luma + (rgb[2] - luma) * saturation).clamp(0.0, 1.0);

        px[0] = unit_to_u8(rgb[0]);
        px[1] = unit_to_u8(rgb[1]);
        px[2] = unit_to_u8(rgb[2]);
    }
}

fn apply_point_grade_rgba8(buffer: &mut [u8], grade: impl Fn([f32; 3]) -> [f32; 3]) {
    for pixel in buffer.chunks_exact_mut(4) {
        if pixel[3] == 0 {
            continue;
        }
        let rgb = grade(rgb_to_unit(pixel));
        pixel[0] = unit_to_u8(rgb[0]);
        pixel[1] = unit_to_u8(rgb[1]);
        pixel[2] = unit_to_u8(rgb[2]);
    }
}

fn apply_point_grade_rgba_f32_controlled<E>(
    buffer: &mut [[f32; 4]],
    checkpoint: &mut impl FnMut() -> Result<(), E>,
    grade: impl Fn([f32; 3]) -> [f32; 3],
) -> Result<(), E> {
    for chunk in buffer.chunks_mut(4_096) {
        checkpoint()?;
        for pixel in chunk {
            if !has_positive_coverage(pixel[3].clamp(0.0, 1.0)) {
                continue;
            }
            let rgb = grade([pixel[0], pixel[1], pixel[2]]);
            pixel[0] = rgb[0];
            pixel[1] = rgb[1];
            pixel[2] = rgb[2];
        }
    }
    checkpoint()
}

fn apply_primary_color_adjustments_f32_controlled<E>(
    buffer: &mut [[f32; 4]],
    params: AdjustmentLayerParams,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), E> {
    let exposure_scale = 2.0f32.powf(params.exposure.clamp(-4.0, 4.0));
    let contrast = params.contrast.clamp(0.0, 3.0);
    let saturation = params.saturation.clamp(0.0, 3.0);
    let luma_coefficients = params.working_color_space.luminance_coefficients();
    const CONTRAST_PIVOT: f32 = 0.18;

    for chunk in buffer.chunks_mut(4_096) {
        checkpoint()?;
        for px in chunk {
            if !has_positive_coverage(px[3].clamp(0.0, 1.0)) {
                continue;
            }

            let mut rgb = [px[0], px[1], px[2]];
            for channel in &mut rgb {
                *channel *= exposure_scale;
                *channel = (*channel - CONTRAST_PIVOT) * contrast + CONTRAST_PIVOT;
            }

            let luma = dot3(rgb, luma_coefficients);
            px[0] = luma + (rgb[0] - luma) * saturation;
            px[1] = luma + (rgb[1] - luma) * saturation;
            px[2] = luma + (rgb[2] - luma) * saturation;
        }
    }
    checkpoint()
}

fn dot3(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn valid_gaussian_radius(radius: f32) -> Option<f32> {
    (radius.is_finite()
        && (0.0..=crate::effect::GAUSSIAN_BLUR_AUTHOR_MAX_RADIUS_PIXELS as f32).contains(&radius))
    .then_some(radius)
}

fn gaussian_blur_rgba8(input: &[u8], width: usize, height: usize, radius: f32) -> Vec<u8> {
    let required_len = width.saturating_mul(height).saturating_mul(4);
    if radius <= 1.0e-4 || required_len == 0 || input.len() != required_len {
        return input.to_vec();
    }

    let float_input = input
        .chunks_exact(4)
        .map(|pixel| {
            [
                pixel[0] as f32 / 255.0,
                pixel[1] as f32 / 255.0,
                pixel[2] as f32 / 255.0,
                pixel[3] as f32 / 255.0,
            ]
        })
        .collect::<Vec<_>>();
    gaussian_blur_rgba_f32(&float_input, width, height, radius)
        .into_iter()
        .flat_map(|pixel| pixel.map(unit_to_u8))
        .collect()
}

fn gaussian_blur_rgba_f32(
    input: &[[f32; 4]],
    width: usize,
    height: usize,
    radius: f32,
) -> Vec<[f32; 4]> {
    match gaussian_blur_rgba_f32_controlled(input, width, height, radius, &mut || {
        Ok::<(), Infallible>(())
    }) {
        Ok(output) => output,
        Err(never) => match never {},
    }
}

fn gaussian_blur_rgba_f32_controlled<E>(
    input: &[[f32; 4]],
    width: usize,
    height: usize,
    radius: f32,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<Vec<[f32; 4]>, E> {
    checkpoint()?;
    let mut output = input.to_vec();
    checkpoint()?;
    gaussian_blur_rgba_f32_in_place_controlled(
        output.as_mut_slice(),
        width,
        height,
        radius,
        checkpoint,
    )?;
    Ok(output)
}

fn gaussian_blur_rgba_f32_in_place_controlled<E>(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    radius: f32,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), E> {
    checkpoint()?;
    if radius <= 1.0e-4 || width == 0 || height == 0 || pixels.len() != width * height {
        return Ok(());
    }

    for chunk in pixels.chunks_mut(4_096) {
        checkpoint()?;
        for pixel in chunk {
            let alpha = pixel[3].clamp(0.0, 1.0);
            pixel[0] *= alpha;
            pixel[1] *= alpha;
            pixel[2] *= alpha;
            pixel[3] = alpha;
        }
    }
    let mut scratch = vec![[0.0; 4]; pixels.len()];
    checkpoint()?;

    let kernel = gaussian_fractional_box_kernel(radius);
    for _ in 0..GAUSSIAN_BOX_PASS_COUNT {
        apply_fractional_box_blur_pass_controlled(
            pixels,
            &mut scratch,
            width,
            height,
            kernel,
            checkpoint,
        )?;
    }

    unpremultiply_rgba_controlled(pixels, checkpoint)
}

#[derive(Debug, Clone, Copy)]
struct FractionalBoxKernel {
    whole_radius: usize,
    edge_weight: f32,
}

fn gaussian_fractional_box_kernel(radius: f32) -> FractionalBoxKernel {
    let sigma = radius / 3.0;
    let target_pass_variance = sigma * sigma / GAUSSIAN_BOX_PASS_COUNT as f32;
    let whole_radius = (((1.0 + 12.0 * target_pass_variance).sqrt() - 1.0) * 0.5).floor() as usize;
    let whole = whole_radius as f32;
    let whole_weight = whole.mul_add(2.0, 1.0);
    let whole_second_moment = whole * (whole + 1.0) * whole.mul_add(2.0, 1.0) / 3.0;
    let edge_distance = whole + 1.0;
    let denominator = 2.0 * (edge_distance * edge_distance - target_pass_variance);
    let edge_weight = if denominator > f32::EPSILON {
        ((target_pass_variance * whole_weight - whole_second_moment) / denominator).clamp(0.0, 1.0)
    } else {
        0.0
    };
    FractionalBoxKernel { whole_radius, edge_weight }
}

/// Exact one-axis input halo consumed by the current three-pass Gaussian
/// implementation.
///
/// ROI contracts call this same kernel function instead of approximating the
/// dependency from the author-facing radius. Fractional outer taps extend the
/// finite support even when their weight is below one.
pub(crate) fn gaussian_blur_input_halo(radius: f32) -> Option<u32> {
    let radius = valid_gaussian_radius(radius)?;
    if radius <= 1.0e-4 {
        return Some(0);
    }
    let kernel = gaussian_fractional_box_kernel(radius);
    if kernel.whole_radius == 0 && kernel.edge_weight <= f32::EPSILON {
        return Some(0);
    }
    let per_pass = kernel.whole_radius.checked_add(usize::from(kernel.edge_weight > 0.0))?;
    u32::try_from(per_pass.checked_mul(GAUSSIAN_BOX_PASS_COUNT)?).ok()
}

fn apply_fractional_box_blur_pass_controlled<E>(
    pixels: &mut [[f32; 4]],
    scratch: &mut [[f32; 4]],
    width: usize,
    height: usize,
    kernel: FractionalBoxKernel,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), E> {
    let radius = kernel.whole_radius;
    let edge_weight = kernel.edge_weight;
    if radius == 0 && edge_weight <= f32::EPSILON {
        return checkpoint();
    }

    // Keep the sliding accumulator wider than the stored frame. ROI execution
    // deliberately replaces pixels outside the exact finite halo with zero.
    // A Float32 accumulator can retain cancellation residue from those
    // irrelevant pixels and make a cropped result differ from the same pixels
    // in a full-frame execution. Float64 accumulation keeps that residue below
    // the final Float32 rounding threshold without changing the public working
    // precision or the admitted halo.
    let normalization =
        1.0 / (radius.saturating_mul(2).saturating_add(1) as f64 + 2.0 * f64::from(edge_weight));
    for y in 0..height {
        checkpoint()?;
        let row_start = y * width;
        let mut sum = [0.0_f64; 4];
        for offset in -(radius as isize)..=radius as isize {
            let sample_x = offset.clamp(0, width as isize - 1) as usize;
            add_rgba(&mut sum, pixels[row_start + sample_x]);
        }
        for x in 0..width {
            let left_edge =
                (x as isize - radius as isize - 1).clamp(0, width as isize - 1) as usize;
            let right_edge =
                (x as isize + radius as isize + 1).clamp(0, width as isize - 1) as usize;
            let mut weighted = sum;
            add_scaled_rgba(&mut weighted, pixels[row_start + left_edge], edge_weight);
            add_scaled_rgba(&mut weighted, pixels[row_start + right_edge], edge_weight);
            scratch[row_start + x] = weighted.map(|channel| (channel * normalization) as f32);
            let remove_x = (x as isize - radius as isize).clamp(0, width as isize - 1) as usize;
            let add_x = (x as isize + radius as isize + 1).clamp(0, width as isize - 1) as usize;
            subtract_rgba(&mut sum, pixels[row_start + remove_x]);
            add_rgba(&mut sum, pixels[row_start + add_x]);
        }
    }

    for x in 0..width {
        checkpoint()?;
        let mut sum = [0.0_f64; 4];
        for offset in -(radius as isize)..=radius as isize {
            let sample_y = offset.clamp(0, height as isize - 1) as usize;
            add_rgba(&mut sum, scratch[sample_y * width + x]);
        }
        for y in 0..height {
            let upper_edge =
                (y as isize - radius as isize - 1).clamp(0, height as isize - 1) as usize;
            let lower_edge =
                (y as isize + radius as isize + 1).clamp(0, height as isize - 1) as usize;
            let mut weighted = sum;
            add_scaled_rgba(&mut weighted, scratch[upper_edge * width + x], edge_weight);
            add_scaled_rgba(&mut weighted, scratch[lower_edge * width + x], edge_weight);
            pixels[y * width + x] = weighted.map(|channel| (channel * normalization) as f32);
            let remove_y = (y as isize - radius as isize).clamp(0, height as isize - 1) as usize;
            let add_y = (y as isize + radius as isize + 1).clamp(0, height as isize - 1) as usize;
            subtract_rgba(&mut sum, scratch[remove_y * width + x]);
            add_rgba(&mut sum, scratch[add_y * width + x]);
        }
    }
    checkpoint()
}

fn add_rgba(sum: &mut [f64; 4], pixel: [f32; 4]) {
    for channel in 0..4 {
        sum[channel] += f64::from(pixel[channel]);
    }
}

fn add_scaled_rgba(sum: &mut [f64; 4], pixel: [f32; 4], scale: f32) {
    for channel in 0..4 {
        sum[channel] += f64::from(pixel[channel]) * f64::from(scale);
    }
}

fn subtract_rgba(sum: &mut [f64; 4], pixel: [f32; 4]) {
    for channel in 0..4 {
        sum[channel] -= f64::from(pixel[channel]);
    }
}

fn unpremultiply_rgba_controlled<E>(
    pixels: &mut [[f32; 4]],
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), E> {
    for chunk in pixels.chunks_mut(4_096) {
        checkpoint()?;
        for pixel in chunk {
            *pixel = straight_rgba_from_premultiplied(*pixel);
        }
    }
    checkpoint()
}

fn apply_unsharp_mask_f32_controlled<E>(
    buffer: &mut [[f32; 4]],
    blurred: &[[f32; 4]],
    amount: f32,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), E> {
    for (buffer_chunk, blurred_chunk) in buffer.chunks_mut(4_096).zip(blurred.chunks(4_096)) {
        checkpoint()?;
        for (pixel, softened) in buffer_chunk.iter_mut().zip(blurred_chunk) {
            if !has_positive_coverage(pixel[3].clamp(0.0, 1.0)) {
                continue;
            }
            for channel in 0..3 {
                pixel[channel] += (pixel[channel] - softened[channel]) * amount;
            }
        }
    }
    checkpoint()
}

fn apply_vignette_f32_region_controlled<E>(
    buffer: &mut [[f32; 4]],
    region: EffectRasterRegion,
    intensity: f32,
    feather: f32,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), E> {
    let width = region.width as usize;
    let height = region.height as usize;
    let center_x = region.frame_width.saturating_sub(1) as f32 * 0.5;
    let center_y = region.frame_height.saturating_sub(1) as f32 * 0.5;
    let feather = feather.clamp(0.05, 1.0);
    let inner = 1.0 - feather * 0.85;

    for y in 0..height {
        checkpoint()?;
        for x in 0..width {
            let pixel = &mut buffer[y * width + x];
            if !has_positive_coverage(pixel[3].clamp(0.0, 1.0)) {
                continue;
            }
            let frame_x = region.x as f32 + x as f32;
            let frame_y = region.y as f32 + y as f32;
            let normalized_x = (frame_x - center_x) / center_x.max(1.0);
            let normalized_y = (frame_y - center_y) / center_y.max(1.0);
            let distance =
                (normalized_x * normalized_x + normalized_y * normalized_y).sqrt().min(1.0);
            let gain = 1.0 - smoothstep(inner, 1.0, distance) * intensity;
            pixel[0] *= gain;
            pixel[1] *= gain;
            pixel[2] *= gain;
        }
    }
    checkpoint()
}

fn apply_chromatic_aberration_f32_controlled<E>(
    input: &[[f32; 4]],
    width: usize,
    height: usize,
    amount: f32,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<Vec<[f32; 4]>, E> {
    let mut output = input.to_vec();
    checkpoint()?;
    let center_x = width.saturating_sub(1) as f32 * 0.5;
    let center_y = height.saturating_sub(1) as f32 * 0.5;
    let maximum_shift = amount * 5.0;

    for y in 0..height {
        checkpoint()?;
        for x in 0..width {
            let index = y * width + x;
            if !has_positive_coverage(input[index][3].clamp(0.0, 1.0)) {
                continue;
            }
            let dx = x as f32 - center_x;
            let dy = y as f32 - center_y;
            let distance = ((dx * dx + dy * dy).sqrt() / center_x.max(center_y).max(1.0)).min(1.0);
            let shift = maximum_shift * distance;
            output[index][0] = sample_premultiplied_channel_f32(
                input,
                width,
                height,
                x as f32 + shift,
                y as f32,
                0,
            );
            output[index][1] =
                sample_premultiplied_channel_f32(input, width, height, x as f32, y as f32, 1);
            output[index][2] = sample_premultiplied_channel_f32(
                input,
                width,
                height,
                x as f32 - shift,
                y as f32,
                2,
            );
        }
    }
    checkpoint()?;
    Ok(output)
}

fn sample_premultiplied_channel_f32(
    input: &[[f32; 4]],
    width: usize,
    height: usize,
    x: f32,
    y: f32,
    channel: usize,
) -> f32 {
    let x = x.clamp(0.0, width.saturating_sub(1) as f32);
    let y = y.clamp(0.0, height.saturating_sub(1) as f32);
    let x0 = x.floor() as usize;
    let y0 = y.floor() as usize;
    let x1 = (x0 + 1).min(width.saturating_sub(1));
    let y1 = (y0 + 1).min(height.saturating_sub(1));
    let fraction_x = x - x0 as f32;
    let fraction_y = y - y0 as f32;
    let sample = |sample_x: usize, sample_y: usize| {
        let pixel = input[sample_y * width + sample_x];
        let alpha = pixel[3].clamp(0.0, 1.0);
        (pixel[channel] * alpha, alpha)
    };
    let (top_left, alpha_top_left) = sample(x0, y0);
    let (top_right, alpha_top_right) = sample(x1, y0);
    let (bottom_left, alpha_bottom_left) = sample(x0, y1);
    let (bottom_right, alpha_bottom_right) = sample(x1, y1);
    let interpolate = |a: f32, b: f32, t: f32| a + (b - a) * t;
    let premultiplied = interpolate(
        interpolate(top_left, top_right, fraction_x),
        interpolate(bottom_left, bottom_right, fraction_x),
        fraction_y,
    );
    let alpha = interpolate(
        interpolate(alpha_top_left, alpha_top_right, fraction_x),
        interpolate(alpha_bottom_left, alpha_bottom_right, fraction_x),
        fraction_y,
    );
    if has_positive_coverage(alpha) {
        premultiplied / alpha
    } else {
        0.0
    }
}

fn apply_grain_f32_region_controlled<E>(
    buffer: &mut [[f32; 4]],
    region: EffectRasterRegion,
    amount: f32,
    frame_seed: i64,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), E> {
    let width = region.width as usize;
    let height = region.height as usize;
    for y in 0..height {
        checkpoint()?;
        for x in 0..width {
            let pixel = &mut buffer[y * width + x];
            if !has_positive_coverage(pixel[3].clamp(0.0, 1.0)) {
                continue;
            }
            let noise =
                grain_noise(region.x + x as u32, region.y + y as u32, frame_seed) * amount * 0.18;
            pixel[0] += noise;
            pixel[1] += noise;
            pixel[2] += noise;
        }
    }
    checkpoint()
}

fn box_blur_rgb(input: &[u8], width: usize, height: usize, radius: usize) -> Vec<u8> {
    if radius == 0 || width == 0 || height == 0 {
        return input.to_vec();
    }

    let mut horizontal = input.to_vec();
    let mut output = input.to_vec();

    for y in 0..height {
        for channel in 0..3 {
            let mut sum = 0u32;
            let mut count = 0u32;
            for dx in 0..=radius.min(width.saturating_sub(1)) {
                sum += input[(y * width + dx) * 4 + channel] as u32;
                count += 1;
            }

            for x in 0..width {
                horizontal[(y * width + x) * 4 + channel] = (sum / count.max(1)) as u8;
                let remove_x = x.saturating_sub(radius);
                let add_x = x + radius + 1;
                if x >= radius {
                    sum = sum.saturating_sub(input[(y * width + remove_x) * 4 + channel] as u32);
                    count = count.saturating_sub(1);
                }
                if add_x < width {
                    sum += input[(y * width + add_x) * 4 + channel] as u32;
                    count += 1;
                }
            }
        }
    }

    for x in 0..width {
        for channel in 0..3 {
            let mut sum = 0u32;
            let mut count = 0u32;
            for dy in 0..=radius.min(height.saturating_sub(1)) {
                sum += horizontal[(dy * width + x) * 4 + channel] as u32;
                count += 1;
            }

            for y in 0..height {
                output[(y * width + x) * 4 + channel] = (sum / count.max(1)) as u8;
                let remove_y = y.saturating_sub(radius);
                let add_y = y + radius + 1;
                if y >= radius {
                    sum =
                        sum.saturating_sub(horizontal[(remove_y * width + x) * 4 + channel] as u32);
                    count = count.saturating_sub(1);
                }
                if add_y < height {
                    sum += horizontal[(add_y * width + x) * 4 + channel] as u32;
                    count += 1;
                }
            }
        }
    }

    for (src_px, out_px) in input.chunks_exact(4).zip(output.chunks_exact_mut(4)) {
        out_px[3] = src_px[3];
    }
    output
}

fn apply_unsharp_mask(buffer: &mut [u8], blurred: &[u8], amount: f32) {
    for (px, blur_px) in buffer.chunks_exact_mut(4).zip(blurred.chunks_exact(4)) {
        if px[3] == 0 {
            continue;
        }
        for channel in 0..3 {
            let original = px[channel] as f32 / 255.0;
            let softened = blur_px[channel] as f32 / 255.0;
            let enhanced = (original + (original - softened) * amount).clamp(0.0, 1.0);
            px[channel] = unit_to_u8(enhanced);
        }
    }
}

fn apply_chromatic_aberration(input: &[u8], width: usize, height: usize, amount: f32) -> Vec<u8> {
    let mut output = input.to_vec();
    let cx = (width.saturating_sub(1)) as f32 * 0.5;
    let cy = (height.saturating_sub(1)) as f32 * 0.5;
    let max_shift = 1.0 + amount * 4.0;

    for y in 0..height {
        for x in 0..width {
            let idx = (y * width + x) * 4;
            if input[idx + 3] == 0 {
                continue;
            }

            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let dist = ((dx * dx + dy * dy).sqrt() / cx.max(cy).max(1.0)).clamp(0.0, 1.0);
            let shift = max_shift * dist;
            let rx = sample_channel(input, width, height, x as f32 + shift, y as f32, 0);
            let gx = sample_channel(input, width, height, x as f32, y as f32, 1);
            let bx = sample_channel(input, width, height, x as f32 - shift, y as f32, 2);
            output[idx] = rx;
            output[idx + 1] = gx;
            output[idx + 2] = bx;
        }
    }

    output
}

fn apply_vignette(buffer: &mut [u8], width: usize, height: usize, params: AdjustmentLayerParams) {
    let cx = (width.saturating_sub(1)) as f32 * 0.5;
    let cy = (height.saturating_sub(1)) as f32 * 0.5;
    let feather = params.vignette_feather.clamp(0.05, 1.0);
    let inner = 1.0 - feather * 0.85;
    let intensity = params.vignette_intensity.clamp(0.0, 1.0);

    for y in 0..height {
        for x in 0..width {
            let idx = (y * width + x) * 4;
            if buffer[idx + 3] == 0 {
                continue;
            }

            let nx = (x as f32 - cx) / cx.max(1.0);
            let ny = (y as f32 - cy) / cy.max(1.0);
            let dist = (nx * nx + ny * ny).sqrt().clamp(0.0, 1.0);
            let edge = smoothstep(inner, 1.0, dist);
            let gain = 1.0 - edge * intensity;
            buffer[idx] = unit_to_u8((buffer[idx] as f32 / 255.0) * gain);
            buffer[idx + 1] = unit_to_u8((buffer[idx + 1] as f32 / 255.0) * gain);
            buffer[idx + 2] = unit_to_u8((buffer[idx + 2] as f32 / 255.0) * gain);
        }
    }
}

fn apply_grain(buffer: &mut [u8], width: usize, height: usize, amount: f32, frame_seed: i64) {
    for y in 0..height {
        for x in 0..width {
            let idx = (y * width + x) * 4;
            if buffer[idx + 3] == 0 {
                continue;
            }
            let noise = grain_noise(x as u32, y as u32, frame_seed) * amount * 0.18;
            for channel in 0..3 {
                let value = (buffer[idx + channel] as f32 / 255.0 + noise).clamp(0.0, 1.0);
                buffer[idx + channel] = unit_to_u8(value);
            }
        }
    }
}

fn blend_mode_rgb(mode: BlendMode, base: [f32; 3], blend: [f32; 3]) -> [f32; 3] {
    match mode {
        BlendMode::Hue => set_lum(set_sat(blend, sat(base)), lum(base)),
        BlendMode::Saturation => set_lum(set_sat(base, sat(blend)), lum(base)),
        BlendMode::Color => set_lum(blend, lum(base)),
        BlendMode::Luminosity => set_lum(base, lum(blend)),
        BlendMode::DarkerColor => {
            if lum(blend) < lum(base) {
                blend
            } else {
                base
            }
        }
        BlendMode::LighterColor => {
            if lum(blend) > lum(base) {
                blend
            } else {
                base
            }
        }
        _ => [
            blend_mode_channel(mode, base[0], blend[0]),
            blend_mode_channel(mode, base[1], blend[1]),
            blend_mode_channel(mode, base[2], blend[2]),
        ],
    }
}

fn blend_mode_channel(mode: BlendMode, base: f32, blend: f32) -> f32 {
    match mode {
        BlendMode::Normal | BlendMode::Dissolve => blend,
        BlendMode::Multiply => base * blend,
        BlendMode::Screen => 1.0 - (1.0 - base) * (1.0 - blend),
        BlendMode::Overlay => {
            if base <= 0.5 {
                2.0 * base * blend
            } else {
                1.0 - 2.0 * (1.0 - base) * (1.0 - blend)
            }
        }
        BlendMode::Darken => base.min(blend),
        BlendMode::Lighten => base.max(blend),
        BlendMode::ColorDodge => {
            if blend >= 0.999 {
                1.0
            } else {
                (base / (1.0 - blend)).clamp(0.0, 1.0)
            }
        }
        BlendMode::ColorBurn => {
            if blend <= 0.001 {
                0.0
            } else {
                (1.0 - (1.0 - base) / blend).clamp(0.0, 1.0)
            }
        }
        BlendMode::HardLight => {
            if blend <= 0.5 {
                2.0 * base * blend
            } else {
                1.0 - 2.0 * (1.0 - base) * (1.0 - blend)
            }
        }
        BlendMode::SoftLight => {
            if blend <= 0.5 {
                base - (1.0 - 2.0 * blend) * base * (1.0 - base)
            } else {
                let d = if base <= 0.25 {
                    ((16.0 * base - 12.0) * base + 4.0) * base
                } else {
                    base.sqrt()
                };
                base + (2.0 * blend - 1.0) * (d - base)
            }
        }
        BlendMode::Difference => (base - blend).abs(),
        BlendMode::Exclusion => base + blend - 2.0 * base * blend,
        BlendMode::LinearDodge => (base + blend).clamp(0.0, 1.0),
        BlendMode::Subtract => (base - blend).clamp(0.0, 1.0),
        BlendMode::Divide => {
            if blend <= 0.001 {
                1.0
            } else {
                (base / blend).clamp(0.0, 1.0)
            }
        }
        BlendMode::LinearBurn => (base + blend - 1.0).clamp(0.0, 1.0),
        BlendMode::VividLight => {
            if blend <= 0.5 {
                if blend <= 0.001 {
                    0.0
                } else {
                    (1.0 - (1.0 - base) / (2.0 * blend)).clamp(0.0, 1.0)
                }
            } else if blend >= 0.999 {
                1.0
            } else {
                (base / (2.0 * (1.0 - blend))).clamp(0.0, 1.0)
            }
        }
        BlendMode::LinearLight => (base + 2.0 * blend - 1.0).clamp(0.0, 1.0),
        BlendMode::PinLight => {
            if blend <= 0.5 {
                base.min(2.0 * blend)
            } else {
                base.max(2.0 * (blend - 0.5))
            }
        }
        BlendMode::HardMix => {
            if base + blend < 1.0 {
                0.0
            } else {
                1.0
            }
        }
        // Non-separable modes handled in blend_mode_rgb, unreachable here
        BlendMode::Hue
        | BlendMode::Saturation
        | BlendMode::Color
        | BlendMode::Luminosity
        | BlendMode::DarkerColor
        | BlendMode::LighterColor => blend,
    }
}

fn lum(c: [f32; 3]) -> f32 {
    0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2]
}

fn clip_color(c: [f32; 3]) -> [f32; 3] {
    let l = lum(c);
    let n = c[0].min(c[1]).min(c[2]);
    let x = c[0].max(c[1]).max(c[2]);
    if n < 0.0 {
        let t = l / (l - n);
        [
            (l + (c[0] - l) * t),
            (l + (c[1] - l) * t),
            (l + (c[2] - l) * t),
        ]
    } else if x > 1.0 {
        let t = (1.0 - l) / (x - l);
        [
            (l + (c[0] - l) * t),
            (l + (c[1] - l) * t),
            (l + (c[2] - l) * t),
        ]
    } else {
        c
    }
}

fn set_lum(c: [f32; 3], target_lum: f32) -> [f32; 3] {
    let d = target_lum - lum(c);
    clip_color([c[0] + d, c[1] + d, c[2] + d])
}

fn sat(c: [f32; 3]) -> f32 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}

fn set_sat(c: [f32; 3], target_sat: f32) -> [f32; 3] {
    let mut result = c;
    let mut idx: [usize; 3] = [0, 1, 2];
    idx.sort_by(|&a, &b| c[a].partial_cmp(&c[b]).unwrap_or(std::cmp::Ordering::Equal));
    let min_ch = idx[0];
    let mid_ch = idx[1];
    let max_ch = idx[2];
    if c[max_ch] > c[min_ch] {
        result[mid_ch] = (c[mid_ch] - c[min_ch]) * target_sat / (c[max_ch] - c[min_ch]);
        result[max_ch] = target_sat;
        result[min_ch] = 0.0;
    } else {
        result = [0.0, 0.0, 0.0];
    }
    result
}

fn rgb_to_unit(px: &[u8]) -> [f32; 3] {
    [
        px[0] as f32 / 255.0,
        px[1] as f32 / 255.0,
        px[2] as f32 / 255.0,
    ]
}

pub(crate) fn unit_to_u8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0).max(1.0e-5)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn grain_noise(x: u32, y: u32, frame_seed: i64) -> f32 {
    let mut value = x
        .wrapping_mul(1973)
        .wrapping_add(y.wrapping_mul(9277))
        .wrapping_add((frame_seed as u32).wrapping_mul(26699))
        .wrapping_add(0x68bc_21eb);
    value ^= value << 13;
    value ^= value >> 17;
    value ^= value << 5;
    (value as f32 / u32::MAX as f32) * 2.0 - 1.0
}

fn sample_channel(input: &[u8], width: usize, height: usize, x: f32, y: f32, channel: usize) -> u8 {
    let sx = x.round().clamp(0.0, width.saturating_sub(1) as f32) as usize;
    let sy = y.round().clamp(0.0, height.saturating_sub(1) as f32) as usize;
    input[(sy * width + sx) * 4 + channel]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        compile_reference_effect_graph, EffectGraphNode, EffectGraphNodeId, EffectGraphNodeKind,
        EffectRenderGraph, EffectRenderPlan,
    };
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    #[test]
    fn identity_adjustment_leaves_frame_unchanged() {
        let input = vec![32, 64, 96, 255, 12, 24, 36, 255];
        let output = apply_adjustment_layer(&input, 2, 1, AdjustmentLayerParams::default(), 0)
            .expect("identity adjustment");
        assert_eq!(output, input);
    }

    #[test]
    fn normal_blend_with_full_opacity_uses_processed_pixels() {
        let base = vec![10, 20, 30, 255];
        let processed = vec![200, 150, 100, 255];
        let mut out = Vec::new();
        blend_adjustment_result(
            &base,
            &processed,
            1,
            1,
            1.0,
            Some(BlendMode::Normal),
            &mut out,
        );
        assert_eq!(out, processed);
    }

    #[test]
    fn multiply_blend_darkens_pixels() {
        let base = vec![128, 128, 128, 255];
        let processed = vec![128, 64, 255, 255];
        let mut out = Vec::new();
        blend_adjustment_result(
            &base,
            &processed,
            1,
            1,
            1.0,
            Some(BlendMode::Multiply),
            &mut out,
        );
        assert!(out[0] < base[0]);
        assert!(out[1] < base[1]);
        assert_eq!(out[3], 255);
    }

    #[test]
    fn f32_pixel_blend_matches_rgba8_multiply_semantics() {
        let base_u8 = [128, 96, 64, 255];
        let blend_u8 = [64, 192, 128, 255];
        let rgba8 = blend_rgba_pixel_seeded(base_u8, blend_u8, 0.75, BlendMode::Multiply, 0);
        let f32 = blend_rgba_f32_pixel(
            [
                base_u8[0] as f32 / 255.0,
                base_u8[1] as f32 / 255.0,
                base_u8[2] as f32 / 255.0,
                base_u8[3] as f32 / 255.0,
            ],
            [
                blend_u8[0] as f32 / 255.0,
                blend_u8[1] as f32 / 255.0,
                blend_u8[2] as f32 / 255.0,
                blend_u8[3] as f32 / 255.0,
            ],
            0.75,
            BlendMode::Multiply,
        );

        for channel in 0..4 {
            let actual = (f32[channel].clamp(0.0, 1.0) * 255.0).round() as u8;
            assert!((actual as i16 - rgba8[channel] as i16).abs() <= 1);
        }
    }

    #[test]
    fn normal_source_over_preserves_sixteen_bit_and_smaller_positive_coverage() {
        fn f64_reference(base: [f32; 4], source: [f32; 4], opacity: f32) -> [f64; 4] {
            let base_alpha = f64::from(base[3].clamp(0.0, 1.0));
            let source_alpha =
                f64::from(source[3].clamp(0.0, 1.0)) * f64::from(opacity.clamp(0.0, 1.0));
            let alpha = source_alpha + base_alpha * (1.0 - source_alpha);
            if alpha == 0.0 {
                return [0.0; 4];
            }
            [
                (f64::from(source[0]) * source_alpha
                    + f64::from(base[0]) * base_alpha * (1.0 - source_alpha))
                    / alpha,
                (f64::from(source[1]) * source_alpha
                    + f64::from(base[1]) * base_alpha * (1.0 - source_alpha))
                    / alpha,
                (f64::from(source[2]) * source_alpha
                    + f64::from(base[2]) * base_alpha * (1.0 - source_alpha))
                    / alpha,
                alpha,
            ]
        }

        let corpus = [
            ([0.0; 4], [1.25, -0.25, 0.5, 1.0 / 65_535.0], 1.0),
            (
                [0.2, 0.4, 0.6, 1.0 / 32_768.0],
                [1.5, -0.5, 0.125, 1.0 / 65_535.0],
                0.375,
            ),
            ([0.1, 0.3, 0.7, 1.0], [2.0, -1.0, 0.5, 1.0], 1.0e-8),
        ];

        for (base, source, opacity) in corpus {
            let actual = blend_rgba_f32_pixel(base, source, opacity, BlendMode::Normal);
            let expected = f64_reference(base, source, opacity);
            for channel in 0..4 {
                let error = (f64::from(actual[channel]) - expected[channel]).abs();
                assert!(
                    error <= 8.0 * f64::from(f32::EPSILON) * expected[channel].abs().max(1.0),
                    "channel {channel}: expected {}, got {}, error {error}",
                    expected[channel],
                    actual[channel]
                );
            }
        }
    }

    #[test]
    fn repeated_sixteen_bit_edges_accumulate_instead_of_disappearing() {
        const LAYERS: usize = 4_096;
        let edge_alpha = 1.0 / 65_535.0;
        let source = [1.25, -0.25, 0.5, edge_alpha];
        let mut actual = [0.0; 4];
        for _ in 0..LAYERS {
            actual = blend_rgba_f32_pixel(actual, source, 1.0, BlendMode::Normal);
        }

        let expected_alpha = 1.0 - (1.0 - f64::from(edge_alpha)).powi(LAYERS as i32);
        assert!((f64::from(actual[3]) - expected_alpha).abs() <= 2.0e-5);
        for channel in 0..3 {
            assert!((actual[channel] - source[channel]).abs() <= 2.0e-5);
        }
        assert!(
            actual[3] > 0.06,
            "positive edges must accumulate: {actual:?}"
        );
    }

    #[test]
    fn dissolve_gates_full_source_over_instead_of_squaring_opacity() {
        let base_u8 = [0, 0, 0, 255];
        let blend_u8 = [255, 0, 0, 128];
        let expected_u8 = blend_rgba_pixel_seeded(base_u8, blend_u8, 1.0, BlendMode::Normal, 0);
        let dissolved_u8 = blend_rgba_pixel_seeded(base_u8, blend_u8, 0.5, BlendMode::Dissolve, 0);

        let base_f32 = [0.0, 0.0, 0.0, 1.0];
        let blend_f32 = [2.0, -0.25, 0.5, 0.5];
        let expected_f32 =
            blend_rgba_f32_pixel_seeded(base_f32, blend_f32, 1.0, BlendMode::Normal, 0);
        let dissolved_f32 =
            blend_rgba_f32_pixel_seeded(base_f32, blend_f32, 0.5, BlendMode::Dissolve, 0);

        assert_eq!(dissolved_u8, expected_u8);
        assert_eq!(dissolved_f32, expected_f32);
        assert_eq!(dissolved_u8[3], 255);
        assert!((dissolved_f32[0] - 1.0).abs() <= f32::EPSILON);
        assert!((dissolved_f32[1] + 0.125).abs() <= f32::EPSILON);
        assert!((dissolved_f32[3] - 1.0).abs() <= f32::EPSILON);
    }

    #[test]
    fn apply_adjustment_pass_blends_from_base_to_adjusted() {
        let base = vec![200, 40, 20, 255];
        let mut out = Vec::new();

        apply_adjustment_pass(
            &base,
            1,
            1,
            AdjustmentLayerParams { exposure: -2.0, ..AdjustmentLayerParams::default() },
            0.5,
            Some(BlendMode::Normal),
            0,
            &mut out,
        )
        .expect("apply adjustment pass");

        assert_eq!(out[3], 255);
        assert!(out[0] < base[0]);
        assert!(out[0] > 0);
    }

    #[test]
    fn primary_color_uses_sequence_luminance_and_scene_linear_contrast_pivot() {
        let source = [0.7, 0.2, 0.05, 0.8];
        let mut rec2020 = vec![source];
        assert!(apply_render_op_f32(
            &mut rec2020,
            1,
            1,
            &EffectRenderOp::ColorAdjust {
                exposure: 0.0,
                contrast: 1.0,
                saturation: 0.0,
                working_color_space: WorkingColorSpace::LinearRec2020,
            },
            0,
        ));
        let coefficients = WorkingColorSpace::LinearRec2020.luminance_coefficients();
        let expected_luminance =
            source[0] * coefficients[0] + source[1] * coefficients[1] + source[2] * coefficients[2];
        for channel in rec2020[0].iter().take(3) {
            assert!((*channel - expected_luminance).abs() <= 1.0e-6);
        }
        assert_eq!(rec2020[0][3], source[3]);

        let mut pivot = vec![[0.18, 0.18, 0.18, 1.0]];
        assert!(apply_render_op_f32(
            &mut pivot,
            1,
            1,
            &EffectRenderOp::ColorAdjust {
                exposure: 0.0,
                contrast: 2.5,
                saturation: 1.0,
                working_color_space: WorkingColorSpace::LinearRec2020,
            },
            0,
        ));
        for channel in pivot[0].iter().take(3) {
            assert!((*channel - 0.18).abs() <= 1.0e-6);
        }
    }

    #[test]
    fn large_gaussian_radius_is_executed_instead_of_clamped_to_legacy_limit() {
        const WIDTH: usize = 401;
        let mut impulse = vec![[0.0; 4]; WIDTH];
        impulse[WIDTH / 2] = [1.0, 0.0, 0.0, 1.0];

        let legacy_limit = gaussian_blur_rgba_f32(&impulse, WIDTH, 1, 24.0);
        let large = gaussian_blur_rgba_f32(&impulse, WIDTH, 1, 100.0);
        let sample = WIDTH / 2 + 50;

        assert!(legacy_limit[sample][3].abs() <= 1.0e-8);
        assert!(
            large[sample][3] > legacy_limit[sample][3].abs() * 100.0,
            "a 100 px authored radius must contribute beyond the old 24 px support"
        );
        assert!((large[sample][0] - 1.0).abs() <= 1.0e-5);
    }

    #[test]
    fn gaussian_radius_is_continuous_across_the_removed_legacy_threshold() {
        const WIDTH: usize = 201;
        let mut impulse = vec![[0.0; 4]; WIDTH];
        impulse[WIDTH / 2] = [1.0, 0.0, 0.0, 1.0];

        let before = gaussian_blur_rgba_f32(&impulse, WIDTH, 1, 23.99);
        let after = gaussian_blur_rgba_f32(&impulse, WIDTH, 1, 24.01);
        let maximum_delta = before
            .iter()
            .zip(after.iter())
            .map(|(left, right)| (left[3] - right[3]).abs())
            .fold(0.0f32, f32::max);

        assert!(
            maximum_delta < 1.0e-3,
            "animated blur radius must not cross an implementation discontinuity: {maximum_delta}"
        );
    }

    #[test]
    fn gaussian_blur_uses_premultiplied_alpha_without_transparent_color_contamination() {
        let input = vec![
            [0.0, 1.0, 0.0, 0.0],
            [1.0, 0.0, 0.0, 1.0],
            [0.0, 0.0, 1.0, 0.0],
        ];
        let output = gaussian_blur_rgba_f32(&input, 3, 1, 1.0);

        for pixel in output {
            if pixel[3] > 1.0e-6 {
                assert!((pixel[0] - 1.0).abs() <= 1.0e-5);
                assert!(pixel[1].abs() <= 1.0e-5);
                assert!(pixel[2].abs() <= 1.0e-5);
            }
        }
    }

    #[test]
    fn large_gaussian_radius_preserves_a_constant_field() {
        let input = vec![[1.75, 0.25, -0.5, 0.4]; 37 * 11];
        let output = gaussian_blur_rgba_f32(&input, 37, 11, 200.0);

        for pixel in output {
            for (actual, expected) in pixel.into_iter().zip([1.75, 0.25, -0.5, 0.4]) {
                assert!((actual - expected).abs() <= 2.0e-4);
            }
        }
    }

    #[test]
    fn gaussian_execution_rejects_values_outside_the_author_contract() {
        let mut rgba8 = vec![0, 0, 0, 255];
        let error = apply_render_op(
            &mut rgba8,
            1,
            1,
            &EffectRenderOp::GaussianBlur { radius: 201.0 },
            0,
        )
        .expect_err("out-of-contract radius must fail closed");
        assert_eq!(
            error,
            EffectExecutionError::InvalidRenderParameter {
                op: "gaussian_blur",
                parameter: "radius",
            }
        );

        let mut float = vec![[0.0, 0.0, 0.0, 1.0]];
        assert!(!apply_render_op_f32(
            &mut float,
            1,
            1,
            &EffectRenderOp::GaussianBlur { radius: f32::NAN },
            0,
        ));
    }

    #[test]
    fn custom_render_processor_can_modify_effect_render_plan_output() {
        let processor = crate::CustomEffectProcessorBinding::new(Arc::new(
            |buffer, _width, _height, params, _frame_seed| {
                let amount = params["amount"].as_f64().unwrap_or(0.0) as f32;
                for px in buffer.chunks_exact_mut(4) {
                    px[0] = unit_to_u8((px[0] as f32 / 255.0 + amount).clamp(0.0, 1.0));
                }
                Ok(())
            },
        ));

        let input = vec![0u8, 0, 0, 255];
        let output = apply_effect_render_plan(
            &input,
            1,
            1,
            &EffectRenderPlan {
                ops: vec![EffectRenderOp::Custom {
                    key: "plugin.render.glow".to_string(),
                    params: serde_json::json!({ "amount": 0.5 }),
                    cache_key: None,
                    cache_policy: crate::effect::EffectCachePolicy::Deterministic,
                    processor: Some(processor),
                }],
            },
            0,
        )
        .expect("execute effect render plan");

        assert_eq!(output[0], 128);
        assert_eq!(output[3], 255);
    }

    #[test]
    fn blend_graph_node_combines_two_inputs() {
        let graph = EffectRenderGraph {
            nodes: vec![
                EffectGraphNode {
                    id: EffectGraphNodeId(0),
                    kind: EffectGraphNodeKind::Source,
                },
                EffectGraphNode {
                    id: EffectGraphNodeId(1),
                    kind: EffectGraphNodeKind::UnaryEffect {
                        input: EffectGraphNodeId(0),
                        op: EffectRenderOp::ColorAdjust {
                            exposure: 0.0,
                            contrast: 1.0,
                            saturation: 0.0,
                            working_color_space: WorkingColorSpace::LinearRec709,
                        },
                    },
                },
                EffectGraphNode {
                    id: EffectGraphNodeId(2),
                    kind: EffectGraphNodeKind::Blend {
                        base: EffectGraphNodeId(0),
                        overlay: EffectGraphNodeId(1),
                        blend_mode: BlendMode::Normal,
                        opacity: 0.5,
                    },
                },
            ],
            output: Some(EffectGraphNodeId(2)),
        };
        let schedule = crate::graph::schedule_effect_render_graph(&graph).expect("schedule graph");
        let input = vec![200u8, 40, 20, 255];

        let output = apply_effect_render_graph(&input, 1, 1, &graph, &schedule, 0)
            .expect("execute effect graph");
        assert_eq!(output[3], 255);
        assert!(output[0] < input[0]);
        assert!(output[1] > input[1]);
    }

    #[test]
    fn mask_graph_node_modulates_alpha() {
        let processor = crate::CustomEffectProcessorBinding::new(Arc::new(
            |buffer, _width, _height, _params, _frame_seed| {
                for px in buffer.chunks_exact_mut(4) {
                    px[3] = 64;
                }
                Ok(())
            },
        ));
        let graph = EffectRenderGraph {
            nodes: vec![
                EffectGraphNode {
                    id: EffectGraphNodeId(0),
                    kind: EffectGraphNodeKind::Source,
                },
                EffectGraphNode {
                    id: EffectGraphNodeId(1),
                    kind: EffectGraphNodeKind::DomainEffect {
                        input: EffectGraphNodeId(0),
                        op: EffectRenderOp::Custom {
                            key: "plugin.render.alpha_mask".to_string(),
                            params: serde_json::json!({}),
                            cache_key: None,
                            cache_policy: crate::effect::EffectCachePolicy::Deterministic,
                            processor: Some(processor),
                        },
                        domain_contract: crate::EffectColorDomainContract {
                            input: crate::EffectColorDomain::SceneLinearRgb,
                            output: crate::EffectColorDomain::AlphaMask,
                        },
                    },
                },
                EffectGraphNode {
                    id: EffectGraphNodeId(2),
                    kind: EffectGraphNodeKind::Mask {
                        input: EffectGraphNodeId(0),
                        mask: EffectGraphNodeId(1),
                        invert: false,
                        mask_op: crate::mask::MaskOp::Add,
                    },
                },
            ],
            output: Some(EffectGraphNodeId(2)),
        };

        let schedule = crate::graph::schedule_effect_render_graph(&graph).expect("schedule graph");
        let input = vec![20u8, 30, 40, 255];
        let output = apply_effect_render_graph(&input, 1, 1, &graph, &schedule, 0)
            .expect("execute alpha-mask effect graph");
        assert_eq!(&output[0..3], &input[0..3]);
        assert_eq!(output[3], 64);
    }

    #[test]
    fn mask_source_and_mask_pipeline_applies_alpha() {
        // Build: Source(0) → MaskSource(1, full-rect, opaque) → Mask(2, input=0, mask=1)
        // Result: output alpha should be modulated by mask (all opaque → no change from mask)
        let graph = EffectRenderGraph {
            nodes: vec![
                EffectGraphNode {
                    id: EffectGraphNodeId(0),
                    kind: EffectGraphNodeKind::Source,
                },
                EffectGraphNode {
                    id: EffectGraphNodeId(1),
                    kind: EffectGraphNodeKind::MaskSource {
                        shape: crate::mask::MaskShape::Rectangle {
                            x: 0.0,
                            y: 0.0,
                            width: 1.0,
                            height: 1.0,
                            corner_radius: 0.0,
                        },
                        feather: 0.0,
                        expansion: 0.0,
                        opacity: 0.5,
                        invert: false,
                    },
                },
                EffectGraphNode {
                    id: EffectGraphNodeId(2),
                    kind: EffectGraphNodeKind::Mask {
                        input: EffectGraphNodeId(0),
                        mask: EffectGraphNodeId(1),
                        invert: false,
                        mask_op: crate::mask::MaskOp::Add,
                    },
                },
            ],
            output: Some(EffectGraphNodeId(2)),
        };

        let schedule = crate::graph::schedule_effect_render_graph(&graph).expect("schedule graph");
        let input = vec![100u8, 150, 200, 200];
        let output = apply_effect_render_graph(&input, 1, 1, &graph, &schedule, 0)
            .expect("execute mask graph");
        // RGB unchanged, alpha halved (200 * 0.5 = 100)
        assert_eq!(&output[0..3], &input[0..3]);
        assert!(
            (output[3] as i32 - 100).abs() <= 1,
            "expected alpha ~100, got {}",
            output[3]
        );
    }

    #[test]
    fn mask_source_half_rect_produces_partial_mask() {
        // Rectangle covering left half of canvas at 50% opacity.
        let graph = EffectRenderGraph {
            nodes: vec![
                EffectGraphNode {
                    id: EffectGraphNodeId(0),
                    kind: EffectGraphNodeKind::Source,
                },
                EffectGraphNode {
                    id: EffectGraphNodeId(1),
                    kind: EffectGraphNodeKind::MaskSource {
                        shape: crate::mask::MaskShape::Rectangle {
                            x: 0.0,
                            y: 0.0,
                            width: 0.5,
                            height: 1.0,
                            corner_radius: 0.0,
                        },
                        feather: 0.0,
                        expansion: 0.0,
                        opacity: 1.0,
                        invert: false,
                    },
                },
                EffectGraphNode {
                    id: EffectGraphNodeId(2),
                    kind: EffectGraphNodeKind::Mask {
                        input: EffectGraphNodeId(0),
                        mask: EffectGraphNodeId(1),
                        invert: false,
                        mask_op: crate::mask::MaskOp::Add,
                    },
                },
            ],
            output: Some(EffectGraphNodeId(2)),
        };

        let schedule = crate::graph::schedule_effect_render_graph(&graph).expect("schedule graph");
        // 2x1 image: left pixel inside rect, right pixel outside.
        let input = vec![255u8, 255, 255, 255, 255, 255, 255, 255];
        let output = apply_effect_render_graph(&input, 2, 1, &graph, &schedule, 0)
            .expect("execute partial mask graph");
        // Left pixel: alpha modulated (inside mask → opaque → alpha = 255)
        assert_eq!(output[3], 255, "left pixel should stay opaque");
        // Right pixel: alpha = 0 (outside mask, feather=0)
        assert_eq!(output[7], 0, "right pixel should be transparent");
    }

    #[test]
    fn blend_graph_node_supports_shared_input_branch() {
        let graph = EffectRenderGraph {
            nodes: vec![
                EffectGraphNode {
                    id: EffectGraphNodeId(0),
                    kind: EffectGraphNodeKind::Source,
                },
                EffectGraphNode {
                    id: EffectGraphNodeId(1),
                    kind: EffectGraphNodeKind::Blend {
                        base: EffectGraphNodeId(0),
                        overlay: EffectGraphNodeId(0),
                        blend_mode: BlendMode::Screen,
                        opacity: 0.5,
                    },
                },
            ],
            output: Some(EffectGraphNodeId(1)),
        };

        let schedule = crate::graph::schedule_effect_render_graph(&graph).expect("schedule graph");
        let input = vec![64u8, 96, 128, 255];
        let output = apply_effect_render_graph(&input, 1, 1, &graph, &schedule, 0)
            .expect("execute blend graph");
        assert_eq!(output[3], 255);
        assert!(output[0] >= input[0]);
        assert!(output[1] >= input[1]);
        assert!(output[2] >= input[2]);
    }

    #[test]
    fn deterministic_custom_effect_output_hits_shared_cache() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_processor = Arc::clone(&calls);
        let processor = crate::CustomEffectProcessorBinding::new(Arc::new(
            move |buffer, _width, _height, _params, _frame_seed| {
                calls_for_processor.fetch_add(1, Ordering::SeqCst);
                for px in buffer.chunks_exact_mut(4) {
                    px[0] = px[0].saturating_add(10);
                }
                Ok(())
            },
        ));

        let compiled = compile_reference_effect_graph(&EffectRenderPlan {
            ops: vec![
                EffectRenderOp::GaussianBlur { radius: 2.0 },
                EffectRenderOp::Custom {
                    key: "plugin.render.cache_counter".to_string(),
                    params: serde_json::json!({}),
                    cache_key: Some("cache-counter".to_string()),
                    cache_policy: crate::effect::EffectCachePolicy::Deterministic,
                    processor: Some(processor),
                },
            ],
        })
        .expect("compile effect graph");

        let input = vec![12u8, 24, 36, 255];
        let mut session = crate::EffectExecutionSession::default();
        let first = session
            .apply_compiled_rgba8(&input, 1, 1, compiled.as_ref(), 0)
            .expect("execute first cached graph");
        let second = session
            .apply_compiled_rgba8(&input, 1, 1, compiled.as_ref(), 0)
            .expect("execute second cached graph");

        assert_eq!(first, second);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn frame_dependent_effect_output_cache_respects_frame_seed() {
        let compiled = compile_reference_effect_graph(&EffectRenderPlan {
            ops: vec![
                EffectRenderOp::GaussianBlur { radius: 2.0 },
                EffectRenderOp::Grain { amount: 0.5 },
            ],
        })
        .expect("compile grain graph");

        let input = vec![80u8, 90, 100, 255];
        let first = apply_compiled_effect_graph(&input, 1, 1, compiled.as_ref(), 1)
            .expect("execute first seeded graph");
        let second = apply_compiled_effect_graph(&input, 1, 1, compiled.as_ref(), 2)
            .expect("execute second seeded graph");

        assert_ne!(first, second);
    }

    #[test]
    fn deterministic_node_cache_does_not_cross_compiled_graph_identity() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_processor = Arc::clone(&calls);
        let processor = crate::CustomEffectProcessorBinding::new(Arc::new(
            move |buffer, _width, _height, _params, _frame_seed| {
                calls_for_processor.fetch_add(1, Ordering::SeqCst);
                for px in buffer.chunks_exact_mut(4) {
                    px[0] = px[0].saturating_add(20);
                }
                Ok(())
            },
        ));

        let first_graph = compile_reference_effect_graph(&EffectRenderPlan {
            ops: vec![
                EffectRenderOp::Custom {
                    key: "plugin.render.shared_subtree".to_string(),
                    params: serde_json::json!({}),
                    cache_key: Some("shared-subtree".to_string()),
                    cache_policy: crate::effect::EffectCachePolicy::Deterministic,
                    processor: Some(processor.clone()),
                },
                EffectRenderOp::ColorAdjust {
                    exposure: 0.0,
                    contrast: 1.0,
                    saturation: 0.0,
                    working_color_space: WorkingColorSpace::LinearRec709,
                },
            ],
        })
        .expect("compile first graph");

        let second_graph = compile_reference_effect_graph(&EffectRenderPlan {
            ops: vec![
                EffectRenderOp::Custom {
                    key: "plugin.render.shared_subtree".to_string(),
                    params: serde_json::json!({}),
                    cache_key: Some("shared-subtree".to_string()),
                    cache_policy: crate::effect::EffectCachePolicy::Deterministic,
                    processor: Some(processor),
                },
                EffectRenderOp::Sharpen { amount: 0.25 },
            ],
        })
        .expect("compile second graph");

        let input = vec![30u8, 60, 90, 255];
        let mut session = crate::EffectExecutionSession::default();
        session
            .apply_compiled_rgba8(&input, 1, 1, first_graph.as_ref(), 0)
            .expect("execute first shared-subtree graph");
        session
            .apply_compiled_rgba8(&input, 1, 1, second_graph.as_ref(), 0)
            .expect("execute second shared-subtree graph");

        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "diagnostic subtree signatures cannot authorize cross-graph pixel reuse"
        );
    }

    #[test]
    fn failing_custom_processor_does_not_commit_partial_frame_changes() {
        let key = "plugin.render.fail_safe";
        let processor = crate::CustomEffectProcessorBinding::new(Arc::new(
            |buffer, _width, _height, _params, _frame_seed| {
                for px in buffer.chunks_exact_mut(4) {
                    px[0] = 255;
                }
                Err(mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "plugin.render.fail_safe".to_string(),
                    reason: "intentional failure".to_string(),
                })
            },
        ));

        let input = vec![32u8, 48, 64, 255];
        let error = apply_effect_render_plan(
            &input,
            1,
            1,
            &EffectRenderPlan {
                ops: vec![EffectRenderOp::Custom {
                    key: key.to_string(),
                    params: serde_json::json!({}),
                    cache_key: None,
                    cache_policy: crate::effect::EffectCachePolicy::Deterministic,
                    processor: Some(processor),
                }],
            },
            0,
        )
        .expect_err("failed custom effect must fail closed");

        assert!(matches!(
            error,
            crate::EffectExecutionError::CustomProcessorFailed { ref key, .. }
                if key == "plugin.render.fail_safe"
        ));
    }

    #[test]
    fn failure_from_old_compiled_custom_binding_does_not_quarantine_replacement_definition() {
        let key = "plugin.render.generation_isolation";
        let effect_type = crate::EffectType::Plugin(key.to_owned());
        let contract = crate::EffectExecutionContract {
            execution_modes: crate::EffectExecutionModes::CPU_U8,
            determinism: crate::EffectDeterminism::Deterministic,
            state_model: crate::EffectStateModel::Stateless,
            temporal_input: crate::EffectTemporalInputExtent::CURRENT_FRAME,
            roi_propagation: crate::EffectRoiPropagation::UnknownRequiresFullFrame,
            resource_lifetime: crate::EffectResourceLifetime::Frame,
            topology: crate::EffectGraphTopology::LinearChain,
        };
        let plugin_contract = crate::EffectPluginContract::new("1.0.0")
            .with_runtime_failure_policy(
                crate::EffectPluginRuntimeFailurePolicy::DisableDefinition,
            );
        crate::register_effect_definition(
            crate::EffectDefinition::new(
                key,
                "Old failing definition",
                Default::default(),
                crate::EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(contract)
            .with_custom_render_backend(
                Arc::new(|_, _| Ok(Some(serde_json::json!({})))),
                None,
                crate::EffectCachePolicy::Deterministic,
                Arc::new(|_, _, _, _, _| {
                    Err(mondrian_core::MondrianError::WorkflowStepFailed {
                        step_id: "old_custom_generation".to_owned(),
                        reason: "intentional old-generation failure".to_owned(),
                    })
                }),
            )
            .with_plugin_contract(plugin_contract.clone()),
        )
        .expect("register old definition");
        let old_graph = crate::PreparedEffectProgram::prepare(
            &[crate::EffectNode::new(effect_type.clone())],
            &[],
            WorkingColorSpace::LinearRec709,
        )
        .expect("prepare old definition")
        .evaluate(mondrian_core::TimelineTime::ZERO)
        .expect("compile old definition");

        crate::register_effect_definition(
            crate::EffectDefinition::new(
                key,
                "Replacement definition",
                Default::default(),
                crate::EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(contract)
            .with_custom_render_backend(
                Arc::new(|_, _| Ok(Some(serde_json::json!({})))),
                None,
                crate::EffectCachePolicy::Deterministic,
                Arc::new(|buffer, _, _, _, _| {
                    buffer[0] = 91;
                    Ok(())
                }),
            )
            .with_plugin_contract(plugin_contract),
        )
        .expect("register replacement definition");
        let current_before =
            crate::effect_plugin_runtime_status(key).expect("replacement runtime status");
        assert!(!current_before.disabled);

        let error = apply_compiled_effect_graph(&[0, 0, 0, 255], 1, 1, old_graph.as_ref(), 0)
            .expect_err("old processor must report its own failure");
        assert!(matches!(
            error,
            crate::EffectExecutionError::CustomProcessorFailed { .. }
        ));
        let current_after =
            crate::effect_plugin_runtime_status(key).expect("replacement runtime status");
        assert_eq!(
            current_after, current_before,
            "old Program failure must not mutate replacement Definition quarantine"
        );

        let replacement_graph = crate::PreparedEffectProgram::prepare(
            &[crate::EffectNode::new(effect_type)],
            &[],
            WorkingColorSpace::LinearRec709,
        )
        .expect("prepare replacement definition")
        .evaluate(mondrian_core::TimelineTime::ZERO)
        .expect("compile replacement definition");
        let output =
            apply_compiled_effect_graph(&[0, 0, 0, 255], 1, 1, replacement_graph.as_ref(), 0)
                .expect("execute replacement definition");
        assert_eq!(output[0], 91);
    }

    #[test]
    fn lut_render_op_preserves_alpha_and_changes_rgb() {
        let lut = crate::Lut3D {
            name: "red-to-blue".to_string(),
            size: 2,
            domain_min: [0.0; 3],
            domain_max: [1.0; 3],
            data: vec![
                [0.0, 0.0, 0.0],
                [0.0, 0.0, 1.0],
                [0.0, 1.0, 0.0],
                [0.0, 1.0, 1.0],
                [1.0, 0.0, 0.0],
                [1.0, 0.0, 1.0],
                [1.0, 1.0, 0.0],
                [1.0, 1.0, 1.0],
            ],
        };
        let output = apply_effect_render_plan(
            &[255, 0, 0, 91],
            1,
            1,
            &EffectRenderPlan {
                ops: vec![EffectRenderOp::Lut3D {
                    lut: Arc::new(crate::PreparedLut3D::new(lut)),
                    intensity: 1.0,
                }],
            },
            0,
        )
        .expect("execute LUT effect");
        assert_eq!(&output, &[0, 0, 255, 91]);
    }

    #[test]
    fn crop_rgba8_keeps_only_pixel_centers_inside_source_relative_bounds() {
        let mut pixels =
            (0_u8..16).flat_map(|value| [value, value, value, 255]).collect::<Vec<_>>();

        apply_render_op(
            &mut pixels,
            4,
            4,
            &EffectRenderOp::Crop { left: 0.25, top: 0.25, right: 0.25, bottom: 0.25 },
            0,
        )
        .expect("execute RGBA8 Crop");

        for (index, pixel) in pixels.chunks_exact(4).enumerate() {
            let x = index % 4;
            let y = index / 4;
            if (1..3).contains(&x) && (1..3).contains(&y) {
                assert_eq!(pixel, &[index as u8, index as u8, index as u8, 255]);
            } else {
                assert_eq!(pixel, &[0, 0, 0, 0]);
            }
        }
    }

    #[test]
    fn crop_partial_region_uses_complete_frame_coordinates() {
        let source = [
            [0.2, 0.3, 0.4, 1.0],
            [0.5, 0.6, 0.7, 1.0],
            [0.8, 0.9, 1.0, 1.0],
            [1.1, 1.2, 1.3, 1.0],
        ];
        let mut region_pixels = source.to_vec();
        let supported = apply_render_op_f32_region_controlled(
            &mut region_pixels,
            EffectRasterRegion::new(4, 4, 2, 1, 2, 2),
            &EffectRenderOp::Crop { left: 0.25, top: 0.25, right: 0.25, bottom: 0.25 },
            0,
            &mut || Ok::<(), Infallible>(()),
        )
        .expect("execute partial-region Crop");

        assert!(supported);
        assert_eq!(
            region_pixels,
            vec![source[0], [0.0; 4], source[2], [0.0; 4]]
        );
    }

    #[test]
    fn crop_rejects_non_finite_insets() {
        let error = apply_render_op(
            &mut vec![255, 255, 255, 255],
            1,
            1,
            &EffectRenderOp::Crop { left: f32::NAN, top: 0.0, right: 0.0, bottom: 0.0 },
            0,
        )
        .expect_err("non-finite Crop must fail closed");

        assert!(matches!(
            error,
            EffectExecutionError::InvalidRenderParameter { op: "crop", parameter: "insets" }
        ));
    }
}
