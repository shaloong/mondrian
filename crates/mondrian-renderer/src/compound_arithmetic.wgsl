// Compound Arithmetic (CHN-03): one primary texel and one auxiliary sample are
// unpremultiplied to straight scene-linear colour, clamped to 0..1, combined by
// one of fifteen Adobe-style operators, run through the overflow rule, applied
// to the authored channel set, mixed with the original by the authored weight
// and written premultiplied (design §3 and §6).
//
// The pass table has exactly one draw: `fs_compound_arithmetic`. The record path
// publishes the delivery affine in `aces`/`extra1` but the auxiliary mapping is
// fixed to the primary grid (design §5): the kernel derives the auxiliary uv
// from the extent ratio itself and `sample_auxiliary` samples exactly the uv it
// is handed (`size.w == 0`), so no sampled affine is applied twice.
//
// Uniform block (design §6):
//   params.x operator_code code, params.y channel code, params.z overflow code,
//   params.w linear-filter flag (written by the renderer),
//   extra.x  stretch code, extra.y blend-with-original / 100, extra.z layer.

/// Primary pixel of one compound-arithmetic pass: both images clamp at their
/// own edge.
fn compound_arithmetic_index(position: vec2<i32>) -> vec2<i32> {
    return clamp(position, vec2<i32>(0), frame_dims() - vec2<i32>(1));
}

/// Auxiliary uv of one (already clamped) primary pixel (design §5).
///
/// Stretch reads the whole auxiliary frame, so the primary uv is the auxiliary
/// uv. Original reads the top-left auxiliary window at one auxiliary texel per
/// primary texel: the uv scales by `primary extent / auxiliary extent`, and
/// anything past the auxiliary frame reads transparent through
/// `sample_auxiliary`.
fn compound_arithmetic_auxiliary_uv(index: vec2<i32>) -> vec2<f32> {
    let centre = (vec2<f32>(index) + vec2<f32>(AUX_PIXEL_CENTRE)) / uniforms.size.xy;
    // Stretch is code 1; code 2 is the original-size window.
    if (u32(uniforms.extra.x) != 2u) {
        return centre;
    }
    let auxiliary_extent = vec2<f32>(textureDimensions(auxiliary_texture));
    // Same association as the CPU kernel: scale by the primary extent first,
    // then divide by the auxiliary extent, so both round the uv identically.
    return centre * uniforms.size.xy / auxiliary_extent;
}

/// Rec.709 luma of a straight scene-linear colour (design §3).
///
/// The three products are rounded into named locals before the additions and
/// the additions stay left-associated, so the CPU kernel and this kernel agree
/// on the `lighter`/`darker` comparison even if a backend would otherwise
/// contract the sum into fused multiply-adds.
fn compound_arithmetic_luma(colour: vec3<f32>) -> f32 {
    let red = 0.2126 * colour.r;
    let green = 0.7152 * colour.g;
    let blue = 0.0722 * colour.b;
    return red + green + blue;
}

/// `round(clamp(value, 0, 1) * 255)` — the 8-bit operand of the bitwise
/// operators (design §3).
///
/// `floor(x + 0.5)` spells out half-up rounding: WGSL `round` is round-half-to-
/// even, while the CPU kernel's `f32::round` rounds ties away from zero, so the
/// two would disagree on an exact half such as `0.5 * 255`.
fn compound_arithmetic_quantized(value: f32) -> u32 {
    return u32(floor(clamp(value, 0.0, 1.0) * 255.0 + 0.5));
}

/// One computed component after the overflow rule (design §3).
///
/// `clip` is code 1, `wrap` is code 2 and `scale` is code 3. `scale` needs the
/// whole-frame low/high reduction and is CPU-only; the pass table rejects it
/// before this kernel is ever recorded, so the code never reaches the GPU.
fn compound_arithmetic_overflow(value: f32, mode: u32) -> f32 {
    if (mode == 2u) {
        return value - floor(value);
    }
    if (mode == 3u) {
        return value;
    }
    return clamp(value, 0.0, 1.0);
}

/// The overflow rule applied to the three colour components at once.
fn compound_arithmetic_overflow_rgb(value: vec3<f32>, mode: u32) -> vec3<f32> {
    return vec3<f32>(
        compound_arithmetic_overflow(value.r, mode),
        compound_arithmetic_overflow(value.g, mode),
        compound_arithmetic_overflow(value.b, mode),
    );
}

/// The fifteen operators over one straight sample pair (design §3), before the
/// channel selection and the overflow rule.
///
/// `lighter` and `darker` compare the whole sample by its luma and keep the
/// chosen sample verbatim; a tie keeps the source. Every other operator_code runs
/// component by component, including the alpha component, so the `argb` channel
/// mode can reuse the same candidate.
fn compound_arithmetic_operate(
    operator_code: u32,
    source: vec4<f32>,
    auxiliary: vec4<f32>,
) -> vec4<f32> {
    if (operator_code == 1u) {
        return auxiliary;
    }
    if (operator_code == 2u) {
        return source + auxiliary;
    }
    if (operator_code == 3u) {
        return source - auxiliary;
    }
    if (operator_code == 4u) {
        return source * auxiliary;
    }
    if (operator_code == 5u) {
        return abs(source - auxiliary);
    }
    if (operator_code == 6u) {
        return vec4<f32>(
            f32(compound_arithmetic_quantized(source.r)
                & compound_arithmetic_quantized(auxiliary.r)) / 255.0,
            f32(compound_arithmetic_quantized(source.g)
                & compound_arithmetic_quantized(auxiliary.g)) / 255.0,
            f32(compound_arithmetic_quantized(source.b)
                & compound_arithmetic_quantized(auxiliary.b)) / 255.0,
            f32(compound_arithmetic_quantized(source.a)
                & compound_arithmetic_quantized(auxiliary.a)) / 255.0,
        );
    }
    if (operator_code == 7u) {
        return vec4<f32>(
            f32(compound_arithmetic_quantized(source.r)
                | compound_arithmetic_quantized(auxiliary.r)) / 255.0,
            f32(compound_arithmetic_quantized(source.g)
                | compound_arithmetic_quantized(auxiliary.g)) / 255.0,
            f32(compound_arithmetic_quantized(source.b)
                | compound_arithmetic_quantized(auxiliary.b)) / 255.0,
            f32(compound_arithmetic_quantized(source.a)
                | compound_arithmetic_quantized(auxiliary.a)) / 255.0,
        );
    }
    if (operator_code == 8u) {
        return vec4<f32>(
            f32(compound_arithmetic_quantized(source.r)
                ^ compound_arithmetic_quantized(auxiliary.r)) / 255.0,
            f32(compound_arithmetic_quantized(source.g)
                ^ compound_arithmetic_quantized(auxiliary.g)) / 255.0,
            f32(compound_arithmetic_quantized(source.b)
                ^ compound_arithmetic_quantized(auxiliary.b)) / 255.0,
            f32(compound_arithmetic_quantized(source.a)
                ^ compound_arithmetic_quantized(auxiliary.a)) / 255.0,
        );
    }
    if (operator_code == 9u) {
        return select(source, auxiliary, compound_arithmetic_luma(auxiliary.rgb)
            > compound_arithmetic_luma(source.rgb));
    }
    if (operator_code == 10u) {
        return select(source, auxiliary, compound_arithmetic_luma(auxiliary.rgb)
            < compound_arithmetic_luma(source.rgb));
    }
    if (operator_code == 11u) {
        return min(source, auxiliary);
    }
    if (operator_code == 12u) {
        return max(source, auxiliary);
    }
    if (operator_code == 13u) {
        return vec4<f32>(1.0) - (vec4<f32>(1.0) - source) * (vec4<f32>(1.0) - auxiliary);
    }
    if (operator_code == 14u) {
        // Overlay uses the source as the base.
        return select(
            vec4<f32>(1.0) - vec4<f32>(2.0) * (vec4<f32>(1.0) - source)
                * (vec4<f32>(1.0) - auxiliary),
            vec4<f32>(2.0) * source * auxiliary,
            source < vec4<f32>(0.5),
        );
    }
    if (operator_code == 15u) {
        // Hard light uses the auxiliary as the base.
        return select(
            vec4<f32>(1.0) - vec4<f32>(2.0) * (vec4<f32>(1.0) - source)
                * (vec4<f32>(1.0) - auxiliary),
            vec4<f32>(2.0) * source * auxiliary,
            auxiliary < vec4<f32>(0.5),
        );
    }
    // Code 1 (`copy`) is the documented fallback for an unknown operator_code, which
    // keeps an unrecognized grade identical to the CPU kernel's `else` branch.
    return auxiliary;
}

/// CHN-03 (design §3 and §6): compound one premultiplied primary texel.
///
/// The CPU kernel reads its primary buffer premultiplied and writes the graded
/// pixel back premultiplied, so the shader unpremultiplies the loaded texel,
/// grades in straight colour and multiplies the final coverage back in.
@fragment
fn fs_compound_arithmetic(input: VsOut) -> @location(0) vec4<f32> {
    let position = vec2<i32>(floor(input.position.xy));
    let loaded = textureLoad(original_texture, compound_arithmetic_index(position), 0);
    let alpha_loaded = loaded.a;
    // Straight source colour: an uncovered texel has no colour, so its straight
    // sample is zero, matching the CPU kernel's `straight_from_premultiplied`.
    // The pass does not return early here: `argb` runs the operator_code on coverage
    // and can invent coverage from the layer, and only the final coverage
    // decides whether the written pixel is transparent.
    var source = vec4<f32>(0.0);
    if (alpha_loaded > 0.0) {
        source = vec4<f32>(clamp(loaded.rgb / alpha_loaded, vec3<f32>(0.0), vec3<f32>(1.0)),
            clamp(alpha_loaded, 0.0, 1.0));
    }
    let reference = sample_auxiliary(compound_arithmetic_auxiliary_uv(position), false, false);

    // Both inputs are clamped to 0..1 before any operator_code runs (design §3).
    let auxiliary = clamp(reference, vec4<f32>(0.0), vec4<f32>(1.0));

    let operator_code = u32(uniforms.params.x);
    let channels = u32(uniforms.params.y);
    let overflow = u32(uniforms.params.z);
    let candidate = compound_arithmetic_operate(operator_code, source, auxiliary);

    // Channel modes (design §3): `rgb` is code 1, `argb` is code 2 and `alpha`
    // is code 3. The overflow rule lands on every computed component only; the
    // channels a mode keeps from the source pass through untouched.
    var graded = source;
    if (channels == 2u) {
        graded = vec4<f32>(
            compound_arithmetic_overflow_rgb(candidate.rgb, overflow),
            compound_arithmetic_overflow(candidate.a, overflow),
        );
    } else if (channels == 3u) {
        graded = vec4<f32>(source.rgb, compound_arithmetic_overflow(candidate.a, overflow));
    } else {
        graded = vec4<f32>(compound_arithmetic_overflow_rgb(candidate.rgb, overflow), source.a);
    }

    // `Blend With Original` (design §6): `100` means "show the original image",
    // so the default is the identity. The mix happens in premultiplied space and
    // the coverage mixes by its complement, exactly like the CPU kernel.
    let weight = clamp(uniforms.extra.y, 0.0, 1.0);
    let source_premultiplied = vec4<f32>(source.rgb * source.a, source.a);
    let graded_premultiplied = vec4<f32>(graded.rgb * graded.a, graded.a);
    let mixed = graded_premultiplied * (1.0 - weight) + source_premultiplied * weight;
    if (mixed.a <= 0.0) {
        return vec4<f32>(0.0);
    }
    return vec4<f32>(mixed.rgb, clamp(mixed.a, 0.0, 1.0));
}