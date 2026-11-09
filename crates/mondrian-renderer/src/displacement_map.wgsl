fn displacement_offset(pixel: vec4<f32>, selector: u32) -> f32 {
    if (selector == 9u) { return 1.0; }
    if (selector == 10u) { return 0.0; }
    if (selector == 11u) { return -1.0; }
    if (selector == 4u) { return 2.0 * pixel.a - 1.0; }

    let rgb = clamp(pixel.rgb, vec3<f32>(0.0), vec3<f32>(1.0));
    var channel = selector - 1u;
    if (selector == 7u) { channel = 7u; }
    if (selector == 8u) { channel = 6u; }
    let value = shift_channels_source(rgb, pixel.a, channel, vec3<f32>(0.2126, 0.7152, 0.0722));
    return (2.0 * clamp(value, 0.0, 1.0) - 1.0) * pixel.a;
}

fn displacement_wrapped_primary(base: vec2<i32>, fraction: vec2<f32>) -> vec4<f32> {
    let dims = frame_dims();
    let tl = premultiply(textureLoad(original_texture, periodic_texel(base, dims), 0));
    let tr = premultiply(textureLoad(original_texture, periodic_texel(base + AUX_BILINEAR_TAPS[1], dims), 0));
    let bl = premultiply(textureLoad(original_texture, periodic_texel(base + AUX_BILINEAR_TAPS[2], dims), 0));
    let br = premultiply(textureLoad(original_texture, periodic_texel(base + AUX_BILINEAR_TAPS[3], dims), 0));
    let top = tl + (tr - tl) * fraction.x;
    let bottom = bl + (br - bl) * fraction.x;
    return top + (bottom - top) * fraction.y;
}

fn displacement_primary(position: vec2<f32>, wrap: bool) -> vec4<f32> {
    let origin = floor(position);
    let base = vec2<i32>(origin);
    let fraction = position - origin;
    if (wrap) {
        return displacement_wrapped_primary(base, fraction);
    }
    let dims = frame_dims();
    let weights = auxiliary_weights(fraction);
    var result = vec4<f32>(0.0);
    for (var tap = 0u; tap < 4u; tap += 1u) {
        let index = base + AUX_BILINEAR_TAPS[tap];
        if (weights[tap] != 0.0 && all(index >= vec2<i32>(0)) && all(index < dims)) {
            result += weights[tap] * premultiply(textureLoad(original_texture, index, 0));
        }
    }
    return result;
}

@fragment
fn fs_displacement_map(input: VsOut) -> @location(0) vec4<f32> {
    let index = floor(input.position.xy);
    if (uniforms.params.z != 0.0) {
        return textureLoad(original_texture, vec2<i32>(index), 0);
    }
    let centre = index + vec2<f32>(AUX_PIXEL_CENTRE);
    let primary_extent = uniforms.size.xy;
    let map_extent = vec2<f32>(textureDimensions(auxiliary_texture));
    let map_layout = u32(uniforms.params.x);
    var uv = centre / primary_extent;
    if (map_layout == 1u) {
        uv = (centre + (map_extent - primary_extent) * 0.5) / map_extent;
    } else if (map_layout == 3u) {
        uv = centre / map_extent;
    }
    let pixel = unpremultiply(sample_auxiliary(uv, true, map_layout == 3u));
    let offset = vec2<f32>(
        displacement_offset(pixel, u32(uniforms.extra.z)),
        displacement_offset(pixel, u32(uniforms.extra.w)),
    );
    return unpremultiply(displacement_primary(index + offset * uniforms.extra.xy, uniforms.params.y != 0.0));
}
