// UI 2D vertex shader

struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) tex_coord: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) rect_size: vec2<f32>,
    @location(4) corner_radius_px: vec4<f32>,
    @location(5) render_mode: u32,
    @location(6) blur_radius_px: f32,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) tex_coord: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) @interpolate(flat) rect_size: vec2<f32>,
    @location(3) @interpolate(flat) corner_radius_px: vec4<f32>,
    @location(4) @interpolate(flat) render_mode: u32,
    @location(5) @interpolate(flat) blur_radius_px: f32,
};

@vertex
fn main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.clip_position = vec4<f32>(in.position, 0.0, 1.0);
    out.tex_coord = in.tex_coord;
    out.color = in.color;
    out.rect_size = in.rect_size;
    out.corner_radius_px = in.corner_radius_px;
    out.render_mode = in.render_mode;
    out.blur_radius_px = in.blur_radius_px;
    return out;
}
