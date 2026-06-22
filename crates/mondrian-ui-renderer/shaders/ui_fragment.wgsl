// UI 2D fragment shader
// Shape mode: pixel-space rounded-rect SDF with analytic AA.
// Glyph mode: texture sampling.

const RENDER_MODE_SHAPE: u32 = 0u;
const RENDER_MODE_GLYPH: u32 = 1u;
const RENDER_MODE_LINE: u32 = 2u;
const RENDER_MODE_IMAGE: u32 = 3u;
const RENDER_MODE_SOFT_SHADOW: u32 = 4u;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) tex_coord: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) @interpolate(flat) rect_size: vec2<f32>,
    @location(3) @interpolate(flat) corner_radius_px: f32,
    @location(4) @interpolate(flat) render_mode: u32,
    @location(5) @interpolate(flat) blur_radius_px: f32,
};

@group(1) @binding(0) var glyph_sampler: sampler;
@group(1) @binding(1) var glyph_texture: texture_2d<f32>;

fn sd_rounded_box_px(p: vec2<f32>, size: vec2<f32>, r: f32) -> f32 {
    let half = size * 0.5;
    let q = abs(p - half) - half + r;
    return length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - r;
}

fn sd_segment_px(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> f32 {
    let pa = p - a;
    let ba = b - a;
    let h = clamp(dot(pa, ba) / max(dot(ba, ba), 0.000001), 0.0, 1.0);
    return length(pa - ba * h);
}

@fragment
fn main(in: VertexOutput) -> @location(0) vec4<f32> {
    if in.render_mode == RENDER_MODE_GLYPH {
        let sampled = textureSample(glyph_texture, glyph_sampler, in.tex_coord);
        return vec4<f32>(in.color.rgb, in.color.a * sampled.a);
    }

    if in.render_mode == RENDER_MODE_IMAGE {
        let sampled = textureSample(glyph_texture, glyph_sampler, in.tex_coord);
        return sampled * in.color;
    }

    if in.render_mode == RENDER_MODE_LINE {
        let radius = max(in.corner_radius_px, 0.0);
        let center_y = in.rect_size.y * 0.5;
        let axis_padding = center_y;
        let a = vec2<f32>(axis_padding, center_y);
        let b = vec2<f32>(max(in.rect_size.x - axis_padding, axis_padding), center_y);
        let d = sd_segment_px(in.tex_coord, a, b) - radius;
        let aa = clamp(fwidth(d), 0.75, 1.5);
        let alpha = smoothstep(aa * 0.5, -aa * 0.5, d);
        if alpha <= 0.001 { discard; }
        return vec4<f32>(in.color.rgb, in.color.a * alpha);
    }

    if in.render_mode == RENDER_MODE_SOFT_SHADOW {
        let r = clamp(in.corner_radius_px, 0.0, min(in.rect_size.x, in.rect_size.y) * 0.5);
        let d = sd_rounded_box_px(in.tex_coord, in.rect_size, r);
        let blur = max(in.blur_radius_px, 0.001);
        let outside = max(d, 0.0);
        let falloff = 1.0 - smoothstep(0.0, blur, outside);
        let alpha = in.color.a * falloff * falloff;
        if alpha <= 0.001 { discard; }
        return vec4<f32>(in.color.rgb, alpha);
    }

    let r = clamp(in.corner_radius_px, 0.0, min(in.rect_size.x, in.rect_size.y) * 0.5);
    if r <= 0.0 { return in.color; }

    let p = in.tex_coord * in.rect_size;
    let d = sd_rounded_box_px(p, in.rect_size, r);
    let aa = clamp(fwidth(d), 0.75, 1.5);
    let alpha = smoothstep(aa * 0.5, -aa * 0.5, d);
    if alpha <= 0.001 { discard; }
    return vec4<f32>(in.color.rgb, in.color.a * alpha);
}
