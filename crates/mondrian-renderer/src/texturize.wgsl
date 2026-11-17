// Texturize (STY-08): the auxiliary map is a height map, its Rec. 709 luminance
// is the height, and the luminance slope along the authored light direction
// shades the primary frame (design §1).
//
// The pass table has exactly one draw: `fs_texturize`. The map is sampled
// bilinearly through `sample_auxiliary` after this shader applies the authored
// placement itself, the source texel (which is already premultiplied) is loaded
// and its colour is scaled by one shade scalar while its coverage is kept. The
// record path publishes the delivery affine in `aces`/`extra1`, but the
// auxiliary mapping is fixed to the primary grid (design §3), so `size.w == 0`
// and `sample_auxiliary` samples exactly the uv it is handed and never applies
// the affine a second time.
//
// Uniform block (design §5):
//   size   = [width, height, transparent-out-of-bounds flag, 0],
//   params = [contrast, cos(light), placement code, linear-filter flag written
//            by the renderer],
//   extra  = [primary/auxiliary width ratio, height ratio, 0, 0],
//   aces   = [affine a, affine b, affine tx, sin(light)],
//   extra1 = [affine c, affine d, affine ty, 0].
//
// `sin(light)` rides in `aces.w` because `params.w` is the filter flag the
// record path overwrites for every auxiliary pass, and the placement code owns
// `params.z`; the last lane of `aces` is the one cell the affine leaves free.

const TEXTURIZE_TILE: u32 = 1u;
const TEXTURIZE_CENTER: u32 = 2u;

/// Map uv of one primary uv (design §3).
///
/// `tiles` is `[W / Wt, H / Ht]`, so tiling repeats the map at its own pixel
/// scale, centring keeps that scale and pins the map centre to the frame
/// centre, and stretching reads the primary uv as it is.
fn texturize_uv(uv: vec2<f32>, placement: u32, tiles: vec2<f32>) -> vec2<f32> {
    if placement == TEXTURIZE_TILE {
        return fract(uv * tiles);
    }
    if placement == TEXTURIZE_CENTER {
        return vec2<f32>(0.5) + (uv - vec2<f32>(0.5)) * tiles;
    }
    return uv;
}

/// Offset one already placed map uv by a fraction of one map texel.
///
/// A tiled map wraps, so the central difference stays continuous across a tile
/// border; centred and stretched maps step off their window instead and are
/// then caught by the transparent-window rule in `texturize_height`.
fn texturize_shift(uv: vec2<f32>, placement: u32, delta: vec2<f32>) -> vec2<f32> {
    if placement == TEXTURIZE_TILE {
        return fract(uv + delta);
    }
    return uv + delta;
}

/// Rec. 709 luminance of one straight height sample (design §1).
///
/// The three products land in named locals before the sum stays
/// left-associated, so a backend cannot contract the expression into fused
/// multiply-adds and drift from `0.2126 R + 0.7152 G + 0.0722 B`.
fn texturize_luma(sample: vec4<f32>) -> f32 {
    let red = 0.2126 * sample.r;
    let green = 0.7152 * sample.g;
    let blue = 0.0722 * sample.b;
    return red + green + blue;
}

/// Height of the map at one placed uv.
///
/// A centred map only lights its own window: a sample outside `0..1` is
/// transparent there and contributes no relief. A tiled or stretched map reads
/// its uv, and the sampler clamps at the map border.
fn texturize_height(uv: vec2<f32>, placement: u32) -> f32 {
    if placement == TEXTURIZE_CENTER
        && (any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0))) {
        return 0.0;
    }
    return texturize_luma(sample_auxiliary(uv, false, false));
}

/// Texturize: `du = t(u + 1 texel) - t(u - 1 texel)` and the same along `v`;
/// `relief = (du cos + dv sin) * contrast` and `shade = clamp(1 + relief, 0, 2)`.
///
/// One scalar multiplies every colour channel, so the premultiplied coverage
/// stays the authored one and no colour value is clamped. A pixel without
/// coverage carries no colour and canonicalizes to transparent black.
@fragment
fn fs_texturize(input: VsOut) -> @location(0) vec4<f32> {
    let dims = frame_dims();
    let index = clamp(vec2<i32>(floor(input.position.xy)), vec2<i32>(0), dims - vec2<i32>(1));
    let source = textureLoad(original_texture, index, 0);
    let placement = u32(round(uniforms.params.z));
    let centre = (vec2<f32>(index) + vec2<f32>(AUX_PIXEL_CENTRE)) / uniforms.size.xy;
    let map_uv = texturize_uv(centre, placement, uniforms.extra.xy);
    // One map texel, so the difference is taken on the map's own pixel grid and
    // not on the primary grid the uv lives in.
    let texel_step = vec2<f32>(1.0) / vec2<f32>(textureDimensions(auxiliary_texture));
    let left = texturize_height(
        texturize_shift(map_uv, placement, vec2<f32>(-texel_step.x, 0.0)), placement,
    );
    let right = texturize_height(
        texturize_shift(map_uv, placement, vec2<f32>(texel_step.x, 0.0)), placement,
    );
    let top = texturize_height(
        texturize_shift(map_uv, placement, vec2<f32>(0.0, -texel_step.y)), placement,
    );
    let bottom = texturize_height(
        texturize_shift(map_uv, placement, vec2<f32>(0.0, texel_step.y)), placement,
    );
    let du = right - left;
    let dv = bottom - top;
    let relief = (du * uniforms.params.y + dv * uniforms.aces.w) * uniforms.params.x;
    let shade = clamp(1.0 + relief, 0.0, 2.0);
    if (source.a <= 0.0) {
        return vec4<f32>(0.0);
    }
    return vec4<f32>(source.rgb * shade, source.a);
}