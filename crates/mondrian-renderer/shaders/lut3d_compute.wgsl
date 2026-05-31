// 3D LUT color grading — compute shader.
// Uses textureLoad with manual trilinear interpolation (no sampler needed).
// LUT table is a 3D texture (size³, Rgba32Float).

@group(0) @binding(0) var input_tex: texture_2d<f32>;
@group(0) @binding(1) var output_tex: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(2) var lut_tex: texture_3d<f32>;

struct Uniforms {
    intensity: f32,  // 0.0 = passthrough, 1.0 = full LUT
    lut_size: f32,   // e.g., 33.0
}

@group(1) @binding(0) var<uniform> uniforms: Uniforms;

// Trilinear interpolation on a 3D texture using textureLoad with
// 8 nearest-neighbor samples and fractional blending.
fn sample_lut_3d(lut_coord: vec3<f32>, size: f32) -> vec4<f32> {
    // Map [0,1] to texel space [0.5, size-0.5] for safe bilinear interpolation
    let n = size;
    let coord_scaled = lut_coord * (n - 1.0);
    let base = floor(coord_scaled);
    let frac = coord_scaled - base;

    let b000 = vec3<i32>(i32(base.x), i32(base.y), i32(base.z));
    let b001 = b000 + vec3<i32>(0, 0, 1);
    let b010 = b000 + vec3<i32>(0, 1, 0);
    let b011 = b000 + vec3<i32>(0, 1, 1);
    let b100 = b000 + vec3<i32>(1, 0, 0);
    let b101 = b000 + vec3<i32>(1, 0, 1);
    let b110 = b000 + vec3<i32>(1, 1, 0);
    let b111 = b000 + vec3<i32>(1, 1, 1);

    let c000 = textureLoad(lut_tex, b000, 0);
    let c001 = textureLoad(lut_tex, b001, 0);
    let c010 = textureLoad(lut_tex, b010, 0);
    let c011 = textureLoad(lut_tex, b011, 0);
    let c100 = textureLoad(lut_tex, b100, 0);
    let c101 = textureLoad(lut_tex, b101, 0);
    let c110 = textureLoad(lut_tex, b110, 0);
    let c111 = textureLoad(lut_tex, b111, 0);

    let c00 = mix(c000, c100, frac.x);
    let c01 = mix(c001, c101, frac.x);
    let c10 = mix(c010, c110, frac.x);
    let c11 = mix(c011, c111, frac.x);

    let c0 = mix(c00, c10, frac.y);
    let c1 = mix(c01, c11, frac.y);

    return mix(c0, c1, frac.z);
}

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) global_id: vec3<u32>) {
    let dims = textureDimensions(input_tex);
    if global_id.x >= dims.x || global_id.y >= dims.y {
        return;
    }

    let color = textureLoad(input_tex, vec2<i32>(i32(global_id.x), i32(global_id.y)), 0);

    if uniforms.intensity <= 0.0 {
        textureStore(output_tex, vec2<i32>(i32(global_id.x), i32(global_id.y)), color);
        return;
    }

    let lut_color = sample_lut_3d(color.rgb, uniforms.lut_size);
    let result = mix(color, lut_color, uniforms.intensity);
    textureStore(output_tex, vec2<i32>(i32(global_id.x), i32(global_id.y)), result);
}
