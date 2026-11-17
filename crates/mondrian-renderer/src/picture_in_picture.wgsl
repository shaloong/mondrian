// Picture in picture (TRF-04): one full-screen draw resamples the source into
// an authored window, paints the window's fill and border, and drops the
// window's analytic shadow over the untouched frame (design §3).
//
// The pass writes the whole frame, so a transparent input pixel is still a
// valid background: there is deliberately no `source.a <= 0` early-out (the
// texturize and key-cleaner passes have one, this pass must not).
//
// Uniform block (design §5). The shared `Uniforms` block carries the geometry:
//   size   = [W, H, transparent-out-of-bounds flag, 0],
//   params = [wpx, hpx, theta, linear-filter flag written by the renderer],
//   extra  = [scale, pan_x, pan_y, anti_alias],
//   aces   = [center_x, center_y, rounding_px, content code],
//   extra1 = [style code, opacity, border flag, border_width_px].
// The second block, `PictureInPictureUniform` on group 0 binding 3, carries the
// colours and the shadow:
//   shadow          = [shadow r, g, b, strength],
//   shadow_geometry = [shadow angle, shadow distance px, shadow zoom, shadow blur px],
//   fill            = [fill r, g, b, fill opacity],
//   flags           = [fill flag, fill matching flag, shadow flag, border opacity],
//   border          = [border r, g, b, 0].
//
// `params.w` is the linear-filter flag every auxiliary pass publishes; this
// kernel always samples the source bilinearly (design §3.3), so the flag is
// informational and the sampler below never switches on it.
//
// The frame entering the spatial chain carries straight alpha, and so does the
// written frame: the kernel premultiplies once at entry, composites the shadow,
// the fill, the resampled content and the border in premultiplied coverage, and
// unassociates once at the end. That is exactly the CPU kernel's order, and it
// is why an untouched frame comes back bit for bit.

struct PictureInPictureUniform {
    shadow: vec4<f32>,
    shadow_geometry: vec4<f32>,
    fill: vec4<f32>,
    flags: vec4<f32>,
    border: vec4<f32>,
};

@group(0) @binding(3) var<uniform> picture_in_picture: PictureInPictureUniform;

const PIP_STYLE_ELLIPSE: u32 = 2u;
const PIP_CONTENT_CONTAIN: u32 = 1u;
const PIP_CONTENT_COVER: u32 = 2u;

/// Signed distance of the rounded rectangle window (design §3.2).
///
/// `half_size` is `(wpx/2, hpx/2)` in pixels, `radius` the authored corner
/// radius in pixels, already clamped to the short half axis by the kernel.
/// Negative inside, positive outside, for both styles.
fn pip_rounded_box_distance(local: vec2<f32>, half_size: vec2<f32>, radius: f32) -> f32 {
    let inset = abs(local) - (half_size - vec2<f32>(radius));
    let outside = length(max(inset, vec2<f32>(0.0)));
    let inside = min(max(inset.x, inset.y), 0.0);
    return outside + inside - radius;
}

/// Signed distance of the ellipse window (design §3.2).
///
/// The true ellipse distance is not closed form, so the kernel uses the
/// normalised radial distance scaled back to pixels. It is negative inside as
/// well, which is what lets one coverage formula serve both styles.
fn pip_ellipse_distance(local: vec2<f32>, half_size: vec2<f32>) -> f32 {
    let uv = local / half_size;
    return (length(uv) - 1.0) * min(half_size.x, half_size.y);
}

/// Signed distance of the authored style at one local window coordinate.
fn pip_shape_distance(
    local: vec2<f32>, half_size: vec2<f32>, radius: f32, style: u32,
) -> f32 {
    if (style == PIP_STYLE_ELLIPSE) {
        return pip_ellipse_distance(local, half_size);
    }
    return pip_rounded_box_distance(local, half_size, radius);
}

/// Coverage of the window edge at one signed distance.
///
/// One formula serves both styles because both distances are negative inside
/// (design §3.2); `edge` is the authored anti-alias width, floored to half a
/// pixel so the ring never disappears at zero.
fn pip_coverage(distance: f32, edge: f32) -> f32 {
    return 1.0 - smoothstep(0.0, edge, distance);
}

/// Bilinear sample of the premultiplied source with the frame edge clamped.
///
/// Mirrors `resample::sample_bilinear_clamped` over the premultiplied copy the
/// CPU kernel snapshots: the texel coordinate is clamped to `0..extent-1`
/// *before* the fractional weights are taken, every tap is associated first,
/// then the four taps are blended as two horizontal lerps and one vertical
/// lerp. Sampling a texel centre therefore means passing `src_px - 0.5`.
fn pip_sample_source(position: vec2<f32>, dims: vec2<i32>) -> vec4<f32> {
    let limit = dims - vec2<i32>(1);
    let clamped = clamp(position, vec2<f32>(0.0), vec2<f32>(limit));
    let origin = floor(clamped);
    let weight = clamped - origin;
    let base = vec2<i32>(origin);
    let far = min(base + vec2<i32>(1), limit);
    let top_left = premultiply(textureLoad(original_texture, base, 0));
    let top_right = premultiply(textureLoad(original_texture, vec2<i32>(far.x, base.y), 0));
    let bottom_left = premultiply(textureLoad(original_texture, vec2<i32>(base.x, far.y), 0));
    let bottom_right = premultiply(textureLoad(original_texture, far, 0));
    let top = top_left + (top_right - top_left) * weight.x;
    let bottom = bottom_left + (bottom_right - bottom_left) * weight.x;
    return top + (bottom - top) * weight.y;
}

/// Source pixel of one frame pixel inside the window (design §3.3).
///
/// `local` is `q = R(-theta) * (p - c)`, so the source point is
/// `c_source + R(-theta)(p - c)`: the window content turns with the window.
/// `fit` is the content fit factor (stretch/contain/cover) and the pan offset
/// is expressed in window pixels, exactly as the CPU kernel does.
fn pip_source_position(
    local: vec2<f32>, half_size: vec2<f32>, scale: f32, pan: vec2<f32>,
) -> vec2<f32> {
    let extent = uniforms.size.xy;
    let content = u32(round(uniforms.aces.w));
    let window_extent = half_size * 2.0;
    var fit = vec2<f32>(extent.x / window_extent.x, extent.y / window_extent.y);
    if (content != 0u) {
        var ratio = min(window_extent.x / extent.x, window_extent.y / extent.y);
        if (content == PIP_CONTENT_COVER) {
            ratio = max(window_extent.x / extent.x, window_extent.y / extent.y);
        }
        fit = vec2<f32>(1.0 / ratio, 1.0 / ratio);
    }
    return extent * 0.5 + local * fit / max(scale, 1.0e-3)
        + vec2<f32>(pan.x * window_extent.x / fit.x, pan.y * window_extent.y / fit.y);
}

/// One premultiplied "over" of a straight colour and its alpha.
///
/// A layer with no alpha leaves the destination untouched, which is what makes
/// the shadowless window band copy the source unchanged.
fn pip_over(destination: vec4<f32>, colour: vec3<f32>, alpha: f32) -> vec4<f32> {
    if (alpha <= 0.0) {
        return destination;
    }
    let keep = 1.0 - alpha;
    return vec4<f32>(
        colour * alpha + destination.rgb * keep,
        alpha + destination.a * keep,
    );
}

/// Picture in picture: shadow, fill, resampled content, border (design §3.6).
@fragment
fn fs_picture_in_picture(input: VsOut) -> @location(0) vec4<f32> {
    let dims = frame_dims();
    let index = clamp(vec2<i32>(floor(input.position.xy)), vec2<i32>(0), dims - vec2<i32>(1));
    let source = textureLoad(original_texture, index, 0);
    let extent = uniforms.size.xy;
    let centre = vec2<f32>(index) + vec2<f32>(0.5);
    // The CPU kernel floors the window to one pixel in each axis, so the
    // half extents never collapse.
    let half_size = max(uniforms.params.xy * 0.5, vec2<f32>(0.5));
    let theta = uniforms.params.z;
    let scale = uniforms.extra.x;
    let pan = uniforms.extra.yz;
    let edge = max(uniforms.extra.w, 0.5);
    let window_centre = uniforms.aces.xy;
    let radius = clamp(uniforms.aces.z, 0.0, min(half_size.x, half_size.y));
    let style = u32(round(uniforms.extra1.x));
    let opacity = clamp(uniforms.extra1.y, 0.0, 1.0);
    let border_flag = uniforms.extra1.z;
    let border_width = uniforms.extra1.w;

    let cosine = cos(theta);
    let sine = sin(theta);
    let offset = centre - window_centre;
    let local = vec2<f32>(
        cosine * offset.x + sine * offset.y,
        -sine * offset.x + cosine * offset.y,
    );
    let distance = pip_shape_distance(local, half_size, radius, style);
    let window_alpha = pip_coverage(distance, edge) * opacity;

    // The shadow is the same shape, zoomed about the window centre and pushed
    // along the authored angle; its soft edge is the analytic band of
    // `shadow_blur_px`, with an explicit hard step when the blur is zero
    // (design §3.5).
    let shadow_strength = picture_in_picture.shadow.w;
    let shadow_angle = picture_in_picture.shadow_geometry.x;
    let shadow_distance = picture_in_picture.shadow_geometry.y;
    let shadow_zoom = picture_in_picture.shadow_geometry.z;
    let shadow_blur = picture_in_picture.shadow_geometry.w;
    var shadow_alpha = 0.0;
    if (picture_in_picture.flags.z >= 0.5) {
        let shadow_half = half_size * shadow_zoom;
        let shadow_centre = window_centre
            + vec2<f32>(cos(shadow_angle), sin(shadow_angle)) * shadow_distance;
        let shadow_offset = centre - shadow_centre;
        let shadow_local = vec2<f32>(
            cosine * shadow_offset.x + sine * shadow_offset.y,
            -sine * shadow_offset.x + cosine * shadow_offset.y,
        );
        let shadow_distance_sdf = pip_shape_distance(
            shadow_local, shadow_half, radius * shadow_zoom, style,
        );
        var coverage = 0.0;
        if (shadow_blur <= 0.0) {
            coverage = select(0.0, 1.0, shadow_distance_sdf <= 0.0);
        } else {
            coverage = 1.0 - smoothstep(-shadow_blur, shadow_blur, shadow_distance_sdf);
        }
        shadow_alpha = clamp(shadow_strength, 0.0, 1.0) * coverage;
    }

    // The kernel has no per-pixel skip: a frame neither the window nor the
    // shadow touches comes back through the premultiplied round trip, which is
    // the identity for unit coverage (design §3.6).
    var result = premultiply(source);

    result = pip_over(result, picture_in_picture.shadow.rgb, shadow_alpha);

    let source_position = pip_source_position(local, half_size, scale, pan);
    var content_sample = vec4<f32>(0.0);
    // Contain leaves the bands outside the source transparent; stretch and
    // cover clamp instead, and the published flag selects the rule (design
    // §3.3).
    let transparent_out_of_bounds = uniforms.size.z != 0.0;
    if (!transparent_out_of_bounds
        || (all(source_position >= vec2<f32>(0.0)) && all(source_position < extent))) {
        content_sample = pip_sample_source(source_position - vec2<f32>(0.5), dims);
    }
    let content_alpha = window_alpha * content_sample.a;

    // Fill sits under the content and only shows where the resampled source
    // does not cover the window (design §3.4).
    if (picture_in_picture.flags.x >= 0.5) {
        var fill_colour = picture_in_picture.fill.rgb;
        if (picture_in_picture.flags.y >= 0.5) {
            fill_colour = picture_in_picture.border.rgb;
        }
        let fill_alpha = clamp(picture_in_picture.fill.w, 0.0, 1.0)
            * window_alpha * (1.0 - content_alpha);
        result = pip_over(result, fill_colour, fill_alpha);
    }

    // The content is already associated, so it draws with its own premultiplied
    // colour scaled by the window coverage.
    let content_keep = 1.0 - content_alpha;
    result = vec4<f32>(
        content_sample.rgb * window_alpha + result.rgb * content_keep,
        content_alpha + result.a * content_keep,
    );

    // The border is the inward band of the shape between its edge and the
    // authored width; the width is unsigned, so a negative authoring value
    // still paints inward (design §3.4).
    if (border_flag >= 0.5) {
        let inner = pip_coverage(distance + border_width, edge);
        let outer = pip_coverage(distance, edge);
        let ring = max(outer - inner, 0.0);
        let border_alpha = clamp(picture_in_picture.flags.w, 0.0, 1.0) * ring;
        result = pip_over(result, picture_in_picture.border.rgb, border_alpha);
    }

    // Unassociate once; a pixel that ends without coverage carries no colour
    // and the RGB channels of a covered pixel are never clamped (design §3.6).
    return unpremultiply(result);
}