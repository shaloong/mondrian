use mondrian_core::types::BlendMode;
use serde::{Deserialize, Serialize};

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
    for ((base_px, processed_px), out_px) in
        base.chunks_exact(4).zip(processed.chunks_exact(4)).zip(out.chunks_exact_mut(4))
    {
        let base_alpha = base_px[3];
        if base_alpha == 0 {
            out_px.copy_from_slice(base_px);
            continue;
        }

        let base_rgb = rgb_to_unit(base_px);
        let processed_rgb = rgb_to_unit(processed_px);
        let blended = blend_mode_rgb(mode, base_rgb, processed_rgb);
        let final_rgb = [
            lerp(base_rgb[0], blended[0], opacity),
            lerp(base_rgb[1], blended[1], opacity),
            lerp(base_rgb[2], blended[2], opacity),
        ];
        out_px[0] = unit_to_u8(final_rgb[0]);
        out_px[1] = unit_to_u8(final_rgb[1]);
        out_px[2] = unit_to_u8(final_rgb[2]);
        out_px[3] = base_alpha;
    }
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
    [
        blend_mode_channel(mode, base[0], blend[0]),
        blend_mode_channel(mode, base[1], blend[1]),
        blend_mode_channel(mode, base[2], blend[2]),
    ]
}

fn blend_mode_channel(mode: BlendMode, base: f32, blend: f32) -> f32 {
    match mode {
        BlendMode::Normal => blend,
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
        BlendMode::Add => (base + blend).clamp(0.0, 1.0),
        BlendMode::Subtract => (base - blend).clamp(0.0, 1.0),
    }
}

fn rgb_to_unit(px: &[u8]) -> [f32; 3] {
    [
        px[0] as f32 / 255.0,
        px[1] as f32 / 255.0,
        px[2] as f32 / 255.0,
    ]
}

fn unit_to_u8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
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
}
