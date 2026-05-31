// Color adjustment — compute shader
// Exposure, contrast, and saturation in a single pass.

@group(0) @binding(0) var input_tex: texture_2d<f32>;
@group(0) @binding(1) var output_tex: texture_storage_2d<rgba8unorm, write>;

struct Uniforms {
    exposure: f32,    // stops, 0.0 = no change
    contrast: f32,    // 1.0 = no change
    saturation: f32,  // 1.0 = no change
}

@group(1) @binding(0) var<uniform> uniforms: Uniforms;

// Convert linear RGB to luminance
fn rgb_to_luminance(color: vec3<f32>) -> f32 {
    return dot(color, vec3<f32>(0.2126, 0.7152, 0.0722));
}

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) global_id: vec3<u32>) {
    let dims = textureDimensions(input_tex);
    if global_id.x >= dims.x || global_id.y >= dims.y {
        return;
    }

    let color = textureLoad(input_tex, vec2<i32>(i32(global_id.x), i32(global_id.y)), 0);

    var rgb = color.rgb;

    // Exposure: multiply by 2^exposure
    let exposure_factor = exp2(uniforms.exposure);
    rgb *= exposure_factor;

    // Contrast: (color - 0.5) * contrast + 0.5
    let contrast_factor = uniforms.contrast;
    rgb = (rgb - 0.5) * contrast_factor + 0.5;

    // Saturation: lerp between luminance and color
    let saturation_factor = uniforms.saturation;
    let lum = rgb_to_luminance(rgb);
    rgb = mix(vec3<f32>(lum), rgb, saturation_factor);

    let result = vec4<f32>(rgb, color.a);
    textureStore(output_tex, vec2<i32>(i32(global_id.x), i32(global_id.y)), result);
}
