struct TemporalControls {
    sigma_and_mix: vec4<f32>,
    // Motion scale, predecessor present, successor present, pad.
    sampling: vec4<f32>,
};

@group(0) @binding(0) var current_frame: texture_2d<f32>;
@group(0) @binding(1) var previous_frame: texture_2d<f32>;
@group(0) @binding(2) var following_frame: texture_2d<f32>;
@group(0) @binding(3) var<uniform> grade: TemporalControls;

@vertex
fn full_screen(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let corners = array(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    return vec4(corners[i], 0.0, 1.0);
}

fn finite_rgba(pixel: vec4<f32>) -> bool {
    let exponent_mask = vec4<u32>(0x7f800000u);
    return all((bitcast<vec4<u32>>(pixel) & exponent_mask) != exponent_mask);
}

fn correction_for_tap(center: vec4<f32>, tap: vec4<f32>) -> vec4<f32> {
    if !finite_rgba(tap) || tap.a <= 0.0 {
        return vec4<f32>(0.0);
    }
    let floor_sigma = 0.0001 * max(max(abs(center.rgb), abs(tap.rgb)), vec3<f32>(1.0));
    let delta = tap.rgb - center.rgb;
    let distances = abs(delta) / max(grade.sigma_and_mix.rgb, floor_sigma);
    let distance = max(distances.x, max(distances.y, distances.z));
    let t = clamp(distance / grade.sampling.x - 1.0, 0.0, 1.0);
    let gate = 1.0 - t * t * (3.0 - 2.0 * t);
    let weight = grade.sigma_and_mix.w * min(tap.a, 1.0) * gate;
    if weight <= 0.0 {
        return vec4<f32>(0.0);
    }
    return vec4<f32>(weight * delta, weight);
}

@fragment
fn fs_temporal_grain(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let index = vec2<i32>(position.xy);
    let center = textureLoad(current_frame, index, 0);
    if grade.sigma_and_mix.w == 0.0 || center.a <= 0.0 || !finite_rgba(center) {
        return center;
    }
    var correction = vec4<f32>(0.0);
    if grade.sampling.y != 0.0 {
        correction += correction_for_tap(center, textureLoad(previous_frame, index, 0));
    }
    if grade.sampling.z != 0.0 {
        correction += correction_for_tap(center, textureLoad(following_frame, index, 0));
    }
    let divisor = 1.0 + correction.a;
    if divisor == 1.0 {
        return center;
    }
    let filtered = center.rgb + correction.rgb / divisor;
    let invalid = (bitcast<vec3<u32>>(filtered) & vec3<u32>(0x7f800000u)) == vec3<u32>(0x7f800000u);
    return vec4<f32>(select(filtered, center.rgb, invalid), center.a);
}
