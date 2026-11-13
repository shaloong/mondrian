struct Control {
    surface: vec4<f32>,
    material: vec4<f32>,
    sampling: vec4<f32>,
    affine_x: vec4<f32>,
    affine_y: vec4<f32>,
    normal_map: vec4<f32>,
    ambient: vec4<f32>,
    original: vec4<f32>,
    auxiliary_x: vec4<f32>,
    auxiliary_y: vec4<f32>,
    stage: vec4<f32>,
}

struct Light {
    color_kind: vec4<f32>,
    position_falloff: vec4<f32>,
    direction_radius: vec4<f32>,
    range_cone: vec4<f32>,
}

@group(0) @binding(0) var original: texture_2d<f32>;
@group(0) @binding(1) var plane: texture_2d<f32>;
@group(0) @binding(2) var bump_map: texture_2d<f32>;
@group(0) @binding(3) var<uniform> controls: Control;
@group(0) @binding(4) var<storage, read> lights: array<Light>;

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let corners = array<vec2<f32>, 3>(vec2<f32>(-1, -1), vec2<f32>(3, -1), vec2<f32>(-1, 3));
    return vec4<f32>(corners[index], 0, 1);
}

fn weights(p: vec2<f32>) -> vec4<f32> {
    return vec4<f32>((1.0-p.x)*(1.0-p.y), p.x*(1.0-p.y), (1.0-p.x)*p.y, p.x*p.y);
}

fn associated(pixel: vec4<f32>) -> vec4<f32> {
    return vec4<f32>(pixel.rgb * pixel.a, pixel.a);
}

fn auxiliary_pixel(p: vec2<i32>, last: vec2<i32>) -> vec4<f32> {
    if (controls.auxiliary_y.w != 0.0 && (any(p < vec2<i32>(0)) || any(p > last))) {
        return vec4<f32>(0.0);
    }
    return associated(textureLoad(bump_map, clamp(p, vec2<i32>(0), last), 0));
}

fn auxiliary_at(center: vec2<f32>) -> vec4<f32> {
    var uv = (center + controls.original.xy) / controls.original.zw;
    if (controls.auxiliary_x.w == 2.0) {
        uv = vec2<f32>(
            controls.auxiliary_x.x * uv.x + controls.auxiliary_x.y * uv.y + controls.auxiliary_x.z,
            controls.auxiliary_y.x * uv.x + controls.auxiliary_y.y * uv.y + controls.auxiliary_y.z,
        );
    }
    if (any(uv != uv) || any(abs(uv) > vec2<f32>(GLASS_FINITE_MAX))) {
        return vec4<f32>(0.0);
    }
    let dims = textureDimensions(bump_map);
    let last = vec2<i32>(dims) - vec2<i32>(1);
    if (controls.auxiliary_y.w != 0.0) {
        let border = vec2<f32>(PIXEL_CENTRE) / vec2<f32>(dims);
        if (any(uv < -border) || any(uv > vec2<f32>(1.0) + border)) {
            return vec4<f32>(0.0);
        }
    } else {
        uv = clamp(uv, vec2<f32>(0.0), vec2<f32>(1.0));
    }
    let texel = uv * vec2<f32>(dims);
    if (controls.stage.z == 0.0) {
        return auxiliary_pixel(vec2<i32>(floor(texel)), last);
    }
    let p = texel - vec2<f32>(PIXEL_CENTRE);
    let base = floor(p);
    let w = weights(p - base);
    var result = vec4<f32>(0.0);
    for (var i = 0u; i < 4u; i += 1u) {
        if (w[i] > 0.0) {
            result += auxiliary_pixel(vec2<i32>(base) + BILINEAR_TAPS[i], last) * w[i];
        }
    }
    return result;
}

fn height_channel(pixel: vec4<f32>) -> f32 {
    if (controls.surface.x == 3.0 || pixel.a == 0.0) {
        return pixel.a;
    }
    let rgb = clamp(pixel.rgb / pixel.a, vec3<f32>(0.0), vec3<f32>(1.0));
    if (controls.surface.x <= 2.0) {
        return pixel.a * rgb[u32(controls.surface.x)];
    }
    if (controls.surface.x == 4.0) {
        return pixel.a * ((rgb.r * 0.2126 + rgb.g * 0.7152) + rgb.b * 0.0722);
    }
    return pixel.a * (0.5 * (min(min(rgb.r, rgb.g), rgb.b) + max(max(rgb.r, rgb.g), rgb.b)));
}

@fragment
fn fs_height(@builtin(position) fragment: vec4<f32>) -> @location(0) f32 {
    if (controls.auxiliary_x.w != 0.0) {
        return height_channel(auxiliary_at(fragment.xy));
    }
    return height_channel(associated(textureLoad(original, vec2<i32>(fragment.xy), 0)));
}

@fragment
fn fs_reduce(@builtin(position) fragment: vec4<f32>) -> @location(0) f32 {
    let p = vec2<i32>(fragment.xy) * 2;
    let q = min(p + vec2<i32>(1), vec2<i32>(textureDimensions(plane)) - vec2<i32>(1));
    let top = textureLoad(plane, p, 0).x + textureLoad(plane, vec2<i32>(q.x, p.y), 0).x;
    let bottom = textureLoad(plane, vec2<i32>(p.x, q.y), 0).x + textureLoad(plane, q, 0).x;
    return (top + bottom) * 0.25;
}

@fragment
fn fs_blur(@builtin(position) fragment: vec4<f32>) -> @location(0) f32 {
    let p = vec2<i32>(fragment.xy);
    let last = vec2<i32>(textureDimensions(plane)) - vec2<i32>(1);
    let radius = controls.stage.x;
    let bound = i32(ceil(radius));
    let axis = select(vec2<i32>(1, 0), vec2<i32>(0, 1), controls.stage.y != 0.0);
    var total = 0.0;
    var sum = 0.0;
    for (var tap = -bound; tap <= bound; tap += 1) {
        let w = max(radius + 1.0 - f32(abs(tap)), 0.0);
        sum += textureLoad(plane, clamp(p + axis * tap, vec2<i32>(0), last), 0).x * w;
        total += w;
    }
    return sum / total;
}

fn plane_height(position: vec2<f32>) -> f32 {
    let last = vec2<i32>(textureDimensions(plane)) - vec2<i32>(1);
    let p = clamp(position, vec2<f32>(0.0), vec2<f32>(last));
    let base = floor(p);
    let w = weights(p - base);
    var result = 0.0;
    for (var i = 0u; i < 4u; i += 1u) {
        result += textureLoad(plane, min(vec2<i32>(base) + BILINEAR_TAPS[i], last), 0).x * w[i];
    }
    return result;
}

fn picture_at(position: vec2<f32>) -> vec4<f32> {
    let last = vec2<i32>(textureDimensions(original)) - vec2<i32>(1);
    let p = clamp(position, vec2<f32>(0.0), vec2<f32>(last));
    let base = floor(p);
    let w = weights(p - base);
    var result = vec4<f32>(0.0);
    for (var i = 0u; i < 4u; i += 1u) {
        let pixel = textureLoad(original, min(vec2<i32>(base) + BILINEAR_TAPS[i], last), 0);
        let weight = w[i] * pixel.a;
        result += vec4<f32>(pixel.rgb * weight, weight);
    }
    if (result.a > 0.0) {
        let rgb = clamp(result.rgb / result.a, vec3<f32>(-GLASS_FINITE_MAX), vec3<f32>(GLASS_FINITE_MAX));
        return vec4<f32>(rgb, min(result.a, 1.0));
    }
    return vec4<f32>(0.0);
}

fn product(a: vec3<f32>, b: vec3<f32>) -> f32 {
    return (a.x * b.x + a.y * b.y) + a.z * b.z;
}

fn direction_and_length(v: vec3<f32>) -> vec4<f32> {
    let scale = max(max(abs(v.x), abs(v.y)), abs(v.z));
    if (scale == 0.0 || scale > GLASS_FINITE_MAX) {
        return vec4<f32>(0.0);
    }
    let reduced = v / scale;
    let len = sqrt(product(reduced, reduced));
    return vec4<f32>(reduced / len, len * scale);
}

fn shade(pixel: vec4<f32>, normal: vec3<f32>, position: vec3<f32>) -> vec4<f32> {
    let exponent = clamp(2.0 / controls.sampling.x - 2.0, 2.0, 256.0);
    var diffuse_sum = controls.material.x * controls.ambient.xyz;
    var specular_sum = vec3<f32>(0.0);
    for (var i = 0u; i < u32(controls.stage.w); i += 1u) {
        let light = lights[i];
        var delta = vec4<f32>(0.0);
        if (light.color_kind.w != 0.0 || light.position_falloff.w != 0.0) {
            delta = direction_and_length(light.position_falloff.xyz - position);
        }
        let to_light = select(delta.xyz, -light.direction_radius.xyz, light.color_kind.w == 0.0);
        let outside = max(delta.w - light.direction_radius.w, 0.0);
        var falloff = 1.0;
        if (light.position_falloff.w == 1.0 && outside > 0.0) {
            falloff = 0.0;
            if (light.range_cone.x > 0.0) {
                falloff = max(1.0 - outside / light.range_cone.x, 0.0);
            }
        } else if (light.position_falloff.w == 2.0 && outside > 0.0) {
            let ratio = light.direction_radius.w / delta.w;
            falloff = ratio * ratio;
        }
        if (light.color_kind.w == 2.0) {
            let angle = product(-to_light, light.direction_radius.xyz);
            let inner = light.range_cone.y;
            let outer = light.range_cone.z;
            var cone = select(0.0, 1.0, angle >= outer);
            if (inner > outer) {
                cone = clamp((angle - outer) / (inner - outer), 0.0, 1.0);
            }
            falloff *= cone;
        }
        let facing = max(product(normal, to_light), 0.0);
        if (facing == 0.0 || falloff == 0.0) {
            continue;
        }
        let halfway = direction_and_length(to_light + vec3<f32>(0.0, 0.0, 1.0)).xyz;
        let glint = pow(clamp(product(normal, halfway), 0.0, 1.0), exponent);
        let color = light.color_kind.xyz * falloff;
        diffuse_sum = clamp(diffuse_sum + color * (facing * controls.material.y), vec3<f32>(-65536.0), vec3<f32>(65536.0));
        specular_sum = clamp(specular_sum + color * (glint * controls.material.z), vec3<f32>(-65536.0), vec3<f32>(65536.0));
    }
    let tint = (1.0 - controls.material.w) + clamp(pixel.rgb, vec3<f32>(0.0), vec3<f32>(1.0)) * controls.material.w;
    let scale = max(abs(pixel.rgb), vec3<f32>(1.0));
    let value = (pixel.rgb / scale) * diffuse_sum + tint * (specular_sum / scale);
    let limit = vec3<f32>(GLASS_FINITE_MAX) / scale;
    let rgb = clamp(clamp(value, -limit, limit) * scale, vec3<f32>(-GLASS_FINITE_MAX), vec3<f32>(GLASS_FINITE_MAX));
    return vec4<f32>(rgb, pixel.a);
}

@fragment
fn fs_glass(@builtin(position) fragment: vec4<f32>) -> @location(0) vec4<f32> {
    let factor = controls.surface.w;
    let p = fragment.xy / factor - vec2<f32>(PIXEL_CENTRE);
    let gx = (plane_height(p + vec2<f32>(1, 0)) - plane_height(p - vec2<f32>(1, 0))) * (0.5 / factor);
    let gy = (plane_height(p + vec2<f32>(0, 1)) - plane_height(p - vec2<f32>(0, 1))) * (0.5 / factor);
    let depth = controls.surface.y;
    let displacement = controls.surface.z * (depth * 0.01);
    let steps = controls.sampling.yz;
    let displaced = fragment.xy - vec2<f32>(PIXEL_CENTRE) + displacement * vec2<f32>(gx, gy) / max(steps * steps, vec2<f32>(GLASS_MIN_POSITIVE));
    let pixel = picture_at(displaced);
    if (pixel.a == 0.0) {
        return vec4<f32>(0.0);
    }
    let n = controls.normal_map;
    let normal = direction_and_length(vec3<f32>(-(n.x * gx + n.y * gy) * depth, -(n.z * gx + n.w * gy) * depth, 1.0)).xyz;
    let a = controls.affine_x;
    let b = controls.affine_y;
    let position = vec3<f32>(
        a.x * fragment.x + a.y * fragment.y + a.z,
        b.x * fragment.x + b.y * fragment.y + b.z,
        0.0,
    );
    return shade(pixel, normal, position);
}
