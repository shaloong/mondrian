// Inner/outer key (KEY-03 / `builtin.inner_outer_key`): one authoring estimate
// keys the frame, then choke, feather and despill shape the coverage.
//
// The pass table is driven by the authored grade (design §6):
//   * `fs_inner_outer_matte` samples both authoring points, keys the chosen
//     side, and leaves the scalar matte in `.r` (`.g/.b/.a` stay zero).
//   * `fs_inner_outer_choke_h` / `fs_inner_outer_choke_v` are the separable
//     dilate (`params.y != 0`) or erode over that plane, radius `params.x`,
//     edge-clamped.
//   * `fs_inner_outer_feather_h` averages the horizontal box half and
//     `fs_inner_outer_feather_v` folds the vertical half into the composite.
//   * `fs_inner_outer_composite` runs despill plus coverage.
//
// Every draw binds one texture per pipeline input and the shared
// `SpatialPassUniform`. The intermediate planes are pure scalar planes, so the
// choke and feather taps read `.r` directly and never premultiply them.
//
// `params.w` is the renderer's linear-filter lane, so nothing is packed there.

/// Bilinear sample of the source frame at one normalized authoring point.
///
/// The point addresses texel centres: `point * extent` is the texel-space
/// position and the half-texel offset moves sample zero onto the centre of
/// texel zero, so `(0, 0)` and `(1, 1)` clamp to the corner texels instead of
/// falling a texel outside the frame. Each tap clamps independently at the
/// edge; the weights are the shared `AUX_BILINEAR_TAPS` order.
fn inner_outer_sample_point(point: vec2<f32>) -> vec3<f32> {
    let dims = frame_dims();
    let extent = vec2<f32>(dims);
    let position = point * extent - vec2<f32>(AUX_PIXEL_CENTRE);
    let origin = floor(position);
    let weights = auxiliary_weights(position - origin);
    var colour = vec3<f32>(0.0);
    for (var tap = 0u; tap < 4u; tap += 1u) {
        if (weights[tap] != 0.0) {
            let index = vec2<i32>(origin) + AUX_BILINEAR_TAPS[tap];
            colour += weights[tap] * load_clamped_from(original_texture, index).rgb;
        }
    }
    return colour;
}

/// Sample one estimate, falling back to the authored colour outside the frame.
fn inner_outer_estimate(point: vec2<f32>, authored: vec3<f32>) -> vec3<f32> {
    if (any(point < vec2<f32>(0.0)) || any(point > vec2<f32>(1.0)) || any(point != point)) {
        return authored;
    }
    return inner_outer_sample_point(point);
}

/// `(tolerance, softness)` of one colour-purity menu code.
fn inner_outer_purity(code: u32) -> vec2<f32> {
    if (code == 2u) {
        return vec2<f32>(0.10, 0.06);
    }
    if (code == 1u) {
        return vec2<f32>(0.20, 0.15);
    }
    return vec2<f32>(0.30, 0.30);
}

/// The estimate of the active 遮罩来源 side, sampled at draw time.
///
/// Both the matte and the composite recompute it from the same cells, so the
/// despill target is always the colour the matte removed even though the
/// intermediate planes carry no colour.
fn inner_outer_selected_estimate() -> vec3<f32> {
    let foreground = inner_outer_estimate(uniforms.extra1.xy, uniforms.extra.rgb);
    let background = inner_outer_estimate(uniforms.extra1.zw, uniforms.aces.rgb);
    return select(foreground, background, u32(uniforms.params.y) == 1u);
}

/// Matte plane of one pixel: the estimate distance keys coverage.
///
/// The frame supplies both estimates at draw time, so a movie can move its
/// sample points without a rebuild. A source pixel without coverage is purely
/// background (`m = 0`), which keeps it from polluting a later feather.
@fragment
fn fs_inner_outer_matte(input: VsOut) -> @location(0) vec4<f32> {
    let index = vec2<i32>(floor(input.position.xy));
    let source = load_clamped_from(original_texture, index);
    let estimate = inner_outer_selected_estimate();
    let difference = max(
        max(abs(source.r - estimate.r), abs(source.g - estimate.g)),
        abs(source.b - estimate.b),
    );
    let purity = inner_outer_purity(u32(uniforms.params.x));
    var matte = 1.0 - smoothstep(purity.x, purity.x + purity.y, difference);
    if (source.a <= 0.0) {
        matte = 0.0;
    }
    return vec4<f32>(matte, 0.0, 0.0, 0.0);
}

/// One matte-plane tap, edge-clamped.
fn inner_outer_matte_tap(index: vec2<i32>) -> f32 {
    return load_clamped_from(original_texture, index).r;
}

/// Separable choke: a maximum window dilates (`dilate`) and a minimum erodes.
fn inner_outer_choke(index: vec2<i32>, horizontal: bool) -> f32 {
    let radius = i32(uniforms.params.x);
    let dilate = uniforms.params.y != 0.0;
    let axis = select(vec2<i32>(0, 1), vec2<i32>(1, 0), horizontal);
    var value = inner_outer_matte_tap(index);
    for (var offset = 1; offset <= radius; offset = offset + 1) {
        let backward = inner_outer_matte_tap(index - axis * offset);
        let forward = inner_outer_matte_tap(index + axis * offset);
        if (dilate) {
            value = max(value, max(backward, forward));
        } else {
            value = min(value, min(backward, forward));
        }
    }
    return value;
}

/// Horizontal half of the feather box average.
fn inner_outer_feather_horizontal(index: vec2<i32>) -> f32 {
    let radius = i32(uniforms.params.x);
    var total = 0.0;
    for (var offset = -radius; offset <= radius; offset = offset + 1) {
        total += inner_outer_matte_tap(index + vec2<i32>(offset, 0));
    }
    return total / f32(2 * radius + 1);
}

/// Vertical half of the feather box average, read from the horizontal plane.
///
/// The folded draw carries the radius in `params.z`, because `params.x` and
/// `params.y` already select the despill tier and the matte-source side there.
fn inner_outer_feather_vertical(index: vec2<i32>) -> f32 {
    let radius = i32(uniforms.params.z);
    var total = 0.0;
    for (var offset = -radius; offset <= radius; offset = offset + 1) {
        total += load_clamped_from(auxiliary_texture, index + vec2<i32>(0, offset)).r;
    }
    return total / f32(2 * radius + 1);
}

/// Despill strength of one menu code: 0, 0.35 or 0.7.
fn inner_outer_despill(code: u32) -> f32 {
    if (code == 2u) {
        return 0.7;
    }
    if (code == 1u) {
        return 0.35;
    }
    return 0.0;
}

/// Despill plus coverage for one pixel, with the finished matte handed in.
///
/// Colour moves through premultiplied coverage and is unpremultiplied once, so
/// an alpha edge keeps the straight keyed colour. An uncovered source pixel
/// canonicalizes to `(0, 0, 0, 0)`.
fn inner_outer_composite(index: vec2<i32>, matte: f32) -> vec4<f32> {
    let source = load_clamped_from(original_texture, index);
    if (source.a <= 0.0) {
        return vec4<f32>(0.0);
    }
    let coverage = clamp(matte, 0.0, 1.0);
    let alpha = clamp(source.a, 0.0, 1.0) * coverage;
    if (alpha <= 0.0) {
        return vec4<f32>(0.0);
    }
    let keyed = premultiply(source);
    let estimate = inner_outer_selected_estimate();
    let despill = inner_outer_despill(u32(uniforms.params.x));
    let colour = mix(keyed.rgb, estimate * keyed.a, despill * (1.0 - coverage)) * coverage;
    return unpremultiply(vec4<f32>(colour, alpha));
}

/// Choke, horizontal half.
@fragment
fn fs_inner_outer_choke_h(input: VsOut) -> @location(0) vec4<f32> {
    let index = vec2<i32>(floor(input.position.xy));
    return vec4<f32>(inner_outer_choke(index, true), 0.0, 0.0, 0.0);
}

/// Choke, vertical half.
@fragment
fn fs_inner_outer_choke_v(input: VsOut) -> @location(0) vec4<f32> {
    let index = vec2<i32>(floor(input.position.xy));
    return vec4<f32>(inner_outer_choke(index, false), 0.0, 0.0, 0.0);
}

/// Feather, horizontal half.
@fragment
fn fs_inner_outer_feather_h(input: VsOut) -> @location(0) vec4<f32> {
    let index = vec2<i32>(floor(input.position.xy));
    return vec4<f32>(inner_outer_feather_horizontal(index), 0.0, 0.0, 0.0);
}

/// Feather, vertical half folded into the composite.
@fragment
fn fs_inner_outer_feather_v(input: VsOut) -> @location(0) vec4<f32> {
    let index = vec2<i32>(floor(input.position.xy));
    return inner_outer_composite(index, inner_outer_feather_vertical(index));
}

/// The composite on its own, reading the matte plane the chain produced.
@fragment
fn fs_inner_outer_composite(input: VsOut) -> @location(0) vec4<f32> {
    let index = vec2<i32>(floor(input.position.xy));
    return inner_outer_composite(index, load_clamped_from(auxiliary_texture, index).r);
}