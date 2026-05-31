// 3D LUT color grading — compute shader
// Applies a 3D lookup table to transform colors.
// Supports 16x16x16, 33x33x33, and 65x65x65 cube sizes.

@group(0) @binding(0) var input_tex: texture_2d<f32>;
@group(0) @binding(1) var output_tex: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(2) var lut_tex: texture_3d<f32>;
@group(0) @binding(3) var lut_sampler: sampler;

struct Uniforms {
    intensity: f32,   // 0.0 = passthrough, 1.0 = full LUT
    lut_size: f32,    // 16.0, 33.0, or 65.0
}

@group(1) @binding(0) var<uniform> uniforms: Uniforms;

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) global_id: vec3<u32>) {
    let dims = textureDimensions(input_tex);
    if global_id.x >= dims.x || global_id.y >= dims.y {
        return;
    }

    let uv = vec2<f32>(f32(global_id.x), f32(global_id.y));
    let color = textureLoad(input_tex, vec2<i32>(i32(global_id.x), i32(global_id.y)), 0);

    if uniforms.intensity <= 0.0 {
        textureStore(output_tex, vec2<i32>(i32(global_id.x), i32(global_id.y)), color);
        return;
    }

    // Convert input RGB to LUT texture coordinates.
    // LUT domain: [0, 1] → [0.5/size, 1 - 0.5/size] for safe interpolation.
    let n = f32(uniforms.lut_size);
    let half_texel = 0.5 / n;
    let scale = 1.0 - 1.0 / n;
    let r = half_texel + color.r * scale;
    let g = half_texel + color.g * scale;
    let b = half_texel + color.b * scale;

    let lut_color = textureSample(lut_tex, lut_sampler, vec3<f32>(r, g, b));

    let result = mix(color, lut_color, uniforms.intensity);
    textureStore(output_tex, vec2<i32>(i32(global_id.x), i32(global_id.y)), result);
}
