// Separable Gaussian blur — compute shader.
// Two-pass: horizontal then vertical, with configurable radius.
// Uses textureLoad for GPU-side neighbor sampling (no sampler needed).

@group(0) @binding(0) var input_tex: texture_2d<f32>;
@group(0) @binding(1) var output_tex: texture_storage_2d<rgba8unorm, write>;

struct Uniforms {
    radius: f32,
    dir_x: f32,    // 1 for horizontal, 0 for vertical
    dir_y: f32,    // 0 for horizontal, 1 for vertical
}

@group(1) @binding(0) var<uniform> uniforms: Uniforms;

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) global_id: vec3<u32>) {
    let dims = textureDimensions(input_tex);
    if global_id.x >= dims.x || global_id.y >= dims.y {
        return;
    }

    let cx = i32(global_id.x);
    let cy = i32(global_id.y);

    if uniforms.radius <= 0.0 {
        let color = textureLoad(input_tex, vec2<i32>(cx, cy), 0);
        textureStore(output_tex, vec2<i32>(cx, cy), color);
        return;
    }

    let r = uniforms.radius;
    let sigma = r * 0.5 + 0.5;
    let samples = i32(ceil(r * 3.0));

    var color = vec4<f32>(0.0);
    var weight_sum = 0.0;

    for (var i = -samples; i <= samples; i++) {
        let sx = cx + i32(round(f32(i) * uniforms.dir_x));
        let sy = cy + i32(round(f32(i) * uniforms.dir_y));
        // Clamp to texture bounds
        let cx_clamped = clamp(sx, 0, i32(dims.x) - 1);
        let cy_clamped = clamp(sy, 0, i32(dims.y) - 1);
        let sample_color = textureLoad(input_tex, vec2<i32>(cx_clamped, cy_clamped), 0);
        let gaussian_weight = exp(-(f32(i) * f32(i)) / (2.0 * sigma * sigma));
        color += sample_color * gaussian_weight;
        weight_sum += gaussian_weight;
    }

    let result = color / max(weight_sum, 0.0001);
    textureStore(output_tex, vec2<i32>(cx, cy), result);
}
