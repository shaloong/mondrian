// GPU color space conversion — compute shader
// Applies gamma decode + primaries matrix + gamma encode in a single pass.
// Handles the common case: Rec709/sRGB source → display profile.

@group(0) @binding(0) var input_tex: texture_2d<f32>;
@group(0) @binding(1) var output_tex: texture_storage_2d<rgba8unorm, write>;

struct ColorParams {
    decode_gamma: f32,    // source transfer gamma (e.g., Rec709 = 2.4, sRGB ≈ 2.2)
    display_matrix_0: vec3<f32>,
    display_matrix_1: vec3<f32>,
    display_matrix_2: vec3<f32>,
    display_gamma: f32,   // profile gamma
    encode_gamma: f32,    // profile color space transfer gamma
}

@group(1) @binding(0) var<uniform> params: ColorParams;

fn decode_gamma(rgb: vec3<f32>, gamma: f32) -> vec3<f32> {
    if gamma <= 0.001 {
        return rgb;
    }
    return pow(max(rgb, vec3<f32>(0.0)), vec3<f32>(1.0 / gamma));
}

fn encode_gamma(rgb: vec3<f32>, gamma: f32) -> vec3<f32> {
    if gamma <= 0.001 {
        return rgb;
    }
    return pow(max(rgb, vec3<f32>(0.0)), vec3<f32>(gamma));
}

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) global_id: vec3<u32>) {
    let dims = textureDimensions(input_tex);
    if global_id.x >= dims.x || global_id.y >= dims.y {
        return;
    }

    let coord = vec2<i32>(i32(global_id.x), i32(global_id.y));
    let color = textureLoad(input_tex, coord, 0);

    // 1. Decode: source transfer → linear
    var rgb = decode_gamma(color.rgb, params.decode_gamma);

    // 2. Display primaries matrix
    let m = mat3x3<f32>(
        params.display_matrix_0,
        params.display_matrix_1,
        params.display_matrix_2,
    );
    rgb = m * rgb;

    // 3. Display gamma
    rgb = encode_gamma(max(rgb, vec3<f32>(0.0)), params.display_gamma);

    // 4. Encode: linear → profile color space transfer
    let encoded = encode_gamma(rgb, params.encode_gamma);

    textureStore(output_tex, coord, vec4<f32>(encoded, color.a));
}
