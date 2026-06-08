// UI 2D fragment shader
// Shape mode: pixel-space rounded-rect SDF with analytic AA.
// Glyph mode: texture sampling.

const RENDER_MODE_SHAPE: u32 = 0u;
const RENDER_MODE_GLYPH: u32 = 1u;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) tex_coord: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) @interpolate(flat) rect_size: vec2<f32>,
    @location(3) @interpolate(flat) corner_radius_px: f32,
    @location(4) @interpolate(flat) render_mode: u32,
};

@group(1) @binding(0) var glyph_sampler: sampler;
@group(1) @binding(1) var glyph_texture: texture_2d<f32>;

fn sd_rounded_box_px(p: vec2<f32>, size: vec2<f32>, r: f32) -> f32 {
    let half = size * 0.5;
    let q = abs(p - half) - half + r;
    return length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - r;
}

@fragment
fn main(in: VertexOutput) -> @location(0) vec4<f32> {
    if in.render_mode == RENDER_MODE_GLYPH {
        let sampled = textureSample(glyph_texture, glyph_sampler, in.tex_coord);
        return vec4<f32>(in.color.rgb, in.color.a * sampled.a);
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