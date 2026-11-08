fn invert_hue_of(rgb: vec3<f32>, high: f32, chroma: f32) -> f32 {
    var hue = 0.0;
    if (high == rgb.r) {
        hue = (rgb.g - rgb.b) / chroma;
    } else if (high == rgb.g) {
        hue = (rgb.b - rgb.r) / chroma + 2.0;
    } else {
        hue = (rgb.r - rgb.g) / chroma + 4.0;
    }
    return fract(hue / 6.0);
}

// CHN-07 shares the raw and derived channel reader with CHN-08.
fn shift_channels_source(
    rgb: vec3<f32>,
    alpha: f32,
    tag: u32,
    coefficients: vec3<f32>,
) -> f32 {
    if (tag == 0u) { return rgb.r; }
    if (tag == 1u) { return rgb.g; }
    if (tag == 2u) { return rgb.b; }
    if (tag == 3u) { return alpha; }
    if (tag == 4u) { return dot(rgb, coefficients); }
    let high = max(rgb.r, max(rgb.g, rgb.b));
    let low = min(rgb.r, min(rgb.g, rgb.b));
    let chroma = high - low;
    let lightness = (high + low) * 0.5;
    if (tag == 5u) {
        if (chroma <= 1.0e-6) { return 0.0; }
        return invert_hue_of(rgb, high, chroma);
    }
    if (tag == 6u) {
        let span = 1.0 - abs(2.0 * lightness - 1.0);
        if (chroma <= 1.0e-6 || span <= 1.0e-6) { return 0.0; }
        return chroma / span;
    }
    if (tag == 7u) { return lightness; }
    if (tag == 8u) { return high; }
    return low;
}

// CHN-08 samples one of eight source codes. Codes 0..3 are the RGBA slots and
// reuse the CHN-07 reader verbatim, so a slot that both effects can sample
// agrees bit for bit; 4 is luminance, 5 lightness, 6 saturation and 7 hue.
fn channel_combiner_sample(
    pixel: vec4<f32>,
    code: u32,
    coefficients: vec3<f32>,
) -> f32 {
    if (code <= 3u) {
        return shift_channels_source(pixel.rgb, pixel.a, code, coefficients);
    }
    if (code == 4u) { return dot(pixel.rgb, coefficients); }
    if (code == 5u) {
        let high = max(pixel.r, max(pixel.g, pixel.b));
        let low = min(pixel.r, min(pixel.g, pixel.b));
        return (high + low) * 0.5;
    }
    if (code == 6u) {
        return shift_channels_source(pixel.rgb, pixel.a, 6u, coefficients);
    }
    if (code == 7u) {
        return shift_channels_source(pixel.rgb, pixel.a, 5u, coefficients);
    }
    return 0.0;
}

// Four bits per destination slot; 15 keeps the primary slot.
// Flags encode the derived target, derived source and invert bit.
fn channel_combiner_apply(
    pixel: vec4<f32>,
    source_pixel: vec4<f32>,
    slots: u32,
    flags: u32,
    params: vec4<f32>,
) -> vec4<f32> {
    let coefficients = params.yzw;
    let inverted = (flags & 256u) != 0u;
    var out = pixel;
    if ((flags & 15u) != 0u) {
        let value = channel_combiner_sample(source_pixel, (flags >> 4u) & 15u, coefficients);
        let target_value = select(value, 1.0 - value, inverted);
        if ((flags & 15u) == 1u) {
            let base = dot(pixel.rgb, coefficients);
            let scale = select(0.0, target_value / base, base > 0.0);
            out = vec4<f32>(pixel.rgb * scale, pixel.a);
        } else {
            let high = max(pixel.r, max(pixel.g, pixel.b));
            let low = min(pixel.r, min(pixel.g, pixel.b));
            let base = (high + low) * 0.5;
            let shift = select(target_value - base, 0.0, base != base);
            out = vec4<f32>(pixel.rgb + vec3<f32>(shift), pixel.a);
        }
    } else {
        let sampled = vec4<f32>(
            channel_combiner_sample(source_pixel, slots & 15u, coefficients),
            channel_combiner_sample(source_pixel, (slots >> 4u) & 15u, coefficients),
            channel_combiner_sample(source_pixel, (slots >> 8u) & 15u, coefficients),
            channel_combiner_sample(source_pixel, (slots >> 12u) & 15u, coefficients),
        );
        let value = select(sampled, vec4<f32>(1.0) - sampled, inverted);
        if ((slots & 15u) != 15u) { out.r = value.x; }
        if (((slots >> 4u) & 15u) != 15u) { out.g = value.y; }
        if (((slots >> 8u) & 15u) != 15u) { out.b = value.z; }
        if (((slots >> 12u) & 15u) != 15u) { out.a = value.w; }
    }
    if (params.x > 0.0) { out.a = params.x; }
    return out;
}
