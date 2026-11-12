// The host supplies working-space luminance coefficients and the range scale.
fn signal_value(rgba: vec4<f32>, mode: u32, luma: vec3<f32>) -> f32 {
    if (mode == 3u) {
        return fma(rgba.b, luma.z, fma(rgba.g, luma.y, rgba.r * luma.x));
    }
    return rgba[mode % 4u];
}

fn sample_signal(rgba: vec4<f32>, mode: u32, range_scale: f32, tail: f32, luma: vec3<f32>) -> vec3<f32> {
    let value = signal_value(rgba, mode, luma);
    let magnitude = abs(value);
    let offset = 1.0 + tail;
    var coordinate = value + offset;
    if (magnitude > 1.0) {
        let excess = magnitude - 1.0;
        let direction = sign(value);
        coordinate = fma(direction * tail, excess / (tail + excess), offset + direction);
    }
    return vec3<f32>(value, rgba.a, coordinate * range_scale);
}

fn finite_signal(value: f32) -> bool {
    return abs(value) <= 3.402823466e+38;
}
