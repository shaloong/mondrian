// UI 2D fragment shader
// Rounded rects + texture sampling via corner_radius sentinel.

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) tex_coord: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) corner_radius: f32,
    @location(3) local_pos: vec2<f32>,
};

@group(1) @binding(0) var glyph_sampler: sampler;
@group(1) @binding(1) var glyph_texture: texture_2d<f32>;

@fragment
fn main(in: VertexOutput) -> @location(0) vec4<f32> {
    // Texture mode: corner_radius < 0 means Image command (glyph)
    if in.corner_radius < 0.0 {
        
        let sampled = textureSample(glyph_texture, glyph_sampler, in.tex_coord);
        return vec4<f32>(in.color.rgb, in.color.a * sampled.a);
    }

    // Rounded rect mode: corner_radius > 0
    let radius = in.corner_radius;
    if radius > 0.0 {
        let p = in.local_pos;
        let corner = vec2<f32>(
            select(p.x, 1.0 - p.x, p.x > 0.5),
            select(p.y, 1.0 - p.y, p.y > 0.5),
        );
        let dist = length(corner);
        if dist > 1.0 {
            discard;
        }
    }
    return in.color;
}
