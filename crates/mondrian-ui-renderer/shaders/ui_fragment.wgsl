// UI 2D fragment shader
// Shape mode: pixel-space rounded-rect SDF with analytic AA.
// Glyph mode: texture sampling.

const RENDER_MODE_SHAPE: u32 = 0u;
const RENDER_MODE_GLYPH: u32 = 1u;
const RENDER_MODE_LINE: u32 = 2u;
const RENDER_MODE_IMAGE: u32 = 3u;
const RENDER_MODE_SOFT_SHADOW: u32 = 4u;

struct UiUniforms {
    screen_size: vec2<f32>,
    ui_to_surface_row_0: vec4<f32>,
    ui_to_surface_row_1: vec4<f32>,
    ui_to_surface_row_2: vec4<f32>,
    external_decode_mode: u32,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) tex_coord: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) @interpolate(flat) rect_size: vec2<f32>,
    @location(3) @interpolate(flat) corner_radius_px: vec4<f32>,
    @location(4) @interpolate(flat) render_mode: u32,
    @location(5) @interpolate(flat) blur_radius_px: f32,
};

@group(1) @binding(0) var glyph_sampler: sampler;
@group(1) @binding(1) var glyph_texture: texture_2d<f32>;
@group(0) @binding(0) var<uniform> ui_uniforms: UiUniforms;

fn ui_linear_to_surface_linear(value: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        dot(ui_uniforms.ui_to_surface_row_0.xyz, value),
        dot(ui_uniforms.ui_to_surface_row_1.xyz, value),
        dot(ui_uniforms.ui_to_surface_row_2.xyz, value),
    );
}

fn ui_output(value: vec4<f32>) -> vec4<f32> {
    return vec4<f32>(ui_linear_to_surface_linear(value.rgb), value.a);
}

/// Uniform corner radius (original path for shadows & lines).
fn sd_rounded_box_px(p: vec2<f32>, size: vec2<f32>, r: f32) -> f32 {
    let half = size * 0.5;
    let q = abs(p - half) - half + r;
    return length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - r;
}

/// Per-corner radius variant. Uses the quadrant of `p` relative to the
/// rectangle centre to select the appropriate radius.
///
/// Packing:  radii.x = top-left,  y = top-right,  z = bottom-right,  w = bottom-left
fn sd_rounded_box_per_corner(p: vec2<f32>, size: vec2<f32>, radii: vec4<f32>) -> f32 {
    let half = size * 0.5;

    var r: f32;
    if p.x < half.x {
        if p.y < half.y {
            r = radii.x;   // top-left
        } else {
            r = radii.w;   // bottom-left
        }
    } else {
        if p.y < half.y {
            r = radii.y;   // top-right
        } else {
            r = radii.z;   // bottom-right
        }
    }

    let q = abs(p - half) - half + r;
    return length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - r;
}

fn sd_segment_px(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> f32 {
    let pa = p - a;
    let ba = b - a;
    let h = clamp(dot(pa, ba) / max(dot(ba, ba), 0.000001), 0.0, 1.0);
    return length(pa - ba * h);
}

@fragment
fn main(in: VertexOutput) -> @location(0) vec4<f32> {
    if in.render_mode == RENDER_MODE_GLYPH {
        let sampled = textureSample(glyph_texture, glyph_sampler, in.tex_coord);
        return ui_output(vec4<f32>(in.color.rgb, in.color.a * sampled.a));
    }

    if in.render_mode == RENDER_MODE_IMAGE {
        let sampled = textureSample(glyph_texture, glyph_sampler, in.tex_coord);
        return ui_output(sampled * in.color);
    }

    if in.render_mode == RENDER_MODE_LINE {
        let radius = max(in.corner_radius_px.x, 0.0);
        let center_y = in.rect_size.y * 0.5;
        let axis_padding = center_y;
        let a = vec2<f32>(axis_padding, center_y);
        let b = vec2<f32>(max(in.rect_size.x - axis_padding, axis_padding), center_y);
        let d = sd_segment_px(in.tex_coord, a, b) - radius;
        let aa = clamp(fwidth(d), 0.75, 1.5);
        let alpha = smoothstep(aa * 0.5, -aa * 0.5, d);
        if alpha <= 0.001 { discard; }
        return ui_output(vec4<f32>(in.color.rgb, in.color.a * alpha));
    }

    if in.render_mode == RENDER_MODE_SOFT_SHADOW {
        let r = clamp(in.corner_radius_px.x, 0.0, min(in.rect_size.x, in.rect_size.y) * 0.5);
        let d = sd_rounded_box_px(in.tex_coord, in.rect_size, r);
        let blur = max(in.blur_radius_px, 0.001);
        let outside = max(d, 0.0);
        // Cubic falloff for smooth transition
        let falloff = 1.0 - smoothstep(0.0, blur, outside);
        let alpha = in.color.a * falloff * falloff * falloff;
        if alpha <= 0.001 { discard; }
        return ui_output(vec4<f32>(in.color.rgb, alpha));
    }

    // RENDER_MODE_SHAPE
    let clamp_half = min(in.rect_size.x, in.rect_size.y) * 0.5;
    let r = clamp(in.corner_radius_px, vec4(0.0), vec4(clamp_half));
    let all_zero = r.x <= 0.0 && r.y <= 0.0 && r.z <= 0.0 && r.w <= 0.0;
    if all_zero { return ui_output(in.color); }

    let p = in.tex_coord * in.rect_size;
    let d = sd_rounded_box_per_corner(p, in.rect_size, r);
    let aa = clamp(fwidth(d), 0.75, 1.5);
    let alpha = smoothstep(aa * 0.5, -aa * 0.5, d);
    if alpha <= 0.001 { discard; }
    return ui_output(vec4<f32>(in.color.rgb, in.color.a * alpha));
}

fn srgb_carrier_to_linear(value: vec3<f32>) -> vec3<f32> {
    let low = value / vec3<f32>(12.92);
    let high = pow((value + vec3<f32>(0.055)) / vec3<f32>(1.055), vec3<f32>(2.4));
    return select(high, low, value <= vec3<f32>(0.04045));
}

fn pq_to_linear_100_nits(value: vec3<f32>) -> vec3<f32> {
    let m1 = 2610.0 / 16384.0;
    let m2 = 2523.0 / 32.0;
    let c1 = 3424.0 / 4096.0;
    let c2 = 2413.0 / 128.0;
    let c3 = 2392.0 / 128.0;
    let powered = pow(clamp(value, vec3<f32>(0.0), vec3<f32>(1.0)), vec3<f32>(1.0 / m2));
    let numerator = max(powered - vec3<f32>(c1), vec3<f32>(0.0));
    let denominator = max(vec3<f32>(c2) - vec3<f32>(c3) * powered, vec3<f32>(0.000001));
    return pow(numerator / denominator, vec3<f32>(1.0 / m1)) * vec3<f32>(100.0);
}

fn hlg_to_linear_100_nits(value: vec3<f32>) -> vec3<f32> {
    let a = 0.17883277;
    let b = 0.28466892;
    let c = 0.55991073;
    let low = value * value / vec3<f32>(3.0);
    let high = (exp((value - vec3<f32>(c)) / vec3<f32>(a)) + vec3<f32>(b)) / vec3<f32>(12.0);
    let scene_linear = select(high, low, value <= vec3<f32>(0.5));
    return pow(max(scene_linear, vec3<f32>(0.0)), vec3<f32>(1.2)) * vec3<f32>(10.0);
}

fn surface_code_to_linear(value: vec3<f32>) -> vec3<f32> {
    if ui_uniforms.external_decode_mode == 1u {
        return pq_to_linear_100_nits(value);
    }
    if ui_uniforms.external_decode_mode == 2u {
        return hlg_to_linear_100_nits(value);
    }
    return srgb_carrier_to_linear(value);
}

fn sample_surface_code_linear(uv: vec2<f32>) -> vec3<f32> {
    let dimensions = vec2<i32>(textureDimensions(glyph_texture));
    let texel_position = uv * vec2<f32>(dimensions) - vec2<f32>(0.5);
    let base = vec2<i32>(floor(texel_position));
    let fraction = fract(texel_position);
    let maximum = dimensions - vec2<i32>(1);
    let p00 = clamp(base, vec2<i32>(0), maximum);
    let p10 = clamp(base + vec2<i32>(1, 0), vec2<i32>(0), maximum);
    let p01 = clamp(base + vec2<i32>(0, 1), vec2<i32>(0), maximum);
    let p11 = clamp(base + vec2<i32>(1, 1), vec2<i32>(0), maximum);
    let c00 = surface_code_to_linear(clamp(textureLoad(glyph_texture, p00, 0).rgb, vec3<f32>(0.0), vec3<f32>(1.0)));
    let c10 = surface_code_to_linear(clamp(textureLoad(glyph_texture, p10, 0).rgb, vec3<f32>(0.0), vec3<f32>(1.0)));
    let c01 = surface_code_to_linear(clamp(textureLoad(glyph_texture, p01, 0).rgb, vec3<f32>(0.0), vec3<f32>(1.0)));
    let c11 = surface_code_to_linear(clamp(textureLoad(glyph_texture, p11, 0).rgb, vec3<f32>(0.0), vec3<f32>(1.0)));
    return mix(mix(c00, c10, fraction.x), mix(c01, c11, fraction.x), fraction.y);
}

// Decode opaque Viewer target code values into the target-primary linear
// composition. This path deliberately does not apply the UI-primary matrix.
@fragment
fn main_encoded_code_values(in: VertexOutput) -> @location(0) vec4<f32> {
    return vec4<f32>(sample_surface_code_linear(in.tex_coord), 1.0);
}

// ICC/device code values have no target-colorimetry transfer to invert. The
// direct sRGB attachment OETF reverses this carrier decode after code-space
// filtering, preserving the opaque device payload.
@fragment
fn main_device_code_values(in: VertexOutput) -> @location(0) vec4<f32> {
    let sampled = textureSample(glyph_texture, glyph_sampler, in.tex_coord);
    let code_value = clamp(sampled.rgb, vec3<f32>(0.0), vec3<f32>(1.0));
    return vec4<f32>(srgb_carrier_to_linear(code_value), 1.0);
}
