// Difference matte (KEY-06): the reference raster is differenced against the
// primary frame in premultiplied space, and that difference becomes the matte.
//
// The pass table has at most two draws:
//   * `fs_difference_matte_h` averages `S(p + k x) - A(p + k x)` over the
//     horizontal taps and leaves the premultiplied difference plane.
//   * `fs_difference_matte` keys and composites in one draw. When it reads the
//     reference raster (`uniforms.extra.z == 0`) the difference is the plain
//     `S - A`; when it reads the horizontal plane (`uniforms.extra.z == 2`) it
//     averages the vertical taps first.
//
// The record path publishes the delivery affine in `aces`/`extra1` and the
// size-mode window in their `.w` components, so this kernel applies design §2's
// transform-then-mode mapping itself and `sample_auxiliary` samples exactly the
// uv it is handed with the graph's FRAME_TRANSFORM_LINEAR_CLAMP.

/// Primary pixel of a differencing tap: each image clamps at its own edge.
fn difference_matte_index(position: vec2<i32>) -> vec2<i32> {
    return clamp(position, vec2<i32>(0), frame_dims() - vec2<i32>(1));
}

/// Reference uv of one (already clamped) pixel.
///
/// The frame transform and the size-mode window land here in the same two steps
/// and the same order the CPU kernel takes (design §2): first the delivery
/// affine from `aces`/`extra1`, then the crop window from `aces.w`/`extra1.w`,
/// where a window of one is the stretch mode. `size.w` stays zero for this pass,
/// so `sample_auxiliary` samples exactly the uv handed to it instead of applying
/// the affine a second time.
fn difference_matte_uv(index: vec2<i32>) -> vec2<f32> {
    let centre = (vec2<f32>(index) + vec2<f32>(AUX_PIXEL_CENTRE)) / uniforms.size.xy;
    let transformed = vec2<f32>(
        uniforms.aces.x * centre.x + uniforms.aces.y * centre.y + uniforms.aces.z,
        uniforms.extra1.x * centre.x + uniforms.extra1.y * centre.y + uniforms.extra1.z,
    );
    let window = vec2<f32>(max(uniforms.aces.w, 1e-6), max(uniforms.extra1.w, 1e-6));
    return vec2<f32>(0.5) + (transformed - vec2<f32>(0.5)) / window;
}

/// `(1 / taps) * sum_k [ S(p + k axis) - A(p + k axis) ]`.
///
/// `uniforms.extra.x` is the whole radius `R` and `uniforms.extra.y` is the
/// reciprocal tap count `1 / (2R + 1)`, so a radius of zero reduces the sum to
/// the plain `S - A` difference. Both frames are read premultiplied: the
/// coverage-weighted difference is correct on alpha edges and never compares
/// hidden colour.
fn difference_matte_difference(position: vec2<i32>, horizontal: bool) -> vec4<f32> {
    let radius = i32(uniforms.extra.x);
    let axis = select(vec2<i32>(0, 1), vec2<i32>(1, 0), horizontal);
    var total = vec4<f32>(0.0);
    for (var offset = -radius; offset <= radius; offset = offset + 1) {
        let index = difference_matte_index(position + axis * offset);
        let primary = premultiply(textureLoad(original_texture, index, 0));
        let auxiliary = sample_auxiliary(difference_matte_uv(index), true, false);
        total += primary - auxiliary;
    }
    return total * uniforms.extra.y;
}

/// Vertical average over the horizontal difference plane.
fn difference_matte_vertical(position: vec2<i32>) -> vec4<f32> {
    let radius = i32(uniforms.extra.x);
    var total = vec4<f32>(0.0);
    for (var offset = -radius; offset <= radius; offset = offset + 1) {
        let index = difference_matte_index(position + vec2<i32>(0, offset));
        total += textureLoad(auxiliary_texture, index, 0);
    }
    return total * uniforms.extra.y;
}

/// Matte coverage from the four-channel distance `d = max |difference|`.
///
/// `d == tolerance` counts as a match: only a strictly greater distance opens
/// the matte at `softness == 0`, and `softness > 0` ramps linearly from
/// `tolerance` to `tolerance + softness`. `1e-6` only guards the division.
fn difference_matte_coverage(difference: vec4<f32>) -> f32 {
    let separation = max(
        max(abs(difference.r), abs(difference.g)),
        max(abs(difference.b), abs(difference.a)),
    );
    let tolerance = uniforms.params.x;
    let softness = uniforms.params.y;
    if (softness <= 0.0) {
        if (separation > tolerance) {
            return 1.0;
        }
        return 0.0;
    }
    return clamp((separation - tolerance) / max(softness, 1e-6), 0.0, 1.0);
}

/// Stage A: the horizontal difference average, written premultiplied.
@fragment
fn fs_difference_matte_h(input: VsOut) -> @location(0) vec4<f32> {
    let position = vec2<i32>(floor(input.position.xy));
    return difference_matte_difference(position, true);
}

/// Stage B (and the single pass): key and composite in one draw.
///
/// The keyed colour stays straight -- only coverage is scaled -- and a fully
/// transparent result canonicalizes to `(0, 0, 0, 0)` so no hidden colour
/// survives into an uncovered pixel.
@fragment
fn fs_difference_matte(input: VsOut) -> @location(0) vec4<f32> {
    let position = vec2<i32>(floor(input.position.xy));
    var difference = vec4<f32>(0.0);
    if (u32(uniforms.extra.z) == 2u) {
        difference = difference_matte_vertical(position);
    } else {
        difference = difference_matte_difference(position, true);
    }
    let source = textureLoad(original_texture, difference_matte_index(position), 0);
    // The CPU kernel clamps coverage to `0..=1` before scaling it by the matte,
    // so an over-range HDR alpha keys identically on both paths.
    let alpha = clamp(source.a, 0.0, 1.0) * difference_matte_coverage(difference);
    if (alpha <= 0.0) {
        return vec4<f32>(0.0);
    }
    return vec4<f32>(source.rgb, alpha);
}