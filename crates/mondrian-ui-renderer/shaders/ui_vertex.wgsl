// UI 2D vertex shader
// Transforms pixel coordinates to NDC and passes through vertex color + tex coords.

struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) tex_coord: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) corner_radius: f32,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) tex_coord: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) corner_radius: f32,
    @location(3) local_pos: vec2<f32>,
};

struct Uniforms {
    screen_size: vec2<f32>,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;

@vertex
fn main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.clip_position = vec4<f32>(in.position, 0.0, 1.0);
    out.tex_coord = in.tex_coord;
    out.color = in.color;
    out.corner_radius = in.corner_radius;
    out.local_pos = in.tex_coord;
    return out;
}
