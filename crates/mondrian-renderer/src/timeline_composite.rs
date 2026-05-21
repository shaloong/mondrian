use mondrian_core::{
    types::{BlendMode, ColorSpace},
    RgbaF32Frame,
};
use mondrian_effects::{
    apply_compiled_effect_graph, apply_compiled_effect_graph_pass, blend_rgba_pixel_seeded,
    CompiledEffectGraph,
};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct TimelineMediaLayer<'a> {
    pub rgba: &'a [u8],
    pub width: u32,
    pub height: u32,
    pub opacity: f32,
    pub blend_mode: BlendMode,
    pub transform: [f32; 6],
    pub effect_graph: Arc<CompiledEffectGraph>,
    pub frame_seed: i64,
}

#[derive(Debug, Clone)]
pub struct TimelineAdjustmentLayer {
    pub effect_graph: Arc<CompiledEffectGraph>,
    pub opacity: f32,
    pub blend_mode: Option<BlendMode>,
    pub frame_seed: i64,
}

#[derive(Debug, Clone)]
pub enum TimelineCompositeElement<'a> {
    Media(TimelineMediaLayer<'a>),
    Adjustment(TimelineAdjustmentLayer),
}

#[derive(Debug, Clone, Copy, Default)]
pub struct TimelineCompositeOptions {
    pub empty_canvas_transparent: bool,
}

#[derive(Default)]
pub struct TimelineCompositeScratch {
    media_effect: Vec<u8>,
    adjustment: Vec<u8>,
}

pub fn composite_timeline_elements(
    width: u32,
    height: u32,
    elements: &[TimelineCompositeElement<'_>],
    options: TimelineCompositeOptions,
    scratch: &mut TimelineCompositeScratch,
) -> Vec<u8> {
    let mut out = Vec::new();
    composite_timeline_elements_into(&mut out, width, height, elements, options, scratch);
    out
}

pub fn composite_timeline_elements_float_linear(
    width: u32,
    height: u32,
    elements: &[TimelineCompositeElement<'_>],
    options: TimelineCompositeOptions,
    working_color_space: ColorSpace,
    scratch: &mut TimelineCompositeScratch,
) -> Vec<u8> {
    if !can_float_linear_composite(elements) {
        return composite_timeline_elements(width, height, elements, options, scratch);
    }

    let pixel_count = width as usize * height as usize;
    if pixel_count == 0 {
        return Vec::new();
    }

    let mut canvas = vec![[0.0, 0.0, 0.0, 1.0]; pixel_count];
    let mut has_composited_media = false;

    for element in elements {
        let TimelineCompositeElement::Media(layer) = element else {
            continue;
        };
        let src_rgba = if layer.effect_graph.graph.is_identity() {
            layer.rgba
        } else {
            scratch.media_effect = apply_compiled_effect_graph(
                layer.rgba,
                layer.width,
                layer.height,
                &layer.effect_graph,
                layer.frame_seed,
            );
            scratch.media_effect.as_slice()
        };
        let src = RgbaF32Frame::from_rgba8(
            layer.width,
            layer.height,
            src_rgba,
            working_color_space,
            working_color_space,
            false,
        );
        alpha_blend_f32_normal(
            &mut canvas,
            width as usize,
            height as usize,
            &src.data,
            layer.width as usize,
            layer.height as usize,
            layer.opacity,
        );
        has_composited_media = true;
    }

    if !has_composited_media && options.empty_canvas_transparent {
        canvas.fill([0.0, 0.0, 0.0, 0.0]);
    }

    RgbaF32Frame {
        width,
        height,
        data: canvas,
        color_space: working_color_space,
    }
    .to_rgba8(working_color_space, false)
}

fn can_float_linear_composite(elements: &[TimelineCompositeElement<'_>]) -> bool {
    elements.iter().all(|element| match element {
        TimelineCompositeElement::Media(layer) => {
            layer.blend_mode == BlendMode::Normal && is_identity_transform(layer.transform)
        }
        TimelineCompositeElement::Adjustment(_) => false,
    })
}

fn alpha_blend_f32_normal(
    dst: &mut [[f32; 4]],
    dst_w: usize,
    dst_h: usize,
    src: &[[f32; 4]],
    src_w: usize,
    src_h: usize,
    opacity: f32,
) {
    let width = dst_w.min(src_w);
    let height = dst_h.min(src_h);
    let opacity = opacity.clamp(0.0, 1.0);
    if opacity <= 1.0e-4 {
        return;
    }
    for y in 0..height {
        for x in 0..width {
            let dst_px = &mut dst[y * dst_w + x];
            let src_px = src[y * src_w + x];
            let src_a = (src_px[3] * opacity).clamp(0.0, 1.0);
            let inv = 1.0 - src_a;
            dst_px[0] = src_px[0] * src_a + dst_px[0] * inv;
            dst_px[1] = src_px[1] * src_a + dst_px[1] * inv;
            dst_px[2] = src_px[2] * src_a + dst_px[2] * inv;
            dst_px[3] = src_a + dst_px[3] * inv;
        }
    }
}

pub fn composite_timeline_elements_into(
    out: &mut Vec<u8>,
    width: u32,
    height: u32,
    elements: &[TimelineCompositeElement<'_>],
    options: TimelineCompositeOptions,
    scratch: &mut TimelineCompositeScratch,
) {
    let required_len = width as usize * height as usize * 4;
    if out.len() != required_len {
        out.resize(required_len, 0);
    }
    if required_len == 0 {
        out.clear();
        return;
    }

    clear_canvas_black_opaque(out);
    let mut has_composited_media = false;

    for element in elements {
        match element {
            TimelineCompositeElement::Media(layer) => {
                let src_rgba = if layer.effect_graph.graph.is_identity() {
                    layer.rgba
                } else {
                    scratch.media_effect = apply_compiled_effect_graph(
                        layer.rgba,
                        layer.width,
                        layer.height,
                        &layer.effect_graph,
                        layer.frame_seed,
                    );
                    scratch.media_effect.as_slice()
                };
                alpha_blend_layer(
                    out,
                    width,
                    height,
                    src_rgba,
                    layer.width,
                    layer.height,
                    layer.opacity,
                    layer.blend_mode,
                    layer.transform,
                );
                has_composited_media = true;
            }
            TimelineCompositeElement::Adjustment(layer) => {
                if !has_composited_media
                    || layer.opacity <= 1.0e-4
                    || layer.effect_graph.graph.is_identity()
                {
                    continue;
                }
                apply_compiled_effect_graph_pass(
                    out,
                    width,
                    height,
                    &layer.effect_graph,
                    layer.opacity,
                    layer.blend_mode,
                    layer.frame_seed,
                    &mut scratch.adjustment,
                );
                std::mem::swap(out, &mut scratch.adjustment);
            }
        }
    }

    if !has_composited_media && options.empty_canvas_transparent {
        out.fill(0);
    }
}

pub fn is_identity_transform(transform: [f32; 6]) -> bool {
    const EPS: f32 = 1.0e-4;
    (transform[0] - 1.0).abs() <= EPS
        && transform[1].abs() <= EPS
        && transform[2].abs() <= EPS
        && transform[3].abs() <= EPS
        && (transform[4] - 1.0).abs() <= EPS
        && transform[5].abs() <= EPS
}

pub fn quantize_transform_signature(transform: [f32; 6]) -> [i32; 6] {
    const SCALE: f32 = 1024.0;
    [
        (transform[0] * SCALE).round() as i32,
        (transform[1] * SCALE).round() as i32,
        (transform[2] * SCALE).round() as i32,
        (transform[3] * SCALE).round() as i32,
        (transform[4] * SCALE).round() as i32,
        (transform[5] * SCALE).round() as i32,
    ]
}

fn clear_canvas_black_opaque(canvas: &mut [u8]) {
    canvas.fill(0);
    for px in canvas.chunks_exact_mut(4) {
        px[3] = 255;
    }
}

fn alpha_blend_layer(
    dst_rgba: &mut [u8],
    dst_w: u32,
    dst_h: u32,
    src_rgba: &[u8],
    src_w: u32,
    src_h: u32,
    opacity: f32,
    blend_mode: BlendMode,
    transform: [f32; 6],
) {
    let width = dst_w.min(src_w) as usize;
    let height = dst_h.min(src_h) as usize;
    let opacity = opacity.clamp(0.0, 1.0);
    if opacity <= 1.0e-4 {
        return;
    }

    let dst_stride = dst_w as usize * 4;
    let src_stride = src_w as usize * 4;

    if is_identity_transform(transform) {
        for y in 0..height {
            let dst_row = &mut dst_rgba[y * dst_stride..(y + 1) * dst_stride];
            let src_row = &src_rgba[y * src_stride..(y + 1) * src_stride];
            for (x, (dst_px, src_px)) in
                dst_row.chunks_exact_mut(4).zip(src_row.chunks_exact(4)).take(width).enumerate()
            {
                let blended = blend_rgba_pixel_seeded(
                    [dst_px[0], dst_px[1], dst_px[2], dst_px[3]],
                    [src_px[0], src_px[1], src_px[2], src_px[3]],
                    opacity,
                    blend_mode,
                    (y * dst_w as usize + x) as u32,
                );
                dst_px.copy_from_slice(&blended);
            }
        }
        return;
    }

    let Some(inv) = invert_affine(transform) else {
        return;
    };

    let dst_width = dst_w as usize;
    let dst_height = dst_h as usize;
    let src_width = src_w as usize;
    let src_height = src_h as usize;

    for dy in 0..dst_height {
        for dx in 0..dst_width {
            let fx = dx as f32 + 0.5;
            let fy = dy as f32 + 0.5;
            let sx = inv[0] * fx + inv[1] * fy + inv[2];
            let sy = inv[3] * fx + inv[4] * fy + inv[5];
            let Some(src_px) = sample_src_rgba(src_rgba, src_width, src_height, sx - 0.5, sy - 0.5)
            else {
                continue;
            };
            let dst_idx = (dy * dst_width + dx) * 4;
            if dst_idx + 3 >= dst_rgba.len() {
                continue;
            }
            let dst_px = &mut dst_rgba[dst_idx..dst_idx + 4];
            let blended = blend_rgba_pixel_seeded(
                [dst_px[0], dst_px[1], dst_px[2], dst_px[3]],
                src_px,
                opacity,
                blend_mode,
                (dy * dst_width + dx) as u32,
            );
            dst_px.copy_from_slice(&blended);
        }
    }
}

fn invert_affine(transform: [f32; 6]) -> Option<[f32; 6]> {
    let a = transform[0];
    let c = transform[1];
    let tx = transform[2];
    let b = transform[3];
    let d = transform[4];
    let ty = transform[5];

    let det = a * d - b * c;
    if det.abs() <= 1.0e-6 {
        return None;
    }

    let inv_det = 1.0 / det;
    let ia = d * inv_det;
    let ic = -c * inv_det;
    let ib = -b * inv_det;
    let id = a * inv_det;
    let itx = -(ia * tx + ic * ty);
    let ity = -(ib * tx + id * ty);
    Some([ia, ic, itx, ib, id, ity])
}

fn sample_src_rgba(
    src_rgba: &[u8],
    src_w: usize,
    src_h: usize,
    sx: f32,
    sy: f32,
) -> Option<[u8; 4]> {
    let x = sx.round() as isize;
    let y = sy.round() as isize;
    if x < 0 || y < 0 || x >= src_w as isize || y >= src_h as isize {
        return None;
    }

    let idx = (y as usize * src_w + x as usize) * 4;
    if idx + 3 >= src_rgba.len() {
        return None;
    }
    Some([
        src_rgba[idx],
        src_rgba[idx + 1],
        src_rgba[idx + 2],
        src_rgba[idx + 3],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_effects::{get_or_compile_scheduled_effect_graph, EffectRenderPlan};

    fn identity_media<'a>(rgba: &'a [u8], width: u32, height: u32) -> TimelineCompositeElement<'a> {
        TimelineCompositeElement::Media(TimelineMediaLayer {
            rgba,
            width,
            height,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_graph: get_or_compile_scheduled_effect_graph(&EffectRenderPlan::default())
                .expect("compile identity graph"),
            frame_seed: 0,
        })
    }

    #[test]
    fn media_effects_are_applied_before_compositing() {
        let mut scratch = TimelineCompositeScratch::default();
        let output = composite_timeline_elements(
            1,
            1,
            &[TimelineCompositeElement::Media(TimelineMediaLayer {
                rgba: &[120, 80, 40, 255],
                width: 1,
                height: 1,
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_graph: get_or_compile_scheduled_effect_graph(&EffectRenderPlan {
                    ops: vec![mondrian_effects::EffectRenderOp::ColorAdjust {
                        exposure: 0.0,
                        contrast: 1.0,
                        saturation: 0.0,
                    }],
                })
                .expect("compile effect graph"),
                frame_seed: 0,
            })],
            TimelineCompositeOptions { empty_canvas_transparent: true },
            &mut scratch,
        );

        assert_eq!(output[0], output[1]);
        assert_eq!(output[1], output[2]);
        assert_eq!(output[3], 255);
    }

    #[test]
    fn custom_render_ops_flow_through_shared_compositor() {
        mondrian_effects::register_custom_render_processor(
            "plugin.render.test_invert",
            std::sync::Arc::new(|buffer, _, _, _, _| {
                for px in buffer.chunks_exact_mut(4) {
                    px[0] = 255u8.saturating_sub(px[0]);
                    px[1] = 255u8.saturating_sub(px[1]);
                    px[2] = 255u8.saturating_sub(px[2]);
                }
                Ok(())
            }),
        );

        let mut scratch = TimelineCompositeScratch::default();
        let output = composite_timeline_elements(
            1,
            1,
            &[TimelineCompositeElement::Media(TimelineMediaLayer {
                rgba: &[10, 20, 30, 255],
                width: 1,
                height: 1,
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_graph: get_or_compile_scheduled_effect_graph(&EffectRenderPlan {
                    ops: vec![mondrian_effects::EffectRenderOp::Custom {
                        key: "plugin.render.test_invert".to_string(),
                        params: Default::default(),
                        cache_key: None,
                        cache_policy: mondrian_effects::EffectCachePolicy::Deterministic,
                    }],
                })
                .expect("compile custom graph"),
                frame_seed: 0,
            })],
            TimelineCompositeOptions { empty_canvas_transparent: true },
            &mut scratch,
        );

        assert_eq!(&output[0..4], &[245, 235, 225, 255]);
    }

    #[test]
    fn adjustment_affects_only_layers_below_it() {
        let mut scratch = TimelineCompositeScratch::default();
        let output = composite_timeline_elements(
            2,
            1,
            &[
                identity_media(&[255, 0, 0, 255, 255, 0, 0, 255], 2, 1),
                TimelineCompositeElement::Adjustment(TimelineAdjustmentLayer {
                    effect_graph: get_or_compile_scheduled_effect_graph(&EffectRenderPlan {
                        ops: vec![mondrian_effects::EffectRenderOp::ColorAdjust {
                            exposure: 0.0,
                            contrast: 1.0,
                            saturation: 0.0,
                        }],
                    })
                    .expect("compile adjustment graph"),
                    opacity: 1.0,
                    blend_mode: Some(BlendMode::Normal),
                    frame_seed: 0,
                }),
                identity_media(&[0, 0, 0, 0, 0, 255, 0, 255], 2, 1),
            ],
            TimelineCompositeOptions::default(),
            &mut scratch,
        );

        assert_eq!(&output[0..4], &[54, 54, 54, 255]);
        assert_eq!(&output[4..8], &[0, 255, 0, 255]);
    }

    #[test]
    fn media_blend_mode_is_applied_during_compositing() {
        let mut scratch = TimelineCompositeScratch::default();
        let output = composite_timeline_elements(
            1,
            1,
            &[
                identity_media(&[128, 64, 32, 255], 1, 1),
                TimelineCompositeElement::Media(TimelineMediaLayer {
                    rgba: &[64, 192, 128, 255],
                    width: 1,
                    height: 1,
                    opacity: 1.0,
                    blend_mode: BlendMode::Multiply,
                    transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                    effect_graph: get_or_compile_scheduled_effect_graph(
                        &EffectRenderPlan::default(),
                    )
                    .expect("compile identity graph"),
                    frame_seed: 0,
                }),
            ],
            TimelineCompositeOptions::default(),
            &mut scratch,
        );

        assert_eq!(&output[0..4], &[32, 48, 16, 255]);
    }

    #[test]
    fn float_linear_compositor_matches_normal_single_layer_and_preserves_alpha() {
        let mut scratch = TimelineCompositeScratch::default();
        let output = composite_timeline_elements_float_linear(
            1,
            1,
            &[TimelineCompositeElement::Media(TimelineMediaLayer {
                rgba: &[64, 128, 192, 255],
                width: 1,
                height: 1,
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_graph: get_or_compile_scheduled_effect_graph(&EffectRenderPlan::default())
                    .expect("compile identity graph"),
                frame_seed: 0,
            })],
            TimelineCompositeOptions::default(),
            mondrian_core::types::ColorSpace::Rec709,
            &mut scratch,
        );

        assert_eq!(output[3], 255);
        assert!((output[0] as i16 - 64).abs() <= 1);
        assert!((output[1] as i16 - 128).abs() <= 1);
        assert!((output[2] as i16 - 192).abs() <= 1);
    }

    #[test]
    fn float_linear_compositor_falls_back_for_adjustments() {
        let mut float_scratch = TimelineCompositeScratch::default();
        let mut legacy_scratch = TimelineCompositeScratch::default();
        let elements = [
            identity_media(&[255, 0, 0, 255], 1, 1),
            TimelineCompositeElement::Adjustment(TimelineAdjustmentLayer {
                effect_graph: get_or_compile_scheduled_effect_graph(&EffectRenderPlan {
                    ops: vec![mondrian_effects::EffectRenderOp::ColorAdjust {
                        exposure: 0.0,
                        contrast: 1.0,
                        saturation: 0.0,
                    }],
                })
                .expect("compile adjustment"),
                opacity: 1.0,
                blend_mode: Some(BlendMode::Normal),
                frame_seed: 0,
            }),
        ];
        let float_output = composite_timeline_elements_float_linear(
            1,
            1,
            &elements,
            TimelineCompositeOptions::default(),
            mondrian_core::types::ColorSpace::Rec709,
            &mut float_scratch,
        );
        let legacy_output = composite_timeline_elements(
            1,
            1,
            &elements,
            TimelineCompositeOptions::default(),
            &mut legacy_scratch,
        );
        assert_eq!(float_output, legacy_output);
    }
}
