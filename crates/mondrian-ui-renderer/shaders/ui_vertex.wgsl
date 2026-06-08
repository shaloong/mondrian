// UI 2D vertex shader

struct Uniforms {
    screen_size: vec2<f32>,
    aa_floor: f32,
    _pad: f32,
};
@group(0) @binding(0) var<uniform> uniforms: Uniforms;

struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) tex_coord: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) rect_size: vec2<f32>,
    @location(4) corner_radius_px: f32,
    @location(5) render_mode: u32,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) tex_coord: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) @interpolate(flat) rect_size: vec2<f32>,
    @location(3) @interpolate(flat) corner_radius_px: f32,
    @location(4) local_px: vec2<f32>,
    @location(5) @interpolate(flat) render_mode: u32,
    @location(6) @interpolate(flat) aa_floor: f32,
};

@vertex
fn main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.clip_position = vec4<f32>(in.position, 0.0, 1.0);
    out.tex_coord = in.tex_coord;
    out.color = in.color;
    out.rect_size = in.rect_size;
    out.corner_radius_px = in.corner_radius_px;
    out.local_px = in.tex_coord * in.rect_size;
    out.render_mode = in.render_mode;
    let ref_height = 1080.0;
    out.aa_floor = clamp(ref_height / uniforms.screen_size.y, 0.5, 1.5);
    return out;
}