// UI 2D fragment shader
// Draws rounded rectangles by discarding fragments outside the rounded corner radius.

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) tex_coord: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) corner_radius: f32,
    @location(3) local_pos: vec2<f32>,
};

@fragment
fn main(in: VertexOutput) -> @location(0) vec4<f32> {
    let radius = in.corner_radius;
    if radius > 0.0 {
        let p = in.tex_coord;
        // Distance from nearest corner
        let corner = vec2<f32>(
            select(p.x, 1.0 - p.x, p.x > 0.5),
            select(p.y, 1.0 - p.y, p.y > 0.5),
        );
        let dist = length(corner) * 0.5;
        if dist > radius * 0.5 {
            discard;
        }
    }
    return in.color;
}
