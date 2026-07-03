use crate::CpuColorFrame;
use mondrian_core::{
    types::{BlendMode, Color, ColorSpace},
    RgbaF32Frame,
};
use mondrian_effects::{
    apply_compiled_effect_graph, apply_compiled_effect_graph_pass,
    apply_compiled_effect_graph_pass_rgba_f32, apply_compiled_effect_graph_rgba_f32,
    blend_rgba_pixel_seeded, compiled_effect_graph_supports_rgba_f32, CompiledEffectGraph,
};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct TimelineMediaLayer<'a> {
    pub frame: &'a CpuColorFrame,
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
pub struct TimelineSolidColorLayer {
    pub color: Color,
    pub opacity: f32,
    pub blend_mode: BlendMode,
    pub transform: [f32; 6],
    pub effect_graph: Arc<CompiledEffectGraph>,
    pub frame_seed: i64,
}

#[derive(Debug, Clone)]
pub enum TimelineCompositeElement<'a> {
    Media(TimelineMediaLayer<'a>),
    Adjustment(TimelineAdjustmentLayer),
    SolidColor(TimelineSolidColorLayer),
}

#[derive(Debug, Clone, Copy, Default)]
pub struct TimelineCompositeOptions {
    pub empty_canvas_transparent: bool,
}

#[derive(Default)]
pub struct TimelineCompositeScratch {
    media_source: Vec<u8>,
    media_effect: Vec<u8>,
    adjustment: Vec<u8>,
    solid_fill: Vec<u8>,
}

/// A CPU composite result paired with color-path diagnostics for the plan.
#[derive(Debug, Clone)]
pub struct TimelineCompositeFrame {
    /// The composited frame in the requested working color context.
    pub frame: CpuColorFrame,
    /// Per-plan diagnostics describing whether compositing stayed float/linear
    /// or fell back to the legacy RGBA8 path.
    pub diagnostics: TimelineCompositeDiagnostics,
}

/// Counters describing which timeline composite path was used and why.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TimelineCompositeDiagnostics {
    /// Number of timeline elements evaluated for the composite plan.
    pub elements: u64,
    /// Composite plans that stayed on the float/linear path.
    pub float_linear_composites: u64,
    /// Composite plans that fell back to the legacy RGBA8 path.
    pub legacy_rgba8_composites: u64,
    /// Media layers that required legacy RGBA8 because of blend mode support.
    pub legacy_media_blend_mode: u64,
    /// Media layers that required legacy RGBA8 because of transform support.
    pub legacy_media_transform: u64,
    /// Media layers that required legacy RGBA8 because of effect graph support.
    pub legacy_media_effect: u64,
    /// Solid layers that required legacy RGBA8 because of blend mode support.
    pub legacy_solid_blend_mode: u64,
    /// Solid layers that required legacy RGBA8 because of transform support.
    pub legacy_solid_transform: u64,
    /// Solid layers that required legacy RGBA8 because of effect graph support.
    pub legacy_solid_effect: u64,
    /// Adjustment layers that required legacy RGBA8 because of blend mode support.
    pub legacy_adjustment_blend_mode: u64,
    /// Adjustment layers that required legacy RGBA8 because of effect graph support.
    pub legacy_adjustment_effect: u64,
}

impl TimelineCompositeDiagnostics {
    /// Merge another diagnostic snapshot into this one using saturating counters.
    pub fn accumulate(&mut self, other: Self) {
        self.elements = self.elements.saturating_add(other.elements);
        self.float_linear_composites =
            self.float_linear_composites.saturating_add(other.float_linear_composites);
        self.legacy_rgba8_composites =
            self.legacy_rgba8_composites.saturating_add(other.legacy_rgba8_composites);
        self.legacy_media_blend_mode =
            self.legacy_media_blend_mode.saturating_add(other.legacy_media_blend_mode);
        self.legacy_media_transform =
            self.legacy_media_transform.saturating_add(other.legacy_media_transform);
        self.legacy_media_effect =
            self.legacy_media_effect.saturating_add(other.legacy_media_effect);
        self.legacy_solid_blend_mode =
            self.legacy_solid_blend_mode.saturating_add(other.legacy_solid_blend_mode);
        self.legacy_solid_transform =
            self.legacy_solid_transform.saturating_add(other.legacy_solid_transform);
        self.legacy_solid_effect =
            self.legacy_solid_effect.saturating_add(other.legacy_solid_effect);
        self.legacy_adjustment_blend_mode = self
            .legacy_adjustment_blend_mode
            .saturating_add(other.legacy_adjustment_blend_mode);
        self.legacy_adjustment_effect =
            self.legacy_adjustment_effect.saturating_add(other.legacy_adjustment_effect);
    }

    /// Returns true when the composite plan used any legacy RGBA8 fallback.
    pub fn uses_legacy_rgba8(self) -> bool {
        self.legacy_rgba8_composites > 0
    }
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

/// Composite timeline elements into a typed color-managed working frame.
pub fn composite_timeline_elements_color_frame(
    width: u32,
    height: u32,
    elements: &[TimelineCompositeElement<'_>],
    options: TimelineCompositeOptions,
    working_color_space: ColorSpace,
    scratch: &mut TimelineCompositeScratch,
) -> CpuColorFrame {
    composite_timeline_elements_color_frame_with_diagnostics(
        width,
        height,
        elements,
        options,
        working_color_space,
        scratch,
    )
    .frame
}

/// Composite timeline elements and return both the working frame and
/// diagnostics for the selected color path.
pub fn composite_timeline_elements_color_frame_with_diagnostics(
    width: u32,
    height: u32,
    elements: &[TimelineCompositeElement<'_>],
    options: TimelineCompositeOptions,
    working_color_space: ColorSpace,
    scratch: &mut TimelineCompositeScratch,
) -> TimelineCompositeFrame {
    let diagnostics = composite_path_diagnostics(elements);
    let frame = if diagnostics.uses_legacy_rgba8() {
        let rgba = composite_timeline_elements(width, height, elements, options, scratch);
        RgbaF32Frame::from_rgba8(
            width,
            height,
            &rgba,
            working_color_space,
            working_color_space,
            false,
        )
    } else {
        composite_supported_elements_to_working_frame(
            width,
            height,
            elements,
            options,
            working_color_space,
            scratch,
        )
    };
    TimelineCompositeFrame { frame: CpuColorFrame::working(frame), diagnostics }
}

fn composite_supported_elements_to_working_frame(
    width: u32,
    height: u32,
    elements: &[TimelineCompositeElement<'_>],
    options: TimelineCompositeOptions,
    working_color_space: ColorSpace,
    _scratch: &mut TimelineCompositeScratch,
) -> RgbaF32Frame {
    let pixel_count = width as usize * height as usize;
    if pixel_count == 0 {
        return RgbaF32Frame {
            width,
            height,
            data: Vec::new(),
            color_space: working_color_space,
        };
    }

    let mut canvas = vec![[0.0, 0.0, 0.0, 1.0]; pixel_count];
    let mut has_composited_layer = false;

    for element in elements {
        match element {
            TimelineCompositeElement::Media(layer) => {
                let frame = layer.frame.rgba_f32();
                let effect_output;
                let (src_data, src_width, src_height) = if layer.effect_graph.graph.is_identity() {
                    (&frame.data, frame.width, frame.height)
                } else {
                    effect_output = apply_compiled_effect_graph_rgba_f32(
                        &frame.data,
                        frame.width,
                        frame.height,
                        &layer.effect_graph,
                        layer.frame_seed,
                    )
                    .expect("float-compatible effect graph");
                    (&effect_output, frame.width, frame.height)
                };
                alpha_blend_f32_normal(
                    &mut canvas,
                    width as usize,
                    height as usize,
                    src_data,
                    src_width as usize,
                    src_height as usize,
                    layer.opacity,
                );
                has_composited_layer = true;
            }
            TimelineCompositeElement::SolidColor(layer) => {
                alpha_blend_f32_solid(
                    &mut canvas,
                    [layer.color.r, layer.color.g, layer.color.b, layer.color.a],
                    layer.opacity,
                );
                has_composited_layer = true;
            }
            TimelineCompositeElement::Adjustment(layer) => {
                if !has_composited_layer
                    || layer.opacity <= 1.0e-4
                    || layer.effect_graph.graph.is_identity()
                {
                    continue;
                }
                canvas = apply_compiled_effect_graph_pass_rgba_f32(
                    &canvas,
                    width,
                    height,
                    &layer.effect_graph,
                    layer.opacity,
                    layer.blend_mode,
                    layer.frame_seed,
                )
                .expect("float-compatible adjustment graph");
            }
        }
    }

    if !has_composited_layer && options.empty_canvas_transparent {
        canvas.fill([0.0, 0.0, 0.0, 0.0]);
    }

    RgbaF32Frame {
        width,
        height,
        data: canvas,
        color_space: working_color_space,
    }
}

/// Diagnose whether a set of timeline elements can stay on the float/linear
/// compositor path, or which capabilities force legacy RGBA8 fallback.
pub fn composite_path_diagnostics(
    elements: &[TimelineCompositeElement<'_>],
) -> TimelineCompositeDiagnostics {
    let mut diagnostics = TimelineCompositeDiagnostics {
        elements: elements.len() as u64,
        ..TimelineCompositeDiagnostics::default()
    };
    for element in elements {
        match element {
            TimelineCompositeElement::Media(layer) => {
                if layer.blend_mode != BlendMode::Normal {
                    diagnostics.legacy_media_blend_mode =
                        diagnostics.legacy_media_blend_mode.saturating_add(1);
                }
                if !is_identity_transform(layer.transform) {
                    diagnostics.legacy_media_transform =
                        diagnostics.legacy_media_transform.saturating_add(1);
                }
                if !compiled_effect_graph_supports_rgba_f32(&layer.effect_graph) {
                    diagnostics.legacy_media_effect =
                        diagnostics.legacy_media_effect.saturating_add(1);
                }
            }
            TimelineCompositeElement::SolidColor(layer) => {
                if layer.blend_mode != BlendMode::Normal {
                    diagnostics.legacy_solid_blend_mode =
                        diagnostics.legacy_solid_blend_mode.saturating_add(1);
                }
                if !is_identity_transform(layer.transform) {
                    diagnostics.legacy_solid_transform =
                        diagnostics.legacy_solid_transform.saturating_add(1);
                }
                if !layer.effect_graph.graph.is_identity() {
                    diagnostics.legacy_solid_effect =
                        diagnostics.legacy_solid_effect.saturating_add(1);
                }
            }
            TimelineCompositeElement::Adjustment(layer) => {
                if layer.blend_mode.unwrap_or(BlendMode::Normal) != BlendMode::Normal {
                    diagnostics.legacy_adjustment_blend_mode =
                        diagnostics.legacy_adjustment_blend_mode.saturating_add(1);
                }
                if !compiled_effect_graph_supports_rgba_f32(&layer.effect_graph) {
                    diagnostics.legacy_adjustment_effect =
                        diagnostics.legacy_adjustment_effect.saturating_add(1);
                }
            }
        }
    }
    let legacy_reasons = diagnostics.legacy_media_blend_mode
        + diagnostics.legacy_media_transform
        + diagnostics.legacy_media_effect
        + diagnostics.legacy_solid_blend_mode
        + diagnostics.legacy_solid_transform
        + diagnostics.legacy_solid_effect
        + diagnostics.legacy_adjustment_blend_mode
        + diagnostics.legacy_adjustment_effect;
    if legacy_reasons == 0 {
        diagnostics.float_linear_composites = 1;
    } else {
        diagnostics.legacy_rgba8_composites = 1;
    }
    diagnostics
}

fn alpha_blend_f32_solid(dst: &mut [[f32; 4]], color: [f32; 4], opacity: f32) {
    let src_a = (color[3] * opacity.clamp(0.0, 1.0)).clamp(0.0, 1.0);
    if src_a <= 1.0e-4 {
        return;
    }
    let inv = 1.0 - src_a;
    for dst_px in dst {
        dst_px[0] = color[0] * src_a + dst_px[0] * inv;
        dst_px[1] = color[1] * src_a + dst_px[1] * inv;
        dst_px[2] = color[2] * src_a + dst_px[2] * inv;
        dst_px[3] = src_a + dst_px[3] * inv;
    }
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
                let descriptor = layer.frame.descriptor();
                scratch.media_source = layer.frame.to_output_rgba8(descriptor.color_space, false);
                let src_rgba = if layer.effect_graph.graph.is_identity() {
                    scratch.media_source.as_slice()
                } else {
                    scratch.media_effect = apply_compiled_effect_graph(
                        &scratch.media_source,
                        descriptor.width,
                        descriptor.height,
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
                    descriptor.width,
                    descriptor.height,
                    layer.opacity,
                    layer.blend_mode,
                    layer.transform,
                );
                has_composited_media = true;
            }
            TimelineCompositeElement::SolidColor(layer) => {
                fill_solid_rgba(
                    &mut scratch.solid_fill,
                    width as usize,
                    height as usize,
                    layer.color,
                );
                let src_rgba = if layer.effect_graph.graph.is_identity() {
                    scratch.solid_fill.as_slice()
                } else {
                    scratch.media_effect = apply_compiled_effect_graph(
                        &scratch.solid_fill,
                        width,
                        height,
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
                    width,
                    height,
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

fn fill_solid_rgba(buf: &mut Vec<u8>, width: usize, height: usize, color: Color) {
    let r = (color.r.clamp(0.0, 1.0) * 255.0).round() as u8;
    let g = (color.g.clamp(0.0, 1.0) * 255.0).round() as u8;
    let b = (color.b.clamp(0.0, 1.0) * 255.0).round() as u8;
    let a = (color.a.clamp(0.0, 1.0) * 255.0).round() as u8;
    let pixel = [r, g, b, a];
    buf.resize(width * height * 4, 0);
    for chunk in buf.chunks_exact_mut(4) {
        chunk.copy_from_slice(&pixel);
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

    fn working_frame(rgba: &[u8], width: u32, height: u32) -> CpuColorFrame {
        CpuColorFrame::working(RgbaF32Frame::from_rgba8(
            width,
            height,
            rgba,
            mondrian_core::types::ColorSpace::Rec709,
            mondrian_core::types::ColorSpace::Rec709,
            false,
        ))
    }

    fn identity_media<'a>(frame: &'a CpuColorFrame) -> TimelineCompositeElement<'a> {
        TimelineCompositeElement::Media(TimelineMediaLayer {
            frame,
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
        let media = working_frame(&[120, 80, 40, 255], 1, 1);
        let output = composite_timeline_elements(
            1,
            1,
            &[TimelineCompositeElement::Media(TimelineMediaLayer {
                frame: &media,
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
        let media = working_frame(&[10, 20, 30, 255], 1, 1);
        let output = composite_timeline_elements(
            1,
            1,
            &[TimelineCompositeElement::Media(TimelineMediaLayer {
                frame: &media,
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
        let lower = working_frame(&[255, 0, 0, 255, 255, 0, 0, 255], 2, 1);
        let upper = working_frame(&[0, 0, 0, 0, 0, 255, 0, 255], 2, 1);
        let output = composite_timeline_elements(
            2,
            1,
            &[
                identity_media(&lower),
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
                identity_media(&upper),
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
        let base = working_frame(&[128, 64, 32, 255], 1, 1);
        let blend = working_frame(&[64, 192, 128, 255], 1, 1);
        let output = composite_timeline_elements(
            1,
            1,
            &[
                identity_media(&base),
                TimelineCompositeElement::Media(TimelineMediaLayer {
                    frame: &blend,
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
        let media = working_frame(&[64, 128, 192, 255], 1, 1);
        let frame = composite_timeline_elements_color_frame(
            1,
            1,
            &[TimelineCompositeElement::Media(TimelineMediaLayer {
                frame: &media,
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
        assert_eq!(frame.descriptor().domain, crate::ColorFrameDomain::Working);
        let output = frame.to_output_rgba8(mondrian_core::types::ColorSpace::Rec709, false);

        assert_eq!(output[3], 255);
        assert!((output[0] as i16 - 64).abs() <= 1);
        assert!((output[1] as i16 - 128).abs() <= 1);
        assert!((output[2] as i16 - 192).abs() <= 1);
        assert!(scratch.media_source.is_empty());
        assert!(scratch.media_effect.is_empty());
    }

    #[test]
    fn float_linear_compositor_preserves_extended_solid_color_without_rgba8_scratch() {
        let mut scratch = TimelineCompositeScratch::default();
        let frame = composite_timeline_elements_color_frame(
            1,
            1,
            &[TimelineCompositeElement::SolidColor(
                TimelineSolidColorLayer {
                    color: Color { r: 1.25, g: 0.5, b: 0.125, a: 1.0 },
                    opacity: 1.0,
                    blend_mode: BlendMode::Normal,
                    transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                    effect_graph: get_or_compile_scheduled_effect_graph(
                        &EffectRenderPlan::default(),
                    )
                    .expect("compile identity graph"),
                    frame_seed: 0,
                },
            )],
            TimelineCompositeOptions::default(),
            mondrian_core::types::ColorSpace::Rec709,
            &mut scratch,
        );

        let px = frame.rgba_f32().data[0];
        assert_eq!(
            frame.descriptor().encoding,
            crate::ColorFrameEncoding::LinearFloat
        );
        assert!((px[0] - 1.25).abs() <= f32::EPSILON);
        assert!((px[1] - 0.5).abs() <= f32::EPSILON);
        assert!((px[2] - 0.125).abs() <= f32::EPSILON);
        assert!((px[3] - 1.0).abs() <= f32::EPSILON);
        assert!(scratch.solid_fill.is_empty());
        assert!(scratch.media_effect.is_empty());
    }

    #[test]
    fn float_linear_compositor_runs_color_adjust_effect_without_rgba8_scratch() {
        let mut scratch = TimelineCompositeScratch::default();
        let media = CpuColorFrame::working(RgbaF32Frame {
            width: 1,
            height: 1,
            data: vec![[1.25, 0.25, 0.125, 1.0]],
            color_space: mondrian_core::types::ColorSpace::Rec709,
        });
        let frame = composite_timeline_elements_color_frame(
            1,
            1,
            &[TimelineCompositeElement::Media(TimelineMediaLayer {
                frame: &media,
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_graph: get_or_compile_scheduled_effect_graph(&EffectRenderPlan {
                    ops: vec![mondrian_effects::EffectRenderOp::ColorAdjust {
                        exposure: 1.0,
                        contrast: 1.0,
                        saturation: 1.0,
                    }],
                })
                .expect("compile float color adjust"),
                frame_seed: 0,
            })],
            TimelineCompositeOptions::default(),
            mondrian_core::types::ColorSpace::Rec709,
            &mut scratch,
        );

        let px = frame.rgba_f32().data[0];
        assert!((px[0] - 2.5).abs() <= 1.0e-6);
        assert!((px[1] - 0.5).abs() <= 1.0e-6);
        assert!((px[2] - 0.25).abs() <= 1.0e-6);
        assert!((px[3] - 1.0).abs() <= f32::EPSILON);
        assert!(scratch.media_source.is_empty());
        assert!(scratch.media_effect.is_empty());
    }

    #[test]
    fn float_linear_compositor_runs_normal_adjustment_without_rgba8_scratch() {
        let mut scratch = TimelineCompositeScratch::default();
        let media = CpuColorFrame::working(RgbaF32Frame {
            width: 1,
            height: 1,
            data: vec![[1.25, 0.25, 0.125, 1.0]],
            color_space: mondrian_core::types::ColorSpace::Rec709,
        });
        let frame = composite_timeline_elements_color_frame(
            1,
            1,
            &[
                identity_media(&media),
                TimelineCompositeElement::Adjustment(TimelineAdjustmentLayer {
                    effect_graph: get_or_compile_scheduled_effect_graph(&EffectRenderPlan {
                        ops: vec![mondrian_effects::EffectRenderOp::ColorAdjust {
                            exposure: 1.0,
                            contrast: 1.0,
                            saturation: 1.0,
                        }],
                    })
                    .expect("compile adjustment"),
                    opacity: 0.5,
                    blend_mode: Some(BlendMode::Normal),
                    frame_seed: 0,
                }),
            ],
            TimelineCompositeOptions::default(),
            mondrian_core::types::ColorSpace::Rec709,
            &mut scratch,
        );

        let px = frame.rgba_f32().data[0];
        assert!((px[0] - 1.875).abs() <= 1.0e-6);
        assert!((px[1] - 0.375).abs() <= 1.0e-6);
        assert!((px[2] - 0.1875).abs() <= 1.0e-6);
        assert!((px[3] - 1.0).abs() <= f32::EPSILON);
        assert!(scratch.media_source.is_empty());
        assert!(scratch.media_effect.is_empty());
        assert!(scratch.adjustment.is_empty());
    }

    #[test]
    fn float_linear_compositor_falls_back_for_legacy_adjustment_blend_modes() {
        let mut float_scratch = TimelineCompositeScratch::default();
        let mut legacy_scratch = TimelineCompositeScratch::default();
        let media = working_frame(&[255, 0, 0, 255], 1, 1);
        let elements = [
            identity_media(&media),
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
                blend_mode: Some(BlendMode::Multiply),
                frame_seed: 0,
            }),
        ];
        let float_output = composite_timeline_elements_color_frame(
            1,
            1,
            &elements,
            TimelineCompositeOptions::default(),
            mondrian_core::types::ColorSpace::Rec709,
            &mut float_scratch,
        )
        .to_output_rgba8(mondrian_core::types::ColorSpace::Rec709, false);
        let legacy_output = composite_timeline_elements(
            1,
            1,
            &elements,
            TimelineCompositeOptions::default(),
            &mut legacy_scratch,
        );
        assert_eq!(float_output, legacy_output);
        let diagnostics = composite_path_diagnostics(&elements);
        assert_eq!(diagnostics.legacy_rgba8_composites, 1);
        assert_eq!(diagnostics.float_linear_composites, 0);
        assert_eq!(diagnostics.legacy_adjustment_blend_mode, 1);
        assert!(diagnostics.uses_legacy_rgba8());
    }

    #[test]
    fn float_linear_compositor_falls_back_for_legacy_media_effects() {
        let mut float_scratch = TimelineCompositeScratch::default();
        let mut legacy_scratch = TimelineCompositeScratch::default();
        let media = working_frame(&[120, 80, 40, 255], 1, 1);
        let elements = [TimelineCompositeElement::Media(TimelineMediaLayer {
            frame: &media,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_graph: get_or_compile_scheduled_effect_graph(&EffectRenderPlan {
                ops: vec![mondrian_effects::EffectRenderOp::GaussianBlur { radius: 1.0 }],
            })
            .expect("compile media effect"),
            frame_seed: 0,
        })];

        let float_output = composite_timeline_elements_color_frame(
            1,
            1,
            &elements,
            TimelineCompositeOptions::default(),
            mondrian_core::types::ColorSpace::Rec709,
            &mut float_scratch,
        )
        .to_output_rgba8(mondrian_core::types::ColorSpace::Rec709, false);
        let legacy_output = composite_timeline_elements(
            1,
            1,
            &elements,
            TimelineCompositeOptions::default(),
            &mut legacy_scratch,
        );

        assert_eq!(float_output, legacy_output);
        let diagnostics = composite_path_diagnostics(&elements);
        assert_eq!(diagnostics.legacy_rgba8_composites, 1);
        assert_eq!(diagnostics.legacy_media_effect, 1);
    }

    #[test]
    fn float_linear_compositor_reports_clean_float_path_diagnostics() {
        let mut scratch = TimelineCompositeScratch::default();
        let media = working_frame(&[64, 128, 192, 255], 1, 1);
        let elements = [identity_media(&media)];

        let output = composite_timeline_elements_color_frame_with_diagnostics(
            1,
            1,
            &elements,
            TimelineCompositeOptions::default(),
            mondrian_core::types::ColorSpace::Rec709,
            &mut scratch,
        );

        assert_eq!(output.diagnostics.elements, 1);
        assert_eq!(output.diagnostics.float_linear_composites, 1);
        assert_eq!(output.diagnostics.legacy_rgba8_composites, 0);
        assert!(!output.diagnostics.uses_legacy_rgba8());
        assert!(scratch.media_source.is_empty());
    }
}
