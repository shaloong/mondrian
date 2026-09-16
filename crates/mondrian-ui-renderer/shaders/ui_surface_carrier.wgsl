// Final carrier from target-primary display-linear UI composition to the
// configured native surface. HDR composition values use 100 nits == 1.0.

@group(0) @binding(0) var composition_texture: texture_2d<f32>;

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> @builtin(position) vec4<f32> {
    let x = f32(i32(vertex_index & 1u) * 4 - 1);
    let y = f32(i32(vertex_index >> 1u) * 4 - 1);
    return vec4<f32>(x, y, 0.0, 1.0);
}

fn composition_sample(position: vec4<f32>) -> vec4<f32> {
    return textureLoad(composition_texture, vec2<i32>(position.xy), 0);
}

fn linear_nits_to_pq(linear_100_nits: vec3<f32>) -> vec3<f32> {
    let m1 = 2610.0 / 16384.0;
    let m2 = 2523.0 / 32.0;
    let c1 = 3424.0 / 4096.0;
    let c2 = 2413.0 / 128.0;
    let c3 = 2392.0 / 128.0;
    let normalized = clamp(linear_100_nits / vec3<f32>(100.0), vec3<f32>(0.0), vec3<f32>(1.0));
    let powered = pow(normalized, vec3<f32>(m1));
    return pow((vec3<f32>(c1) + vec3<f32>(c2) * powered) /
               (vec3<f32>(1.0) + vec3<f32>(c3) * powered), vec3<f32>(m2));
}

fn hlg_oetf(scene_linear: vec3<f32>) -> vec3<f32> {
    let a = 0.17883277;
    let b = 0.28466892;
    let c = 0.55991073;
    let low = sqrt(vec3<f32>(3.0) * scene_linear);
    let high = vec3<f32>(a) * log(vec3<f32>(12.0) * scene_linear - vec3<f32>(b)) + vec3<f32>(c);
    return select(high, low, scene_linear <= vec3<f32>(1.0 / 12.0));
}

fn linear_nits_to_hlg(linear_100_nits: vec3<f32>) -> vec3<f32> {
    let display_relative = clamp(linear_100_nits / vec3<f32>(10.0), vec3<f32>(0.0), vec3<f32>(1.0));
    let scene_linear = pow(display_relative, vec3<f32>(1.0 / 1.2));
    return hlg_oetf(scene_linear);
}

@fragment
fn fs_linear(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    return composition_sample(position);
}

@fragment
fn fs_pq(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let sample = composition_sample(position);
    return vec4<f32>(linear_nits_to_pq(sample.rgb), sample.a);
}

@fragment
fn fs_hlg(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let sample = composition_sample(position);
    return vec4<f32>(linear_nits_to_hlg(sample.rgb), sample.a);
}
