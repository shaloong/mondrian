use crate::execution::custom_render_processor_registry;
use crate::{effect_definition, plugin_contract, record_plugin_runtime_failure};
use crate::{EffectExecutionError, EffectRenderOp};
use mondrian_core::types::BlendMode;
use serde::{Deserialize, Serialize};
use std::panic::{catch_unwind, AssertUnwindSafe};

pub use crate::execution::{
    apply_compiled_effect_graph, apply_compiled_effect_graph_pass, apply_effect_render_graph,
    apply_effect_render_graph_pass, apply_effect_render_plan, apply_effect_render_plan_pass,
    register_custom_render_processor, CustomEffectRenderProcessor,
};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AdjustmentLayerParams {
    pub exposure: f32,
    pub contrast: f32,
    pub temperature: f32,
    pub tint: f32,
    pub saturation: f32,
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
            temperature: 0.0,
            tint: 0.0,
            saturation: 1.0,
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
            && (self.temperature.abs() <= 1.0e-4)
            && (self.tint.abs() <= 1.0e-4)
            && ((self.saturation - 1.0).abs() <= 1.0e-4)
            && (self.blur_radius.abs() <= 1.0e-4)
            && (self.sharpen_amount.abs() <= 1.0e-4)
            && (self.vignette_intensity.abs() <= 1.0e-4)
            && (self.chromatic_aberration.abs() <= 1.0e-4)
            && (self.grain_amount.abs() <= 1.0e-4)
    }

    pub fn signature_words(&self) -> [u32; 11] {
        [
            self.exposure.to_bits(),
            self.contrast.to_bits(),
            self.temperature.to_bits(),
            self.tint.to_bits(),
            self.saturation.to_bits(),
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
) -> Vec<u8> {
    if params.is_identity() || input.is_empty() || width == 0 || height == 0 {
        return input.to_vec();
    }

    let mut working = input.to_vec();
    apply_primary_color_adjustments(&mut working, params);

    let blur_radius = params.blur_radius.round().clamp(0.0, 24.0) as usize;
    if blur_radius > 0 {
        working = box_blur_rgb(&working, width as usize, height as usize, blur_radius);
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

    working
}

pub(crate) fn apply_render_op(
    working: &mut Vec<u8>,
    width: u32,
    height: u32,
    op: &EffectRenderOp,
    frame_seed: i64,
) -> Result<(), EffectExecutionError> {
    match op {
        EffectRenderOp::ColorAdjust { exposure, contrast, saturation } => {
            apply_primary_color_adjustments(
                working,
                AdjustmentLayerParams {
                    exposure: *exposure,
                    contrast: *contrast,
                    saturation: *saturation,
                    ..AdjustmentLayerParams::default()
                },
            );
        }
        EffectRenderOp::WhiteBalance { temperature, tint } => {
            apply_primary_color_adjustments(
                working,
                AdjustmentLayerParams {
                    temperature: *temperature,
                    tint: *tint,
                    ..AdjustmentLayerParams::default()
                },
            );
        }
        EffectRenderOp::GaussianBlur { radius } => {
            let blur_radius = radius.round().clamp(0.0, 24.0) as usize;
            if blur_radius > 0 {
                *working = box_blur_rgb(working, width as usize, height as usize, blur_radius);
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
        EffectRenderOp::Lut3D { lut, intensity } => {
            lut.apply_rgba8_in_place(working, *intensity);
        }
        EffectRenderOp::Custom { key, params, .. } => {
            let contract = plugin_contract(key);
            if let Some(processor) = custom_render_processor_registry()
                .read()
                .expect("custom render processor registry poisoned")
                .get(key)
                .cloned()
            {
                let mut staged = working.clone();
                let result = catch_unwind(AssertUnwindSafe(|| {
                    processor(&mut staged, width, height, params, frame_seed)
                }));
                match result {
                    Ok(Ok(())) => *working = staged,
                    Ok(Err(error)) => {
                        let reason = error.to_string();
                        record_plugin_runtime_failure(key, contract.as_ref(), reason.clone());
                        return Err(EffectExecutionError::CustomProcessorFailed {
                            key: key.clone(),
                            reason,
                        });
                    }
                    Err(_) => {
                        let reason = "custom render processor panicked".to_string();
                        record_plugin_runtime_failure(key, contract.as_ref(), reason.clone());
                        return Err(EffectExecutionError::CustomProcessorFailed {
                            key: key.clone(),
                            reason,
                        });
                    }
                }
            } else {
                if let Some(definition) = effect_definition(&crate::EffectType::from_key(key)) {
                    record_plugin_runtime_failure(
                        key,
                        definition.plugin_contract(),
                        "custom render processor missing",
                    );
                }
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
    match op {
        EffectRenderOp::ColorAdjust { exposure, contrast, saturation } => {
            apply_primary_color_adjustments_f32(
                working.as_mut_slice(),
                AdjustmentLayerParams {
                    exposure: *exposure,
                    contrast: *contrast,
                    saturation: *saturation,
                    ..AdjustmentLayerParams::default()
                },
            );
            true
        }
        EffectRenderOp::WhiteBalance { temperature, tint } => {
            apply_primary_color_adjustments_f32(
                working.as_mut_slice(),
                AdjustmentLayerParams {
                    temperature: *temperature,
                    tint: *tint,
                    ..AdjustmentLayerParams::default()
                },
            );
            true
        }
        EffectRenderOp::GaussianBlur { radius } => {
            let radius = radius.clamp(0.0, 24.0);
            if radius > 1.0e-4 {
                *working = gaussian_blur_rgba_f32(working, width as usize, height as usize, radius);
            }
            true
        }
        EffectRenderOp::Sharpen { amount } => {
            let amount = amount.clamp(0.0, 2.0);
            if amount > 1.0e-4 {
                let blurred = gaussian_blur_rgba_f32(working, width as usize, height as usize, 1.0);
                apply_unsharp_mask_f32(working, &blurred, amount);
            }
            true
        }
        EffectRenderOp::Vignette { intensity, feather } => {
            let intensity = intensity.clamp(0.0, 1.0);
            if intensity > 1.0e-4 {
                apply_vignette_f32(
                    working,
                    width as usize,
                    height as usize,
                    intensity,
                    *feather,
                );
            }
            true
        }
        EffectRenderOp::ChromaticAberration { amount } => {
            let amount = amount.clamp(0.0, 1.0);
            if amount > 1.0e-4 {
                *working = apply_chromatic_aberration_f32(
                    working,
                    width as usize,
                    height as usize,
                    amount,
                );
            }
            true
        }
        EffectRenderOp::Grain { amount } => {
            let amount = amount.clamp(0.0, 1.0);
            if amount > 1.0e-4 {
                apply_grain_f32(working, width as usize, height as usize, amount, frame_seed);
            }
            true
        }
        EffectRenderOp::Lut3D { lut, intensity } => {
            lut.apply_rgba_f32_in_place(working, *intensity);
            true
        }
        EffectRenderOp::Custom { .. } => false,
    }
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
    if opacity <= 1.0e-4 || base.len() != required_len || processed.len() != required_len {
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
    if opacity <= 1.0e-4 {
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
    if blend_alpha <= 1.0e-4 {
        return base_px;
    }
    if base_alpha <= 1.0e-4 {
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
    if out_alpha <= 1.0e-4 {
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
    if opacity <= 1.0e-4 {
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
    if blend_alpha <= 1.0e-4 {
        return base_px;
    }
    if base_alpha <= 1.0e-4 {
        return [blend_px[0], blend_px[1], blend_px[2], blend_alpha];
    }

    let base_rgb = [base_px[0], base_px[1], base_px[2]];
    let blend_rgb = [blend_px[0], blend_px[1], blend_px[2]];
    let blended_rgb = blend_mode_rgb(blend_mode, base_rgb, blend_rgb);
    let out_alpha = blend_alpha + base_alpha * (1.0 - blend_alpha);
    if out_alpha <= 1.0e-4 {
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
) {
    let required_len = width as usize * height as usize * 4;
    if out.len() != required_len {
        out.resize(required_len, 0);
    }

    if required_len == 0 || base.len() != required_len {
        out.clear();
        return;
    }

    if opacity <= 1.0e-4 || params.is_identity() {
        out.copy_from_slice(base);
        return;
    }

    let processed = apply_adjustment_layer(base, width, height, params, frame_seed);
    blend_adjustment_result(base, &processed, width, height, opacity, blend_mode, out);
}

fn apply_primary_color_adjustments(buffer: &mut [u8], params: AdjustmentLayerParams) {
    let exposure_scale = 2.0f32.powf(params.exposure.clamp(-4.0, 4.0));
    let contrast = params.contrast.clamp(0.0, 3.0);
    let temperature = params.temperature.clamp(-1.0, 1.0);
    let tint = params.tint.clamp(-1.0, 1.0);
    let saturation = params.saturation.clamp(0.0, 3.0);

    for px in buffer.chunks_exact_mut(4) {
        if px[3] == 0 {
            continue;
        }

        let mut rgb = rgb_to_unit(px);
        for channel in &mut rgb {
            *channel = (*channel * exposure_scale).clamp(0.0, 1.0);
            *channel = ((*channel - 0.5) * contrast + 0.5).clamp(0.0, 1.0);
        }

        rgb[0] = (rgb[0] + temperature * 0.12 - tint * 0.04).clamp(0.0, 1.0);
        rgb[1] = (rgb[1] + tint * 0.05).clamp(0.0, 1.0);
        rgb[2] = (rgb[2] - temperature * 0.12 - tint * 0.02).clamp(0.0, 1.0);

        let luma = rgb[0] * 0.2126 + rgb[1] * 0.7152 + rgb[2] * 0.0722;
        rgb[0] = (luma + (rgb[0] - luma) * saturation).clamp(0.0, 1.0);
        rgb[1] = (luma + (rgb[1] - luma) * saturation).clamp(0.0, 1.0);
        rgb[2] = (luma + (rgb[2] - luma) * saturation).clamp(0.0, 1.0);

        px[0] = unit_to_u8(rgb[0]);
        px[1] = unit_to_u8(rgb[1]);
        px[2] = unit_to_u8(rgb[2]);
    }
}

fn apply_primary_color_adjustments_f32(buffer: &mut [[f32; 4]], params: AdjustmentLayerParams) {
    let exposure_scale = 2.0f32.powf(params.exposure.clamp(-4.0, 4.0));
    let contrast = params.contrast.clamp(0.0, 3.0);
    let temperature = params.temperature.clamp(-1.0, 1.0);
    let tint = params.tint.clamp(-1.0, 1.0);
    let saturation = params.saturation.clamp(0.0, 3.0);

    for px in buffer {
        if px[3] <= 1.0e-6 {
            continue;
        }

        let mut rgb = [px[0], px[1], px[2]];
        for channel in &mut rgb {
            *channel *= exposure_scale;
            *channel = (*channel - 0.5) * contrast + 0.5;
        }

        rgb[0] += temperature * 0.12 - tint * 0.04;
        rgb[1] += tint * 0.05;
        rgb[2] += -temperature * 0.12 - tint * 0.02;

        let luma = rgb[0] * 0.2126 + rgb[1] * 0.7152 + rgb[2] * 0.0722;
        px[0] = luma + (rgb[0] - luma) * saturation;
        px[1] = luma + (rgb[1] - luma) * saturation;
        px[2] = luma + (rgb[2] - luma) * saturation;
    }
}

fn gaussian_blur_rgba_f32(
    input: &[[f32; 4]],
    width: usize,
    height: usize,
    radius: f32,
) -> Vec<[f32; 4]> {
    if radius <= 1.0e-4 || width == 0 || height == 0 || input.len() != width * height {
        return input.to_vec();
    }

    let kernel_radius = radius.ceil().clamp(1.0, 24.0) as isize;
    let sigma = (radius / 3.0).max(0.5);
    let mut kernel = (-kernel_radius..=kernel_radius)
        .map(|offset| {
            let distance = offset as f32;
            (-distance * distance / (2.0 * sigma * sigma)).exp()
        })
        .collect::<Vec<_>>();
    let weight_sum = kernel.iter().sum::<f32>().max(f32::EPSILON);
    for weight in &mut kernel {
        *weight /= weight_sum;
    }

    let mut premultiplied = input
        .iter()
        .map(|pixel| {
            let alpha = pixel[3].clamp(0.0, 1.0);
            [pixel[0] * alpha, pixel[1] * alpha, pixel[2] * alpha, alpha]
        })
        .collect::<Vec<_>>();
    let mut horizontal = vec![[0.0; 4]; input.len()];
    for y in 0..height {
        for x in 0..width {
            let mut output = [0.0; 4];
            for (kernel_index, weight) in kernel.iter().enumerate() {
                let offset = kernel_index as isize - kernel_radius;
                let sample_x = (x as isize + offset).clamp(0, width as isize - 1) as usize;
                let sample = premultiplied[y * width + sample_x];
                for channel in 0..4 {
                    output[channel] += sample[channel] * weight;
                }
            }
            horizontal[y * width + x] = output;
        }
    }

    premultiplied.fill([0.0; 4]);
    for y in 0..height {
        for x in 0..width {
            let mut output = [0.0; 4];
            for (kernel_index, weight) in kernel.iter().enumerate() {
                let offset = kernel_index as isize - kernel_radius;
                let sample_y = (y as isize + offset).clamp(0, height as isize - 1) as usize;
                let sample = horizontal[sample_y * width + x];
                for channel in 0..4 {
                    output[channel] += sample[channel] * weight;
                }
            }
            premultiplied[y * width + x] = output;
        }
    }

    for pixel in &mut premultiplied {
        let alpha = pixel[3].clamp(0.0, 1.0);
        if alpha > 1.0e-6 {
            pixel[0] /= alpha;
            pixel[1] /= alpha;
            pixel[2] /= alpha;
        } else {
            pixel[0] = 0.0;
            pixel[1] = 0.0;
            pixel[2] = 0.0;
        }
        pixel[3] = alpha;
    }
    premultiplied
}

fn apply_unsharp_mask_f32(buffer: &mut [[f32; 4]], blurred: &[[f32; 4]], amount: f32) {
    for (pixel, softened) in buffer.iter_mut().zip(blurred) {
        if pixel[3] <= 1.0e-6 {
            continue;
        }
        for channel in 0..3 {
            pixel[channel] += (pixel[channel] - softened[channel]) * amount;
        }
    }
}

fn apply_vignette_f32(
    buffer: &mut [[f32; 4]],
    width: usize,
    height: usize,
    intensity: f32,
    feather: f32,
) {
    let center_x = width.saturating_sub(1) as f32 * 0.5;
    let center_y = height.saturating_sub(1) as f32 * 0.5;
    let feather = feather.clamp(0.05, 1.0);
    let inner = 1.0 - feather * 0.85;

    for y in 0..height {
        for x in 0..width {
            let pixel = &mut buffer[y * width + x];
            if pixel[3] <= 1.0e-6 {
                continue;
            }
            let normalized_x = (x as f32 - center_x) / center_x.max(1.0);
            let normalized_y = (y as f32 - center_y) / center_y.max(1.0);
            let distance =
                (normalized_x * normalized_x + normalized_y * normalized_y).sqrt().min(1.0);
            let gain = 1.0 - smoothstep(inner, 1.0, distance) * intensity;
            pixel[0] *= gain;
            pixel[1] *= gain;
            pixel[2] *= gain;
        }
    }
}

fn apply_chromatic_aberration_f32(
    input: &[[f32; 4]],
    width: usize,
    height: usize,
    amount: f32,
) -> Vec<[f32; 4]> {
    let mut output = input.to_vec();
    let center_x = width.saturating_sub(1) as f32 * 0.5;
    let center_y = height.saturating_sub(1) as f32 * 0.5;
    let maximum_shift = amount * 5.0;

    for y in 0..height {
        for x in 0..width {
            let index = y * width + x;
            if input[index][3] <= 1.0e-6 {
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
    output
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
    if alpha > 1.0e-6 {
        premultiplied / alpha
    } else {
        0.0
    }
}

fn apply_grain_f32(
    buffer: &mut [[f32; 4]],
    width: usize,
    height: usize,
    amount: f32,
    frame_seed: i64,
) {
    for y in 0..height {
        for x in 0..width {
            let pixel = &mut buffer[y * width + x];
            if pixel[3] <= 1.0e-6 {
                continue;
            }
            let noise = grain_noise(x as u32, y as u32, frame_seed) * amount * 0.18;
            pixel[0] += noise;
            pixel[1] += noise;
            pixel[2] += noise;
        }
    }
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
        get_or_compile_scheduled_effect_graph, EffectGraphNode, EffectGraphNodeId,
        EffectGraphNodeKind, EffectRenderGraph, EffectRenderPlan,
    };
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    #[test]
    fn identity_adjustment_leaves_frame_unchanged() {
        let input = vec![32, 64, 96, 255, 12, 24, 36, 255];
        let output = apply_adjustment_layer(&input, 2, 1, AdjustmentLayerParams::default(), 0);
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
        );

        assert_eq!(out[3], 255);
        assert!(out[0] < base[0]);
        assert!(out[0] > 0);
    }

    #[test]
    fn custom_render_processor_can_modify_effect_render_plan_output() {
        register_custom_render_processor(
            "plugin.render.glow",
            Arc::new(|buffer, _width, _height, params, _frame_seed| {
                let amount = params["amount"].as_f64().unwrap_or(0.0) as f32;
                for px in buffer.chunks_exact_mut(4) {
                    px[0] = unit_to_u8((px[0] as f32 / 255.0 + amount).clamp(0.0, 1.0));
                }
                Ok(())
            }),
        );

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
        let schedule = crate::schedule_effect_render_graph(&graph).expect("schedule graph");
        let input = vec![200u8, 40, 20, 255];

        let output = apply_effect_render_graph(&input, 1, 1, &graph, &schedule, 0)
            .expect("execute effect graph");
        assert_eq!(output[3], 255);
        assert!(output[0] < input[0]);
        assert!(output[1] > input[1]);
    }

    #[test]
    fn mask_graph_node_modulates_alpha() {
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

        register_custom_render_processor(
            "plugin.render.alpha_mask",
            Arc::new(|buffer, _width, _height, _params, _frame_seed| {
                for px in buffer.chunks_exact_mut(4) {
                    px[3] = 64;
                }
                Ok(())
            }),
        );

        let schedule = crate::schedule_effect_render_graph(&graph).expect("schedule graph");
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

        let schedule = crate::schedule_effect_render_graph(&graph).expect("schedule graph");
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

        let schedule = crate::schedule_effect_render_graph(&graph).expect("schedule graph");
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

        let schedule = crate::schedule_effect_render_graph(&graph).expect("schedule graph");
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
        register_custom_render_processor(
            "plugin.render.cache_counter",
            Arc::new(move |buffer, _width, _height, _params, _frame_seed| {
                calls_for_processor.fetch_add(1, Ordering::SeqCst);
                for px in buffer.chunks_exact_mut(4) {
                    px[0] = px[0].saturating_add(10);
                }
                Ok(())
            }),
        );

        let compiled = get_or_compile_scheduled_effect_graph(&EffectRenderPlan {
            ops: vec![
                EffectRenderOp::GaussianBlur { radius: 2.0 },
                EffectRenderOp::Custom {
                    key: "plugin.render.cache_counter".to_string(),
                    params: serde_json::json!({}),
                    cache_key: Some("cache-counter".to_string()),
                    cache_policy: crate::effect::EffectCachePolicy::Deterministic,
                },
            ],
        })
        .expect("compile effect graph");

        let input = vec![12u8, 24, 36, 255];
        let first = apply_compiled_effect_graph(&input, 1, 1, compiled.as_ref(), 0)
            .expect("execute first cached graph");
        let second = apply_compiled_effect_graph(&input, 1, 1, compiled.as_ref(), 0)
            .expect("execute second cached graph");

        assert_eq!(first, second);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn frame_dependent_effect_output_cache_respects_frame_seed() {
        let compiled = get_or_compile_scheduled_effect_graph(&EffectRenderPlan {
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
    fn expensive_deterministic_subtree_cache_reuses_output_across_graphs() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_processor = Arc::clone(&calls);
        register_custom_render_processor(
            "plugin.render.shared_subtree",
            Arc::new(move |buffer, _width, _height, _params, _frame_seed| {
                calls_for_processor.fetch_add(1, Ordering::SeqCst);
                for px in buffer.chunks_exact_mut(4) {
                    px[0] = px[0].saturating_add(20);
                }
                Ok(())
            }),
        );

        let first_graph = get_or_compile_scheduled_effect_graph(&EffectRenderPlan {
            ops: vec![
                EffectRenderOp::Custom {
                    key: "plugin.render.shared_subtree".to_string(),
                    params: serde_json::json!({}),
                    cache_key: Some("shared-subtree".to_string()),
                    cache_policy: crate::effect::EffectCachePolicy::Deterministic,
                },
                EffectRenderOp::ColorAdjust { exposure: 0.0, contrast: 1.0, saturation: 0.0 },
            ],
        })
        .expect("compile first graph");

        let second_graph = get_or_compile_scheduled_effect_graph(&EffectRenderPlan {
            ops: vec![
                EffectRenderOp::Custom {
                    key: "plugin.render.shared_subtree".to_string(),
                    params: serde_json::json!({}),
                    cache_key: Some("shared-subtree".to_string()),
                    cache_policy: crate::effect::EffectCachePolicy::Deterministic,
                },
                EffectRenderOp::Sharpen { amount: 0.25 },
            ],
        })
        .expect("compile second graph");

        let input = vec![30u8, 60, 90, 255];
        apply_compiled_effect_graph(&input, 1, 1, first_graph.as_ref(), 0)
            .expect("execute first shared-subtree graph");
        apply_compiled_effect_graph(&input, 1, 1, second_graph.as_ref(), 0)
            .expect("execute second shared-subtree graph");

        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn failing_custom_processor_does_not_commit_partial_frame_changes() {
        let key = "plugin.render.fail_safe";
        crate::register_plugin_contract(
            key,
            crate::EffectPluginContract::new("1.0.0").with_runtime_failure_policy(
                crate::EffectPluginRuntimeFailurePolicy::KeepDefinitionAvailable,
            ),
        );
        register_custom_render_processor(
            key,
            Arc::new(|buffer, _width, _height, _params, _frame_seed| {
                for px in buffer.chunks_exact_mut(4) {
                    px[0] = 255;
                }
                Err(mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "plugin.render.fail_safe".to_string(),
                    reason: "intentional failure".to_string(),
                })
            }),
        );

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
    fn lut_render_op_preserves_alpha_and_changes_rgb() {
        let lut = crate::Lut3D {
            name: "red-to-blue".to_string(),
            size: 2,
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
                ops: vec![EffectRenderOp::Lut3D { lut, intensity: 1.0 }],
            },
            0,
        )
        .expect("execute LUT effect");
        assert_eq!(&output, &[0, 0, 255, 91]);
    }
}
