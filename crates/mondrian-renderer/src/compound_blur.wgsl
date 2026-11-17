// Compound Blur (BLR-09): the luminance of one auxiliary blur map sets a radius
// for every primary pixel, and the pixel is replaced by the average of 25
// equal-weight samples spread over that disc in primary pixels (design §3).
//
// The pass table has exactly one draw: `fs_compound_blur`. The map is read
// bilinearly with the authored size mode applied to its uv, the source frame
// (which is already premultiplied) is sampled bilinearly, and the average is
// written premultiplied. The record path publishes the delivery affine in
// `aces`/`extra1`, but the auxiliary mapping is fixed to the primary grid
// (design §4): the kernel derives the map uv itself and `size.w == 0`, so
// `sample_auxiliary` samples exactly the uv it is handed and never applies the
// affine a second time.
//
// Uniform block (design §6):
//   size   = [width, height, transparent-out-of-bounds flag, 0],
//   params = [max blur in pixels, invert flag, size-mode code, linear-filter
//            flag written by the renderer],
//   extra  = [crop width, crop height, blur layer, 0].

const COMPOUND_BLUR_TAPS: u32 = 24u;
const COMPOUND_BLUR_GOLDEN_ANGLE: f32 = 2.3999632;

/// Primary pixel of one compound-blur pass: the source image clamps at its own
/// edge, which is how the CPU kernel samples its premultiplied copy.
fn compound_blur_index(position: vec2<i32>) -> vec2<i32> {
    return clamp(position, vec2<i32>(0), frame_dims() - vec2<i32>(1));
}

/// Blur-map uv of one primary pixel (design §4).
///
/// Stretch (code 1) reads the auxiliary frame at the primary uv. Center crop
/// (code 2) recentres that uv inside the authored window; anything outside the
/// window reads transparent through `sample_auxiliary`, so the map turns black
/// and the pixel is left untouched.
fn compound_blur_uv(
    uv: vec2<f32>,
    size_mode: u32,
    crop_w: f32,
    crop_h: f32,
) -> vec2<f32> {
    if (size_mode != 2u) {
        return uv;
    }
    let window = vec2<f32>(max(crop_w, 1e-6), max(crop_h, 1e-6));
    return vec2<f32>(0.5) + (uv - vec2<f32>(0.5)) / window;
}

/// Blur radius factor of one auxiliary uv (design §3):
/// `clamp(0.2126 R + 0.7152 G + 0.0722 B, 0, 1)`, inverted when the authored
/// toggle is on.
///
/// The three products land in named locals before the additions stay
/// left-associated, so a backend cannot contract the sum into fused
/// multiply-adds and drift from the CPU kernel.
fn compound_blur_map(uv: vec2<f32>) -> f32 {
    let sample = sample_auxiliary(uv, false, false);
    let red = 0.2126 * sample.r;
    let green = 0.7152 * sample.g;
    let blue = 0.0722 * sample.b;
    let luma = clamp(red + green + blue, 0.0, 1.0);
    if (uniforms.params.y >= 0.5) {
        return 1.0 - luma;
    }
    return luma;
}

/// Premultiplied bilinear clamp sample of the primary frame (design §3).
///
/// The working texture already holds premultiplied texels, so this is the four
/// tap weighted sum the CPU kernel takes over its premultiplied copy: clamp the
/// uv, land on the texel grid, floor to the origin and clamp every tap.
fn compound_blur_sample(uv: vec2<f32>) -> vec4<f32> {
    let dims = frame_dims();
    let extent = vec2<f32>(dims);
    let clamped = clamp(uv, vec2<f32>(0.0), vec2<f32>(1.0));
    let position = clamped * extent - vec2<f32>(AUX_PIXEL_CENTRE);
    let origin = floor(position);
    let weights = auxiliary_weights(position - origin);
    var sample = vec4<f32>(0.0);
    for (var tap = 0u; tap < 4u; tap += 1u) {
        if (weights[tap] != 0.0) {
            sample += weights[tap]
                * load_clamped_from(original_texture, vec2<i32>(origin) + AUX_BILINEAR_TAPS[tap]);
        }
    }
    return sample;
}

/// Compound blur: `R = max blur * map`; `R < 0.5` keeps the source pixel, and
/// anything larger averages the 25 disc samples (design §3).
///
/// The disc is a radius in primary pixels, so the offsets convert to uv through
/// the frame extent. Tap `k = 0` is the centre; taps `k = 1..=24` sit at
/// `R * sqrt(k / 24)` on the golden angle, which is the same order the CPU
/// kernel accumulates, keeping the average bit-comparable.
@fragment
fn fs_compound_blur(input: VsOut) -> @location(0) vec4<f32> {
    let index = vec2<i32>(floor(input.position.xy));
    let source = textureLoad(original_texture, compound_blur_index(index), 0);
    let centre = (vec2<f32>(index) + vec2<f32>(AUX_PIXEL_CENTRE)) / uniforms.size.xy;
    let map_uv = compound_blur_uv(
        centre,
        u32(uniforms.params.z),
        uniforms.extra.x,
        uniforms.extra.y,
    );
    let radius = uniforms.params.x * compound_blur_map(map_uv);
    // The CPU kernel keeps the source pixel when `radius < 0.5`; the comparison
    // is written the same way here so both sides agree on the boundary.
    if (radius < 0.5) {
        return source;
    }
    var total = vec4<f32>(0.0);
    for (var k = 0u; k <= COMPOUND_BLUR_TAPS; k += 1u) {
        var ring_radius = 0.0;
        var angle = 0.0;
        if (k > 0u) {
            ring_radius = radius * sqrt(f32(k) / f32(COMPOUND_BLUR_TAPS));
            angle = 0.5 + f32(k) * COMPOUND_BLUR_GOLDEN_ANGLE;
        }
        let du = ring_radius * cos(angle) / uniforms.size.x;
        let dv = ring_radius * sin(angle) / uniforms.size.y;
        total += compound_blur_sample(centre + vec2<f32>(du, dv));
    }
    let average = total / 25.0;
    if (average.a <= 0.0) {
        return vec4<f32>(0.0);
    }
    return vec4<f32>(average.rgb, clamp(average.a, 0.0, 1.0));
}