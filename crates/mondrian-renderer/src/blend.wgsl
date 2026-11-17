// Blend (CHN-02): the reference layer is folded onto the premultiplied primary
// texel in straight scene-linear colour, graded by one of five Adobe-style
// modes, then mixed with the original by the authored `Blend With Original`
// weight.
//
// The pass table has exactly one draw: `fs_blend` unpremultiplies the primary
// texel, samples the reference layer once, computes `alt`, the mode and the
// final coverage, and writes the result premultiplied (design §3 and §6). The
// record path publishes the delivery affine in `aces`/`extra1` and the
// size-mode window in their `.w` components, so this kernel applies design §5's
// transform-then-mode mapping itself and `sample_auxiliary` samples exactly the
// uv it is handed (`size.w == 0`).

/// Primary pixel of one blend: both images clamp at their own edge.
fn blend_index(position: vec2<i32>) -> vec2<i32> {
    return clamp(position, vec2<i32>(0), frame_dims() - vec2<i32>(1));
}

/// Reference uv of one (already clamped) pixel.
///
/// The frame transform and the size-mode window land here in the same two steps
/// and the same order the CPU kernel takes (design §5): first the delivery
/// affine from `aces`/`extra1`, then the crop window from `aces.w`/`extra1.w`,
/// where a window of one is the stretch mode. `size.w` stays zero for this pass,
/// so `sample_auxiliary` samples exactly the uv handed to it instead of applying
/// the affine a second time.
fn blend_auxiliary_uv(index: vec2<i32>) -> vec2<f32> {
    let centre = (vec2<f32>(index) + vec2<f32>(AUX_PIXEL_CENTRE)) / uniforms.size.xy;
    let transformed = vec2<f32>(
        uniforms.aces.x * centre.x + uniforms.aces.y * centre.y + uniforms.aces.z,
        uniforms.extra1.x * centre.x + uniforms.extra1.y * centre.y + uniforms.extra1.z,
    );
    let window = vec2<f32>(max(uniforms.aces.w, 1e-6), max(uniforms.extra1.w, 1e-6));
    return vec2<f32>(0.5) + (transformed - vec2<f32>(0.5)) / window;
}

/// Straight linear interpolation `origin + (destination - origin) * amount`.
///
/// The CPU kernel spells the interpolation this way rather than as
/// `origin * (1 - amount) + destination * amount`, so the shader keeps the same
/// rounding. (`from` and `target` are WGSL reserved words, hence the names.)
fn blend_mix(origin: vec3<f32>, destination: vec3<f32>, amount: f32) -> vec3<f32> {
    return origin + (destination - origin) * amount;
}

/// Rec.709 luma of a straight scene-linear colour (design §3).
///
/// The three products are rounded into named locals before the additions and
/// the additions stay left-associated, so the CPU kernel and this kernel agree
/// on the `LUMA_EPSILON` guard even if a backend would otherwise contract the
/// sum into fused multiply-adds.
fn blend_luma(colour: vec3<f32>) -> f32 {
    let red = 0.2126 * colour.r;
    let green = 0.7152 * colour.g;
    let blue = 0.0722 * colour.b;
    return red + green + blue;
}

/// `max(C) - min(C)` of a straight scene-linear colour (design §3).
fn blend_chroma(colour: vec3<f32>) -> f32 {
    return max(colour.r, max(colour.g, colour.b)) - min(colour.r, min(colour.g, colour.b));
}

/// The colour half of modes 1 and 2: keep the primary luma and take the
/// reference hue/saturation. A reference with no luma collapses to grey at the
/// primary luma; `!(luma <= epsilon)` is the CPU kernel's branch written so a
/// NaN luma also takes the ratio rather than the grey fallback.
fn blend_colour_only(
    primary: vec3<f32>,
    reference: vec3<f32>,
    reference_alpha: f32,
) -> vec3<f32> {
    let primary_luma = blend_luma(primary);
    let reference_luma = blend_luma(reference);
    var tinted = vec3<f32>(primary_luma);
    if (!(reference_luma <= 1e-6)) {
        let scale = primary_luma / reference_luma;
        tinted = reference * scale;
    }
    return blend_mix(primary, tinted, reference_alpha);
}

/// CHN-02 (design §3 and §6): grade one premultiplied primary texel.
///
/// The CPU kernel reads its primary buffer premultiplied and writes the graded
/// pixel back premultiplied, so the shader unpremultiplies the loaded texel,
/// grades in straight colour and multiplies the final coverage back in.
@fragment
fn fs_blend(input: VsOut) -> @location(0) vec4<f32> {
    let position = vec2<i32>(floor(input.position.xy));
    let source = textureLoad(original_texture, blend_index(position), 0);
    // An uncovered primary pixel has no colour to grade, and the CPU kernel
    // returns `(0, 0, 0, 0)` before it looks at the reference at all.
    let alpha_primary = source.a;
    if (alpha_primary <= 0.0) {
        return vec4<f32>(0.0);
    }
    let colour_primary = source.rgb / alpha_primary;
    let reference = sample_auxiliary(blend_auxiliary_uv(position), false, false);
    let alpha_layer = clamp(reference.a, 0.0, 1.0);
    let colour_layer = reference.rgb;
    // `a_a == 0` leaves the primary untouched: a transparent reference does not
    // participate in the blend.
    let alternate = blend_mix(colour_primary, colour_layer, alpha_layer);

    let mode = u32(uniforms.params.x);
    var colour_out = colour_primary;
    var alpha_out = alpha_primary;
    if (mode == 1u) {
        colour_out = blend_colour_only(colour_primary, colour_layer, alpha_layer);
    } else if (mode == 2u) {
        // Only pixels that already carry colour are tinted; the rest of the
        // frame passes through unchanged.
        if (blend_chroma(colour_primary) > 1e-6) {
            colour_out = blend_colour_only(colour_primary, colour_layer, alpha_layer);
        }
    } else if (mode == 3u) {
        colour_out = min(colour_primary, alternate);
    } else if (mode == 4u) {
        colour_out = max(colour_primary, alternate);
    } else {
        // Crossfade: the two coverages composite as a real transition, so both
        // layers keep their transparent zones.
        colour_out = alternate;
        alpha_out = alpha_layer + alpha_primary * (1.0 - alpha_layer);
    }

    // `Blend With Original` (design §3): `1.0` means "show the original image",
    // so the default is the identity. The CPU kernel clamps the weight and
    // mixes the colour by the plain lerp and the coverage by its complement.
    let weight = clamp(uniforms.params.y, 0.0, 1.0);
    let colour_final = blend_mix(colour_out, colour_primary, weight);
    let alpha_final = clamp(alpha_out * (1.0 - weight) + alpha_primary * weight, 0.0, 1.0);
    if (alpha_final <= 0.0) {
        return vec4<f32>(0.0);
    }
    return vec4<f32>(colour_final * alpha_final, alpha_final);
}