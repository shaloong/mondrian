@group(0) @binding(0) var source_texture: texture_2d<f32>;
// Output width, Detail, Alpha mode, Reduce Noise, then X and Y texel tables.
@group(0) @binding(1) var<storage, read> controls: array<u32>;

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let vertices = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0),
    );
    return vec4<f32>(vertices[index], 0.0, 1.0);
}

fn load_pixel(texel: vec2<i32>, last: vec2<i32>) -> vec4<f32> {
    return textureLoad(source_texture, clamp(texel, vec2<i32>(0), last), 0);
}

fn signal(pixel: vec4<f32>) -> f32 {
    let compressed = pixel.rgb / (vec3<f32>(1.0) + abs(pixel.rgb));
    return (0.2126 * compressed.r + 0.7152 * compressed.g + 0.0722 * compressed.b) * pixel.a;
}

fn contrast(a: vec4<f32>, b: vec4<f32>) -> f32 {
    return max(abs(signal(a) - signal(b)), abs(a.a - b.a));
}

fn covered_color(
    taps: array<vec4<f32>, 16>,
    weights: array<f32, 16>,
    count: u32,
) -> vec4<f32> {
    var max_alpha = 0.0;
    var magnitude = vec3<f32>(0.0);
    for (var i = 0u; i < count; i++) {
        if weights[i] > 0.0 && taps[i].a > 0.0 {
            max_alpha = max(max_alpha, taps[i].a);
            magnitude = max(magnitude, abs(taps[i].rgb));
        }
    }
    if max_alpha == 0.0 {
        return vec4<f32>(0.0);
    }
    let divisor = select(vec3<f32>(1.0), magnitude, magnitude > vec3<f32>(0.0));
    var coverage = 0.0;
    var color = vec3<f32>(0.0);
    for (var i = 0u; i < count; i++) {
        if weights[i] > 0.0 && taps[i].a > 0.0 {
            let covered = (taps[i].a / max_alpha) * weights[i];
            coverage += covered;
            color += (taps[i].rgb / divisor) * covered;
        }
    }
    return vec4<f32>(clamp(color / coverage, vec3<f32>(-1.0), vec3<f32>(1.0)) * magnitude, 1.0);
}

fn catmull_rom(t: f32) -> vec4<f32> {
    let t2 = t * t;
    let t3 = t2 * t;
    return vec4<f32>(
        -0.5 * t + t2 - 0.5 * t3,
        1.0 - 2.5 * t2 + 1.5 * t3,
        0.5 * t + 2.0 * t2 - 1.5 * t3,
        -0.5 * t2 + 0.5 * t3,
    );
}

fn b_spline(t: f32) -> vec4<f32> {
    let u = 1.0 - t;
    let t2 = t * t;
    let t3 = t2 * t;
    return vec4<f32>(
        u * u * u / 6.0,
        (4.0 - 6.0 * t2 + 3.0 * t3) / 6.0,
        (1.0 + 3.0 * t + 3.0 * t2 - 3.0 * t3) / 6.0,
        t3 / 6.0,
    );
}

@fragment
fn fs_upscale(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let texel = vec2<u32>(position.xy);
    let x_at = 4u + 2u * texel.x;
    let y_at = 4u + 2u * (controls[0] + texel.y);
    let base = vec2<i32>(i32(controls[x_at]), i32(controls[y_at]));
    let x = bitcast<f32>(controls[x_at + 1u]);
    let y = bitcast<f32>(controls[y_at + 1u]);
    let detail = bitcast<f32>(controls[1]);
    let last = vec2<i32>(textureDimensions(source_texture)) - vec2<i32>(1);
    var taps: array<vec4<f32>, 16>;
    for (var i = 0u; i < 16u; i++) {
        let offset = vec2<i32>(i32(i % 4u) - 1, i32(i / 4u) - 1);
        taps[i] = load_pixel(base + offset, last);
    }
    let central_indices = array<u32, 4>(5u, 6u, 9u, 10u);
    let bilinear = vec4<f32>((1.0 - x) * (1.0 - y), x * (1.0 - y), (1.0 - x) * y, x * y);
    var min_alpha = 1.0;
    var max_alpha = 0.0;
    var alpha = 0.0;
    for (var i = 0u; i < 4u; i++) {
        let coverage = taps[central_indices[i]].a;
        min_alpha = min(min_alpha, coverage);
        max_alpha = max(max_alpha, coverage);
        alpha += coverage * bilinear[i];
    }
    if controls[2] != 0u {
        let wx = catmull_rom(x);
        let wy = catmull_rom(y);
        alpha = 0.0;
        for (var i = 0u; i < 16u; i++) {
            alpha += taps[i].a * (wx[i % 4u] * wy[i / 4u]);
        }
    }
    alpha = clamp(alpha, min_alpha, max_alpha);
    if alpha == 0.0 {
        return vec4<f32>(0.0);
    }
    let descending = contrast(taps[5], taps[10]);
    let ascending = contrast(taps[6], taps[9]);
    let confidence = abs(descending - ascending) / (descending + ascending + 1e-5);
    var guided: vec4<f32>;
    if descending < ascending {
        guided = vec4<f32>(1.0 - max(x, y), max(x - y, 0.0), max(y - x, 0.0), min(x, y));
    } else {
        guided = vec4<f32>(
            max(1.0 - x - y, 0.0), min(x, 1.0 - y), min(y, 1.0 - x), max(x + y - 1.0, 0.0),
        );
    }
    let guide = detail * confidence;
    let smoothing = (1.0 - detail) * (1.0 - detail);
    let wx = b_spline(x);
    let wy = b_spline(y);
    var weights: array<f32, 16>;
    for (var i = 0u; i < 16u; i++) {
        weights[i] = (wx[i % 4u] * wy[i / 4u]) * smoothing;
    }
    for (var i = 0u; i < 4u; i++) {
        weights[central_indices[i]] += (1.0 - smoothing) * (bilinear[i] * (1.0 - guide) + guided[i] * guide);
    }
    var rgb = covered_color(taps, weights, 16u);
    if rgb.a == 0.0 {
        for (var i = 0u; i < 16u; i++) {
            weights[i] = 0.0;
        }
        for (var i = 0u; i < 4u; i++) {
            weights[central_indices[i]] = bilinear[i];
        }
        rgb = covered_color(taps, weights, 16u);
    }
    return vec4<f32>(rgb.rgb, alpha);
}

@fragment
fn fs_denoise(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let texel = vec2<i32>(position.xy);
    let last = vec2<i32>(textureDimensions(source_texture)) - vec2<i32>(1);
    let center = load_pixel(texel, last);
    if center.a <= 0.0 {
        return vec4<f32>(0.0);
    }
    let spatial = array<f32, 9>(1.0, 2.0, 1.0, 2.0, 4.0, 2.0, 1.0, 2.0, 1.0);
    var taps: array<vec4<f32>, 16>;
    var weights: array<f32, 16>;
    for (var i = 0u; i < 9u; i++) {
        let offset = vec2<i32>(i32(i % 3u) - 1, i32(i / 3u) - 1);
        taps[i] = load_pixel(texel + offset, last);
        let range = max(1.0 - 4.0 * contrast(center, taps[i]), 0.0);
        weights[i] = spatial[i] * range * range;
    }
    let strength = bitcast<f32>(controls[3]);
    let filtered = covered_color(taps, weights, 9u).rgb;
    let rgb = clamp(
        center.rgb * (1.0 - strength) + filtered * strength,
        min(center.rgb, filtered),
        max(center.rgb, filtered),
    );
    return vec4<f32>(rgb, center.a);
}
