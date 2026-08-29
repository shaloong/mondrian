//! GPU-resident working-space compositing.
//!
//! This module owns the native wgpu path for preview/playback compositing when
//! a layer stack is simple enough to stay on GPU: affine transforms, fused
//! pointwise effect graphs, the canonical BlendMode algebra, and a bounded layer count.
//! Unsupported layer shapes are rejected with typed blockers so callers can
//! fall back to the CPU reference compositor without losing diagnostic evidence.

use crate::color_frame::GpuColorFrameBindGroupCacheKey;
use crate::creative_lut_gpu::{GpuCreativeLutPreparedBinding, GpuCreativeLutRuntime};
use crate::{
    ColorFrameDescriptor, ColorFrameDomain, ColorFrameEncoding, ColorFrameResidency, CpuColorFrame,
    GpuColorFrameAllocationPlan, GpuColorFrameBindGroupCacheKeyAllocationError,
    GpuColorFrameHandle, GpuColorFrameIdAllocationError, GpuColorFrameIdAllocator,
    GpuColorFrameResource, GpuColorFrameResourceTable, GpuColorFrameTextureFormat,
    GpuColorFrameUploadPlan, GpuColorFrameUploader, GpuColorFrameWgpuResource,
    GpuColorFrameWgpuResourcePool,
};
use bytemuck::{Pod, Zeroable};
use mondrian_core::{
    automation::QualifierSampleOperation,
    types::{BlendMode, Color},
};
use mondrian_effects::{
    CompiledEffectGpuPlan, EffectColorDomain, EffectGpuPointOp, MaskOp, PreparedQualifier,
    QualifierMode, MAX_FUSED_GPU_EFFECT_OPS,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

const GPU_COMPOSITOR_UNIFORM_PAGE_SLOTS: usize = 128;
const GPU_COMPOSITOR_SHADER: &str = r#"
struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

struct CompositeUniforms {
    opacity: f32,
    source_kind: u32,
    effect_count: u32,
    blend_mode: u32,
    frame_seed_lo: u32,
    frame_seed_hi: u32,
    mask_op: u32,
    mask_invert: u32,
    solid_color: vec4<f32>,
    inv_transform0: vec4<f32>,
    inv_transform1: vec4<f32>,
    geometry: vec4<f32>,
    effects: array<EffectUniform, 16>,
};

struct EffectUniform {
    header: vec4<u32>,
    params: vec4<f32>,
    color: vec4<f32>,
    extra0: vec4<f32>,
    extra1: vec4<f32>,
};

@group(0) @binding(0) var layer_tex: texture_2d<f32>;
@group(0) @binding(1) var linear_sampler: sampler;
@group(1) @binding(0) var accum_tex: texture_2d<f32>;
@group(2) @binding(0) var<uniform> uniforms: CompositeUniforms;
@group(3) @binding(0) var creative_lut_tex: texture_3d<f32>;

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VsOut {
    var positions = array<vec2<f32>, 4>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 1.0, -1.0),
        vec2<f32>(-1.0,  1.0),
        vec2<f32>( 1.0,  1.0),
    );
    var uvs = array<vec2<f32>, 4>(
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
    );
    var out: VsOut;
    out.position = vec4<f32>(positions[vertex_index], 0.0, 1.0);
    out.uv = uvs[vertex_index];
    return out;
}

fn hash_u32(input: u32) -> u32 {
    var value = input;
    value = value ^ (value >> 16u);
    value = value * 0x7feb352du;
    value = value ^ (value >> 15u);
    value = value * 0x846ca68bu;
    value = value ^ (value >> 16u);
    return value;
}

fn effect_graph_dither_seed(pixel_index: u32) -> u32 {
    return pixel_index ^
        ((uniforms.frame_seed_lo << 13u) | (uniforms.frame_seed_lo >> 19u)) ^
        ((uniforms.frame_seed_hi >> 7u) | (uniforms.frame_seed_hi << 25u));
}

fn blend_lum(color: vec3<f32>) -> f32 {
    return dot(color, vec3<f32>(0.299, 0.587, 0.114));
}

fn clip_blend_color(color: vec3<f32>) -> vec3<f32> {
    let luminance = blend_lum(color);
    let minimum = min(color.r, min(color.g, color.b));
    let maximum = max(color.r, max(color.g, color.b));
    if (minimum < 0.0) {
        let factor = luminance / (luminance - minimum);
        return vec3<f32>(luminance) + (color - vec3<f32>(luminance)) * factor;
    }
    if (maximum > 1.0) {
        let factor = (1.0 - luminance) / (maximum - luminance);
        return vec3<f32>(luminance) + (color - vec3<f32>(luminance)) * factor;
    }
    return color;
}

fn set_blend_lum(color: vec3<f32>, target_lum: f32) -> vec3<f32> {
    return clip_blend_color(color + vec3<f32>(target_lum - blend_lum(color)));
}

fn blend_sat(color: vec3<f32>) -> f32 {
    return max(color.r, max(color.g, color.b)) - min(color.r, min(color.g, color.b));
}

fn set_blend_sat(color: vec3<f32>, target_sat: f32) -> vec3<f32> {
    var result = vec3<f32>(0.0);
    let minimum = min(color.r, min(color.g, color.b));
    let maximum = max(color.r, max(color.g, color.b));
    if (maximum <= minimum) {
        return result;
    }
    let scale = target_sat / (maximum - minimum);
    if (color.r <= color.g && color.r <= color.b) {
        if (color.g <= color.b) {
            result = vec3<f32>(0.0, (color.g - minimum) * scale, target_sat);
        } else {
            result = vec3<f32>(0.0, target_sat, (color.b - minimum) * scale);
        }
    } else if (color.g <= color.r && color.g <= color.b) {
        if (color.r <= color.b) {
            result = vec3<f32>((color.r - minimum) * scale, 0.0, target_sat);
        } else {
            result = vec3<f32>(target_sat, 0.0, (color.b - minimum) * scale);
        }
    } else if (color.r <= color.g) {
        result = vec3<f32>((color.r - minimum) * scale, target_sat, 0.0);
    } else {
        result = vec3<f32>(target_sat, (color.g - minimum) * scale, 0.0);
    }
    return result;
}

fn blend_channel(mode: u32, base: f32, blend: f32) -> f32 {
    if (mode == 2u) { return base * blend; }
    if (mode == 3u) { return 1.0 - (1.0 - base) * (1.0 - blend); }
    if (mode == 4u) {
        return select(1.0 - 2.0 * (1.0 - base) * (1.0 - blend), 2.0 * base * blend, base <= 0.5);
    }
    if (mode == 5u) { return min(base, blend); }
    if (mode == 6u) { return max(base, blend); }
    if (mode == 7u) {
        if (blend >= 0.999) { return 1.0; }
        return clamp(base / (1.0 - blend), 0.0, 1.0);
    }
    if (mode == 8u) {
        if (blend <= 0.001) { return 0.0; }
        return clamp(1.0 - (1.0 - base) / blend, 0.0, 1.0);
    }
    if (mode == 9u) {
        return select(1.0 - 2.0 * (1.0 - base) * (1.0 - blend), 2.0 * base * blend, blend <= 0.5);
    }
    if (mode == 10u) {
        if (blend <= 0.5) { return base - (1.0 - 2.0 * blend) * base * (1.0 - base); }
        var curved = ((16.0 * base - 12.0) * base + 4.0) * base;
        if (base > 0.25) { curved = sqrt(base); }
        return base + (2.0 * blend - 1.0) * (curved - base);
    }
    if (mode == 11u) { return abs(base - blend); }
    if (mode == 12u) { return base + blend - 2.0 * base * blend; }
    if (mode == 13u) { return clamp(base - blend, 0.0, 1.0); }
    if (mode == 16u) { return clamp(base + blend - 1.0, 0.0, 1.0); }
    if (mode == 17u) { return clamp(base + blend, 0.0, 1.0); }
    if (mode == 18u) {
        if (blend <= 0.5) {
            if (blend <= 0.001) { return 0.0; }
            return clamp(1.0 - (1.0 - base) / (2.0 * blend), 0.0, 1.0);
        }
        if (blend >= 0.999) { return 1.0; }
        return clamp(base / (2.0 * (1.0 - blend)), 0.0, 1.0);
    }
    if (mode == 19u) { return clamp(base + 2.0 * blend - 1.0, 0.0, 1.0); }
    if (mode == 20u) { return select(max(base, 2.0 * (blend - 0.5)), min(base, 2.0 * blend), blend <= 0.5); }
    if (mode == 21u) { return select(1.0, 0.0, base + blend < 1.0); }
    if (mode == 22u) {
        if (blend <= 0.001) { return 1.0; }
        return clamp(base / blend, 0.0, 1.0);
    }
    return blend;
}

fn blend_rgb(mode: u32, base: vec3<f32>, blend: vec3<f32>) -> vec3<f32> {
    if (mode == 14u) { return select(base, blend, blend_lum(blend) < blend_lum(base)); }
    if (mode == 15u) { return select(base, blend, blend_lum(blend) > blend_lum(base)); }
    if (mode == 23u) { return set_blend_lum(set_blend_sat(blend, blend_sat(base)), blend_lum(base)); }
    if (mode == 24u) { return set_blend_lum(set_blend_sat(base, blend_sat(blend)), blend_lum(base)); }
    if (mode == 25u) { return set_blend_lum(blend, blend_lum(base)); }
    if (mode == 26u) { return set_blend_lum(base, blend_lum(blend)); }
    return vec3<f32>(
        blend_channel(mode, base.r, blend.r),
        blend_channel(mode, base.g, blend.g),
        blend_channel(mode, base.b, blend.b),
    );
}

fn blend_straight_alpha(
    base_px: vec4<f32>,
    blend_px: vec4<f32>,
    requested_opacity: f32,
    mode: u32,
    pixel_index: u32,
) -> vec4<f32> {
    var opacity = clamp(requested_opacity, 0.0, 1.0);
    if (opacity <= 0.0) { return base_px; }
    var effective_mode = mode;
    if (mode == 1u) {
        let threshold = f32(hash_u32(effect_graph_dither_seed(pixel_index))) / 4294967295.0;
        if (threshold > opacity) { return base_px; }
        opacity = 1.0;
        effective_mode = 0u;
    }
    let base_alpha = clamp(base_px.a, 0.0, 1.0);
    let blend_alpha = clamp(blend_px.a * opacity, 0.0, 1.0);
    if (blend_alpha <= 0.0) {
        return base_px;
    }
    if (base_alpha <= 0.0) {
        return vec4<f32>(blend_px.rgb, blend_alpha);
    }
    let out_alpha = blend_alpha + base_alpha * (1.0 - blend_alpha);
    if (out_alpha <= 0.0) {
        return vec4<f32>(0.0);
    }
    let blended_rgb = blend_rgb(effective_mode, base_px.rgb, blend_px.rgb);
    let premul = blended_rgb * blend_alpha + base_px.rgb * base_alpha * (1.0 - blend_alpha);
    return vec4<f32>(premul / out_alpha, out_alpha);
}

fn cross_dissolve_straight_alpha(
    left_px: vec4<f32>,
    right_px: vec4<f32>,
    progress: f32,
) -> vec4<f32> {
    let right_weight = clamp(progress, 0.0, 1.0);
    let left_weight = 1.0 - right_weight;
    let out_alpha = left_px.a * left_weight + right_px.a * right_weight;
    if (out_alpha <= 0.0) {
        return vec4<f32>(0.0);
    }
    let premul = left_px.rgb * left_px.a * left_weight +
        right_px.rgb * right_px.a * right_weight;
    return vec4<f32>(premul / out_alpha, out_alpha);
}

fn apply_alpha_mask(source_px: vec4<f32>, mask_px: vec4<f32>) -> vec4<f32> {
    var matte = clamp(mask_px.a, 0.0, 1.0);
    if (uniforms.mask_invert != 0u) {
        matte = 1.0 - matte;
    }
    let source_alpha = clamp(source_px.a, 0.0, 1.0);
    var output_alpha = source_alpha * matte;
    if (uniforms.mask_op == 1u) {
        output_alpha = source_alpha * (1.0 - matte);
    } else if (uniforms.mask_op == 2u) {
        output_alpha = min(source_alpha, matte);
    } else if (uniforms.mask_op == 3u) {
        output_alpha = abs(source_alpha - matte);
    }
    return vec4<f32>(source_px.rgb, output_alpha);
}

fn combine_alpha_masks(left_px: vec4<f32>, right_px: vec4<f32>) -> vec4<f32> {
    let left = clamp(left_px.a, 0.0, 1.0);
    let right = clamp(right_px.a, 0.0, 1.0);
    var output = max(left, right);
    if (uniforms.mask_op == 1u) {
        output = left * (1.0 - right);
    } else if (uniforms.mask_op == 2u) {
        output = min(left, right);
    } else if (uniforms.mask_op == 3u) {
        output = abs(left - right);
    }
    return vec4<f32>(0.0, 0.0, 0.0, output);
}

fn qualifier_safe_smoothstep(edge0: f32, edge1: f32, value: f32) -> f32 {
    if (edge1 <= edge0) {
        return select(0.0, 1.0, value >= edge1);
    }
    let t = clamp((value - edge0) / (edge1 - edge0), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

fn qualifier_tone_map_positive(value: f32) -> f32 {
    let positive = max(value, 0.0);
    return positive / (1.0 + positive);
}

fn qualifier_normalized_rgb(rgb: vec3<f32>) -> vec3<f32> {
    let positive = max(rgb, vec3<f32>(0.0));
    let peak = max(positive.r, max(positive.g, positive.b));
    return positive / (1.0 + peak);
}

fn qualifier_hue_saturation(rgb: vec3<f32>) -> vec2<f32> {
    let maximum = max(rgb.r, max(rgb.g, rgb.b));
    let minimum = min(rgb.r, min(rgb.g, rgb.b));
    let chroma = maximum - minimum;
    if (chroma <= 1.1920929e-7 || maximum <= 1.1920929e-7) {
        return vec2<f32>(0.0);
    }
    var sector: f32;
    if (maximum == rgb.r) {
        sector = positive_mod((rgb.g - rgb.b) / chroma, 6.0);
    } else if (maximum == rgb.g) {
        sector = (rgb.b - rgb.r) / chroma + 2.0;
    } else {
        sector = (rgb.r - rgb.g) / chroma + 4.0;
    }
    return vec2<f32>(sector / 6.0, clamp(chroma / maximum, 0.0, 1.0));
}

fn qualifier_range_matte(value: f32, low: f32, high: f32, softness: f32) -> f32 {
    return qualifier_safe_smoothstep(low - softness, low, value) *
        (1.0 - qualifier_safe_smoothstep(high, high + softness, value));
}

fn qualifier_sample(index: u32) -> vec4<f32> {
    let packed = uniforms.effects[2u + index / 4u];
    let lane = index % 4u;
    var coordinate: vec3<f32>;
    if (lane == 0u) {
        coordinate = packed.params.xyz;
    } else if (lane == 1u) {
        coordinate = packed.color.xyz;
    } else if (lane == 2u) {
        coordinate = packed.extra0.xyz;
    } else {
        coordinate = packed.extra1.xyz;
    }
    let excluded = (packed.header.x & (1u << lane)) != 0u;
    return vec4<f32>(coordinate, select(0.0, 1.0, excluded));
}

fn raw_qualifier_matte(rgb: vec3<f32>) -> f32 {
    let controls = uniforms.effects[0];
    if (controls.header.x == 0u) {
        let positive = max(rgb, vec3<f32>(0.0));
        let hue_saturation = qualifier_hue_saturation(positive);
        let luminance = qualifier_tone_map_positive(
            dot(positive, uniforms.effects[1].params.xyz),
        );
        let hue_delta = abs(hue_saturation.x - controls.params.x);
        let hue_distance = min(hue_delta, 1.0 - hue_delta);
        var hue_matte = 1.0;
        if (controls.params.y < 0.5) {
            hue_matte = 1.0 - qualifier_safe_smoothstep(
                controls.params.y,
                min(controls.params.y + controls.params.z, 0.5),
                hue_distance,
            );
        }
        return hue_matte *
            qualifier_range_matte(
                hue_saturation.y,
                controls.color.x,
                controls.color.y,
                controls.color.z,
            ) *
            qualifier_range_matte(
                luminance,
                controls.extra0.x,
                controls.extra0.y,
                controls.extra0.z,
            );
    }
    let coordinate = qualifier_normalized_rgb(rgb);
    var included = 0.0;
    var excluded = 0.0;
    for (var index = 0u; index < 16u; index = index + 1u) {
        if (index >= controls.header.y) { break; }
        let sample = qualifier_sample(index);
        let distance = length(coordinate - sample.xyz);
        let contribution = 1.0 - qualifier_safe_smoothstep(
            controls.extra1.x,
            controls.extra1.x + controls.extra1.y,
            distance,
        );
        if (sample.w > 0.5) {
            excluded = max(excluded, contribution);
        } else {
            included = max(included, contribution);
        }
    }
    return included * (1.0 - excluded);
}

fn qualifier_clamped_coordinate(coordinate: vec2<i32>) -> vec2<i32> {
    return clamp(
        coordinate,
        vec2<i32>(0),
        vec2<i32>(i32(uniforms.geometry.x) - 1, i32(uniforms.geometry.y) - 1),
    );
}

fn qualifier_filter_radius(filter_kind: u32) -> i32 {
    if (filter_kind == 1u) {
        return i32(uniforms.effects[0].header.z);
    }
    if (filter_kind == 2u) {
        return i32(ceil(uniforms.effects[1].params.w * 3.0));
    }
    return 0;
}

fn qualifier_filter_weight(filter_kind: u32, offset: i32, radius: i32) -> f32 {
    if (filter_kind == 1u) {
        return 1.0 / f32(radius * 2 + 1);
    }
    if (filter_kind == 2u) {
        let sigma = max(uniforms.effects[1].params.w, 0.001);
        let normalized = f32(offset) / sigma;
        let raw = exp(-0.5 * normalized * normalized);
        var total = 1.0;
        for (var sample = 1; sample <= 36; sample = sample + 1) {
            if (sample > radius) { break; }
            let value = f32(sample) / sigma;
            total = total + 2.0 * exp(-0.5 * value * value);
        }
        return raw / total;
    }
    return 1.0;
}

fn qualifier_filtered_matte(position: vec2<i32>, raw_source: bool) -> f32 {
    let filter_kind = uniforms.mask_op;
    let vertical = (uniforms.mask_invert & 1u) != 0u;
    let radius = qualifier_filter_radius(filter_kind);
    var sum = 0.0;
    for (var offset = -36; offset <= 36; offset = offset + 1) {
        if (abs(offset) > radius) { continue; }
        let coordinate = qualifier_clamped_coordinate(
            position + select(vec2<i32>(offset, 0), vec2<i32>(0, offset), vertical),
        );
        var value = textureLoad(layer_tex, coordinate, 0).a;
        if (raw_source) {
            value = raw_qualifier_matte(textureLoad(layer_tex, coordinate, 0).rgb);
        }
        sum = sum + value * qualifier_filter_weight(filter_kind, offset, radius);
    }
    if ((uniforms.mask_invert & 2u) != 0u) {
        let clean = uniforms.effects[0].extra1.zw;
        return qualifier_safe_smoothstep(clean.x, 1.0 - clean.y, sum);
    }
    return sum;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let base_px = textureSample(accum_tex, linear_sampler, in.uv);
    if (uniforms.source_kind == 5u) {
        let right_px = textureSample(layer_tex, linear_sampler, in.uv);
        return cross_dissolve_straight_alpha(base_px, right_px, uniforms.opacity);
    }
    if (uniforms.source_kind == 6u) {
        let mask_px = textureSample(layer_tex, linear_sampler, in.uv);
        return apply_alpha_mask(base_px, mask_px);
    }
    if (uniforms.source_kind == 10u) {
        let right_px = textureSample(layer_tex, linear_sampler, in.uv);
        return combine_alpha_masks(base_px, right_px);
    }
    if (uniforms.source_kind == 7u || uniforms.source_kind == 8u) {
        let position = vec2<i32>(floor(in.position.xy));
        let matte = qualifier_filtered_matte(position, uniforms.source_kind == 7u);
        return vec4<f32>(0.0, 0.0, 0.0, matte);
    }
    if (uniforms.source_kind == 9u) {
        let sampled = textureSample(layer_tex, linear_sampler, in.uv).a;
        let matte = select(sampled, 1.0 - sampled, uniforms.mask_invert != 0u);
        return vec4<f32>(matte, matte, matte, 1.0);
    }
    let source_position = source_coordinate(in.uv);
    var layer_px: vec4<f32>;
    if (uniforms.source_kind == 1u) {
        layer_px = select(vec4<f32>(0.0), uniforms.solid_color, source_inside(source_position));
    } else if (uniforms.source_kind == 4u) {
        layer_px = uniforms.solid_color;
    } else if (uniforms.source_kind == 2u) {
        layer_px = base_px;
    } else {
        layer_px = sample_layer(source_position);
    }
    let effect_position = source_position - vec2<f32>(0.5);
    layer_px = apply_effects(layer_px, effect_position);
    if (uniforms.source_kind == 3u || uniforms.source_kind == 4u) {
        return layer_px;
    }
    let pixel = vec2<u32>(floor(in.position.xy));
    let pixel_index = pixel.y * u32(uniforms.geometry.x) + pixel.x;
    return blend_straight_alpha(
        base_px,
        layer_px,
        uniforms.opacity,
        uniforms.blend_mode,
        pixel_index,
    );
}

fn source_coordinate(dst_uv: vec2<f32>) -> vec2<f32> {
    let dst_center = vec2<f32>(
        dst_uv.x * uniforms.geometry.x,
        dst_uv.y * uniforms.geometry.y,
    );
    return vec2<f32>(
        uniforms.inv_transform0.x * dst_center.x +
            uniforms.inv_transform0.y * dst_center.y +
            uniforms.inv_transform0.z,
        uniforms.inv_transform0.w * dst_center.x +
            uniforms.inv_transform1.x * dst_center.y +
            uniforms.inv_transform1.y,
    );
}

fn sample_layer(src_center: vec2<f32>) -> vec4<f32> {
    if (!source_inside(src_center)) {
        return vec4<f32>(0.0);
    }
    let src_size = uniforms.geometry.zw;
    return textureSample(layer_tex, linear_sampler, src_center / src_size);
}

fn source_inside(src_center: vec2<f32>) -> bool {
    let src_size = uniforms.geometry.zw;
    return src_center.x >= 0.5 &&
        src_center.y >= 0.5 &&
        src_center.x < src_size.x + 0.5 &&
        src_center.y < src_size.y + 0.5;
}

fn grain_noise(position: vec2<f32>) -> f32 {
    var value = u32(position.x) * 1973u + u32(position.y) * 9277u +
        uniforms.frame_seed_lo * 26699u + 0x68bc21ebu;
    value = value ^ (value << 13u);
    value = value ^ (value >> 17u);
    value = value ^ (value << 5u);
    return f32(value) / 4294967295.0 * 2.0 - 1.0;
}

fn creative_lut_texel(base_layer: u32, coordinate: vec3<u32>) -> vec3<f32> {
    return textureLoad(
        creative_lut_tex,
        vec3<i32>(
            i32(coordinate.x),
            i32(coordinate.y),
            i32(base_layer + coordinate.z),
        ),
        0,
    ).rgb;
}

fn sample_creative_lut(effect: EffectUniform, rgb: vec3<f32>) -> vec3<f32> {
    let base_layer = effect.header.y;
    let edge = effect.header.z;
    let maximum = f32(edge - 1u);
    let normalized = clamp(
        (rgb - effect.params.yzw) / (effect.color.xyz - effect.params.yzw),
        vec3<f32>(0.0),
        vec3<f32>(1.0),
    );
    let scaled = normalized * maximum;
    let lower = vec3<u32>(floor(scaled));
    let upper = min(lower + vec3<u32>(1u), vec3<u32>(edge - 1u));
    let fraction = scaled - vec3<f32>(lower);
    let r = fraction.x;
    let g = fraction.y;
    let b = fraction.z;
    let c000 = creative_lut_texel(base_layer, vec3<u32>(lower.x, lower.y, lower.z));
    let c100 = creative_lut_texel(base_layer, vec3<u32>(upper.x, lower.y, lower.z));
    let c010 = creative_lut_texel(base_layer, vec3<u32>(lower.x, upper.y, lower.z));
    let c110 = creative_lut_texel(base_layer, vec3<u32>(upper.x, upper.y, lower.z));
    let c001 = creative_lut_texel(base_layer, vec3<u32>(lower.x, lower.y, upper.z));
    let c101 = creative_lut_texel(base_layer, vec3<u32>(upper.x, lower.y, upper.z));
    let c011 = creative_lut_texel(base_layer, vec3<u32>(lower.x, upper.y, upper.z));
    let c111 = creative_lut_texel(base_layer, vec3<u32>(upper.x, upper.y, upper.z));
    if (r >= g) {
        if (g >= b) {
            return c000 + (c100 - c000) * r + (c110 - c100) * g + (c111 - c110) * b;
        }
        if (r >= b) {
            return c000 + (c100 - c000) * r + (c101 - c100) * b + (c111 - c101) * g;
        }
        return c000 + (c001 - c000) * b + (c101 - c001) * r + (c111 - c101) * g;
    }
    if (b >= g) {
        return c000 + (c001 - c000) * b + (c011 - c001) * g + (c111 - c011) * r;
    }
    if (b >= r) {
        return c000 + (c010 - c000) * g + (c011 - c010) * b + (c111 - c011) * r;
    }
    return c000 + (c010 - c000) * g + (c110 - c010) * r + (c111 - c110) * b;
}

fn color_curve_texel(effect: EffectUniform, row: u32, sample: u32) -> vec4<f32> {
    return textureLoad(
        creative_lut_tex,
        vec3<i32>(i32(sample), i32(row), i32(effect.header.y)),
        0,
    );
}

fn sample_color_curve(effect: EffectUniform, row: u32, component: u32, x: f32) -> f32 {
    let last = effect.header.z - 1u;
    let scale = f32(last);
    if (x <= 0.0) {
        let first = color_curve_texel(effect, row, 0u)[component];
        let second = color_curve_texel(effect, row, 1u)[component];
        return first + (second - first) * x * scale;
    }
    if (x >= 1.0) {
        let before = color_curve_texel(effect, row, last - 1u)[component];
        let endpoint = color_curve_texel(effect, row, last)[component];
        return endpoint + (endpoint - before) * (x - 1.0) * scale;
    }
    let position = x * scale;
    let lower = u32(floor(position));
    let fraction = position - f32(lower);
    let left = color_curve_texel(effect, row, lower)[component];
    let right = color_curve_texel(effect, row, lower + 1u)[component];
    return left + (right - left) * fraction;
}

fn positive_mod(value: f32, modulus: f32) -> f32 {
    return value - floor(value / modulus) * modulus;
}

fn color_curve_rgb_to_hsv(rgb: vec3<f32>) -> vec3<f32> {
    let maximum = max(rgb.r, max(rgb.g, rgb.b));
    let minimum = min(rgb.r, min(rgb.g, rgb.b));
    let chroma = maximum - minimum;
    if (abs(chroma) <= 1.1920929e-7) {
        return vec3<f32>(0.0, 0.0, maximum);
    }
    var sector: f32;
    if (maximum == rgb.r) {
        sector = positive_mod((rgb.g - rgb.b) / chroma, 6.0);
    } else if (maximum == rgb.g) {
        sector = (rgb.b - rgb.r) / chroma + 2.0;
    } else {
        sector = (rgb.r - rgb.g) / chroma + 4.0;
    }
    var saturation = 0.0;
    if (abs(maximum) > 1.1920929e-7) {
        saturation = clamp(chroma / maximum, 0.0, 1.0);
    }
    return vec3<f32>(sector / 6.0, saturation, maximum);
}

fn color_curve_hsv_to_rgb(hue: f32, saturation: f32, value: f32) -> vec3<f32> {
    let sector = positive_mod(hue, 1.0) * 6.0;
    let chroma = value * saturation;
    let x = chroma * (1.0 - abs(positive_mod(sector, 2.0) - 1.0));
    var base: vec3<f32>;
    let index = i32(floor(sector));
    if (index == 0) {
        base = vec3<f32>(chroma, x, 0.0);
    } else if (index == 1) {
        base = vec3<f32>(x, chroma, 0.0);
    } else if (index == 2) {
        base = vec3<f32>(0.0, chroma, x);
    } else if (index == 3) {
        base = vec3<f32>(0.0, x, chroma);
    } else if (index == 4) {
        base = vec3<f32>(x, 0.0, chroma);
    } else {
        base = vec3<f32>(chroma, 0.0, x);
    }
    return base + vec3<f32>(value - chroma);
}

fn apply_color_curves(effect: EffectUniform, input: vec3<f32>) -> vec3<f32> {
    var rgb = input;
    if (effect.header.w == 0u) {
        rgb = vec3<f32>(
            sample_color_curve(effect, 0u, 0u, rgb.r),
            sample_color_curve(effect, 0u, 0u, rgb.g),
            sample_color_curve(effect, 0u, 0u, rgb.b),
        );
    } else {
        let luminance = dot(rgb, effect.params.xyz);
        let delta = sample_color_curve(effect, 0u, 0u, luminance) - luminance;
        rgb = rgb + vec3<f32>(delta);
    }
    rgb = vec3<f32>(
        sample_color_curve(effect, 0u, 1u, rgb.r),
        sample_color_curve(effect, 0u, 2u, rgb.g),
        sample_color_curve(effect, 0u, 3u, rgb.b),
    );
    if (effect.params.w < 0.5) {
        return rgb;
    }
    let hsv = color_curve_rgb_to_hsv(rgb);
    let luminance = clamp(dot(rgb, effect.params.xyz), 0.0, 1.0);
    let hue_delta = sample_color_curve(effect, 1u, 0u, hsv.x) - 0.5;
    let saturation_delta =
        sample_color_curve(effect, 1u, 1u, hsv.x) - 0.5 +
        sample_color_curve(effect, 1u, 3u, luminance) - 0.5 +
        sample_color_curve(effect, 2u, 0u, hsv.y) - 0.5;
    let value_delta =
        sample_color_curve(effect, 1u, 2u, hsv.x) - 0.5 +
        sample_color_curve(effect, 2u, 1u, hsv.y) - 0.5;
    return color_curve_hsv_to_rgb(
        positive_mod(hsv.x + hue_delta, 1.0),
        clamp(hsv.y + saturation_delta, 0.0, 1.0),
        hsv.z + value_delta,
    );
}

fn working_to_ap1(rgb: vec3<f32>, space: u32) -> vec3<f32> {
    if (space == 0u) {
        return vec3<f32>(
            dot(vec3<f32>(0.6130974, 0.33952308, 0.047379527), rgb),
            dot(vec3<f32>(0.07019375, 0.91635394, 0.013452331), rgb),
            dot(vec3<f32>(0.020615578, 0.109569736, 0.86981463), rgb),
        );
    }
    if (space == 1u) {
        return vec3<f32>(
            dot(vec3<f32>(0.974895, 0.019599026, 0.005506001), rgb),
            dot(vec3<f32>(0.002179594, 0.99553555, 0.00228489), rgb),
            dot(vec3<f32>(0.004797217, 0.024531983, 0.97067076), rgb),
        );
    }
    if (space == 2u) {
        return vec3<f32>(
            dot(vec3<f32>(0.7357979, 0.21216641, 0.052035686), rgb),
            dot(vec3<f32>(0.047179915, 0.9380458, 0.01477434), rgb),
            dot(vec3<f32>(0.003563646, 0.04114185, 0.95529443), rgb),
        );
    }
    return rgb;
}

fn ap1_to_working(rgb: vec3<f32>, space: u32) -> vec3<f32> {
    if (space == 0u) {
        return vec3<f32>(
            dot(vec3<f32>(1.705051, -0.6217919, -0.083259076), rgb),
            dot(vec3<f32>(-0.13025646, 1.1408046, -0.010548215), rgb),
            dot(vec3<f32>(-0.024003327, -0.12896892, 1.1529723), rgb),
        );
    }
    if (space == 1u) {
        return vec3<f32>(
            dot(vec3<f32>(1.0258248, -0.020053102, -0.005771651), rgb),
            dot(vec3<f32>(-0.002234402, 1.0045865, -0.002352051), rgb),
            dot(vec3<f32>(-0.005013327, -0.025290035, 1.0303034), rgb),
        );
    }
    if (space == 2u) {
        return vec3<f32>(
            dot(vec3<f32>(1.3792142, -0.308864, -0.07035013), rgb),
            dot(vec3<f32>(-0.069334894, 1.0822966, -0.012961794), rgb),
            dot(vec3<f32>(-0.002158984, -0.045459285, 1.0476183), rgb),
        );
    }
    return rgb;
}

fn aces_gamut_compress_channel(
    channel: f32,
    achromatic: f32,
    achromatic_abs: f32,
    limit: f32,
    threshold: f32,
) -> f32 {
    let distance = (achromatic - channel) / achromatic_abs;
    if (distance < threshold) { return channel; }
    let power = 1.2;
    let ratio = (1.0 - threshold) / (limit - threshold);
    let scale = (limit - threshold) / pow(pow(ratio, -power) - 1.0, 1.0 / power);
    let normalized = (distance - threshold) / scale;
    var compressed_distance = threshold + scale;
    if (normalized <= 1.0e20) {
        compressed_distance = threshold +
            scale * normalized / pow(1.0 + pow(normalized, power), 1.0 / power);
    }
    return achromatic - compressed_distance * achromatic_abs;
}

fn aces_gamut_compress(rgb: vec3<f32>) -> vec3<f32> {
    let achromatic = max(rgb.r, max(rgb.g, rgb.b));
    let achromatic_abs = abs(achromatic);
    if (achromatic_abs <= 1.17549435e-38) { return rgb; }
    return vec3<f32>(
        aces_gamut_compress_channel(rgb.r, achromatic, achromatic_abs, 1.147, 0.815),
        aces_gamut_compress_channel(rgb.g, achromatic, achromatic_abs, 1.264, 0.803),
        aces_gamut_compress_channel(rgb.b, achromatic, achromatic_abs, 1.312, 0.880),
    );
}

fn apply_hdr_grading(effect: EffectUniform, rgb: vec3<f32>) -> vec3<f32> {
    let luminance = max(dot(rgb, effect.params.xyz), 1.0e-12);
    let stops = log2(luminance / 0.18);
    let normalized = clamp((stops + 12.0) / 24.0, 0.0, 1.0);
    let maximum_index = max(effect.header.z, 1u) - 1u;
    let coordinate = normalized * f32(maximum_index);
    let lower = u32(floor(coordinate));
    let upper = min(lower + 1u, maximum_index);
    let fraction = coordinate - f32(lower);
    let base_layer = i32(effect.header.y);
    let lower0 = textureLoad(creative_lut_tex, vec3<i32>(i32(lower), 0, base_layer), 0);
    let upper0 = textureLoad(creative_lut_tex, vec3<i32>(i32(upper), 0, base_layer), 0);
    let lower1 = textureLoad(creative_lut_tex, vec3<i32>(i32(lower), 1, base_layer), 0);
    let upper1 = textureLoad(creative_lut_tex, vec3<i32>(i32(upper), 1, base_layer), 0);
    let primary = mix(lower0, upper0, fraction);
    let secondary = mix(lower1, upper1, fraction);
    let balanced = rgb * vec3<f32>(primary.z, primary.w, secondary.x);
    let exposed = balanced * primary.x;
    let exposed_luma = dot(exposed, effect.params.xyz);
    return vec3<f32>(exposed_luma) +
        (exposed - vec3<f32>(exposed_luma)) * primary.y;
}

fn apply_effects(input: vec4<f32>, position: vec2<f32>) -> vec4<f32> {
    if (input.a <= 0.0) { return input; }
    var pixel = input;
    for (var index = 0u; index < 16u; index = index + 1u) {
        if (index >= uniforms.effect_count) { break; }
        let effect = uniforms.effects[index];
        if (effect.header.x == 1u) {
            let exposure = exp2(clamp(effect.params.x, -4.0, 4.0));
            let contrast = clamp(effect.params.y, 0.0, 3.0);
            let saturation = clamp(effect.params.z, 0.0, 3.0);
            let pivot = vec3<f32>(0.18);
            var rgb = (pixel.rgb * exposure - pivot) * contrast + pivot;
            let luma = dot(rgb, effect.color.xyz);
            pixel = vec4<f32>(vec3<f32>(luma) + (rgb - vec3<f32>(luma)) * saturation, pixel.a);
        } else if (effect.header.x == 2u) {
            let intensity = clamp(effect.params.x, 0.0, 1.0);
            if (intensity > 0.0001) {
                let graded = sample_creative_lut(effect, pixel.rgb);
                pixel = vec4<f32>(pixel.rgb + (graded - pixel.rgb) * intensity, pixel.a);
            }
        } else if (effect.header.x == 3u) {
            let center = max((uniforms.geometry.zw - vec2<f32>(1.0)) * 0.5, vec2<f32>(1.0));
            let normalized = (position - center) / center;
            let distance = min(length(normalized), 1.0);
            let feather = clamp(effect.params.y, 0.05, 1.0);
            let gain = 1.0 - smoothstep(1.0 - feather * 0.85, 1.0, distance) * effect.params.x;
            pixel = vec4<f32>(pixel.rgb * gain, pixel.a);
        } else if (effect.header.x == 4u) {
            let noise = grain_noise(position) * clamp(effect.params.x, 0.0, 1.0) * 0.18;
            pixel = vec4<f32>(pixel.rgb + vec3<f32>(noise), pixel.a);
        } else if (effect.header.x == 5u) {
            let normalized_center = (position + vec2<f32>(0.5)) / uniforms.geometry.zw;
            let left = clamp(effect.params.x, 0.0, 1.0);
            let top = clamp(effect.params.y, 0.0, 1.0);
            let right = 1.0 - clamp(effect.params.z, 0.0, 1.0);
            let bottom = 1.0 - clamp(effect.params.w, 0.0, 1.0);
            if (normalized_center.x < left || normalized_center.x >= right ||
                normalized_center.y < top || normalized_center.y >= bottom) {
                pixel = vec4<f32>(0.0);
            }
        } else if (effect.header.x == 6u) {
            let rgb = vec3<f32>(
                dot(effect.params.xyz, pixel.rgb),
                dot(effect.color.xyz, pixel.rgb),
                dot(effect.extra0.xyz, pixel.rgb),
            );
            pixel = vec4<f32>(rgb, pixel.a);
        } else if (effect.header.x == 7u) {
            let lifted = pixel.rgb + effect.color.xyz * (vec3<f32>(1.0) - pixel.rgb);
            let gained = lifted * effect.extra1.xyz;
            let powered = sign(gained) * pow(abs(gained), effect.extra0.xyz);
            pixel = vec4<f32>(powered + effect.params.xyz, pixel.a);
        } else if (effect.header.x == 8u) {
            let sop = pow(
                max(pixel.rgb * effect.params.xyz + effect.color.xyz, vec3<f32>(0.0)),
                effect.extra0.xyz,
            );
            let luma = dot(sop, vec3<f32>(0.2126, 0.7152, 0.0722));
            pixel = vec4<f32>(vec3<f32>(luma) + (sop - vec3<f32>(luma)) * effect.extra1.x, pixel.a);
        } else if (effect.header.x == 9u) {
            pixel = vec4<f32>(apply_color_curves(effect, pixel.rgb), pixel.a);
        } else if (effect.header.x == 10u) {
            let amount = clamp(effect.params.x, 0.0, 1.0);
            let maximum = max(abs(pixel.r), max(abs(pixel.g), abs(pixel.b)));
            let normalization = select(1.0, maximum, maximum > 1.0e20);
            let ap1 = working_to_ap1(pixel.rgb / normalization, effect.header.y);
            let normalized_compressed =
                ap1_to_working(aces_gamut_compress(ap1), effect.header.y);
            let output_maximum = max(
                abs(normalized_compressed.r),
                max(abs(normalized_compressed.g), abs(normalized_compressed.b)),
            );
            let safe_normalization = select(
                normalization,
                min(normalization, 3.402823466e38 / output_maximum),
                output_maximum > 1.0,
            );
            let compressed = normalized_compressed * safe_normalization;
            pixel = vec4<f32>(mix(pixel.rgb, compressed, amount), pixel.a);
        } else if (effect.header.x == 11u) {
            let threshold = effect.params.x;
            let rolloff = effect.params.y;
            let strength = clamp(effect.params.z, 0.0, 1.0);
            let peak = max(pixel.r, max(pixel.g, pixel.b));
            if (peak > threshold && strength > 0.000001) {
                let transition = clamp((peak - threshold) / rolloff, 0.0, 1.0);
                let weight = transition * transition * (3.0 - 2.0 * transition) * strength;
                let luminance = dot(pixel.rgb, effect.color.xyz);
                pixel = vec4<f32>(mix(pixel.rgb, vec3<f32>(luminance), weight), pixel.a);
            }
        } else if (effect.header.x == 12u) {
            pixel = vec4<f32>(apply_hdr_grading(effect, pixel.rgb), pixel.a);
        }
    }
    return pixel;
}
"#;

const GPU_MATTE_MIX_SHADER: &str = r#"
struct VsOut {
    @builtin(position) position: vec4<f32>,
};

@group(0) @binding(0) var base_tex: texture_2d<f32>;
@group(0) @binding(1) var graded_tex: texture_2d<f32>;
@group(0) @binding(2) var matte_tex: texture_2d<f32>;

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VsOut {
    var positions = array<vec2<f32>, 4>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 1.0, -1.0),
        vec2<f32>(-1.0,  1.0),
        vec2<f32>( 1.0,  1.0),
    );
    var out: VsOut;
    out.position = vec4<f32>(positions[vertex_index], 0.0, 1.0);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let coordinate = vec2<i32>(floor(in.position.xy));
    let base = textureLoad(base_tex, coordinate, 0);
    let graded = textureLoad(graded_tex, coordinate, 0);
    let matte = clamp(textureLoad(matte_tex, coordinate, 0).a, 0.0, 1.0);
    return vec4<f32>(mix(base.rgb, graded.rgb, matte), base.a);
}
"#;

/// Classification of whether GPU compositing is possible for a set of layers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GpuCompositingCapability {
    /// GPU compositing is possible: all layers are GPU-resident, use only
    /// supported blend modes, and have no effect graphs requiring CPU execution.
    GpuNative,
    /// GPU compositing is possible for the layer structure, but layers must
    /// be uploaded from CPU first. The composited result stays on GPU.
    GpuWithUpload,
    /// GPU compositing is not possible. Falls back to CPU compositing with
    /// upload of the final result.
    CpuFallback {
        /// First reason preventing GPU compositing.
        reason: GpuCompositingBlockerReason,
    },
}

/// Reasons GPU compositing cannot be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GpuCompositingBlockerReason {
    /// An effect graph requires CPU execution or has not been lowered to a GPU shader.
    EffectRequiresCpu,
    /// The layer has a transform the GPU compositor cannot sample correctly.
    UnsupportedTransform,
    /// A source frame is not already GPU-resident and uploads were disallowed.
    FrameNotGpuResident,
    /// GPU compositor is not initialized (device/queue unavailable).
    GpuUnavailable,
}

impl GpuCompositingBlockerReason {
    /// Machine-readable code for diagnostics.
    pub fn code(&self) -> &'static str {
        match self {
            Self::EffectRequiresCpu => "effect_requires_cpu",
            Self::UnsupportedTransform => "unsupported_transform",
            Self::FrameNotGpuResident => "frame_not_gpu_resident",
            Self::GpuUnavailable => "gpu_unavailable",
        }
    }

    /// Human-readable description.
    pub fn description(&self) -> &'static str {
        match self {
            Self::EffectRequiresCpu => {
                "Effect graph requires CPU execution or has no GPU shader lowering"
            }
            Self::UnsupportedTransform => "Transform cannot be represented by GPU compositor",
            Self::FrameNotGpuResident => "Frame requires CPU-to-GPU upload before compositing",
            Self::GpuUnavailable => "GPU device/queue not available for compositing",
        }
    }
}

/// Structured diagnostics for GPU vs CPU compositing path selection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuCompositingDiagnostics {
    /// Single GPU working layers reused without recording a composite pass.
    pub gpu_passthrough_frames: u64,
    /// Number of compositing operations that used the GPU-native path.
    pub gpu_native_composites: u64,
    /// Number of compositing operations that uploaded CPU layers for GPU compositing.
    pub gpu_with_upload_composites: u64,
    /// Typed non-color DataTexture uploads consumed by the numeric bypass.
    pub data_texture_uploads: u64,
    /// Dedicated two-input working-linear Cross Dissolve passes.
    pub gpu_cross_dissolve_passes: u64,
    /// Number of compositing operations that fell back to CPU compositing.
    pub cpu_fallback_composites: u64,
    /// Total pixels processed through GPU compositing.
    pub gpu_composited_pixels: u64,
    /// Total pixels processed through CPU compositing.
    pub cpu_composited_pixels: u64,
    /// First blocker reason observed (for health reports).
    pub first_blocker: Option<GpuCompositingBlockerReason>,
}

impl GpuCompositingDiagnostics {
    /// Accumulate another diagnostics snapshot.
    pub fn accumulate(&mut self, other: Self) {
        self.gpu_passthrough_frames =
            self.gpu_passthrough_frames.saturating_add(other.gpu_passthrough_frames);
        self.gpu_native_composites =
            self.gpu_native_composites.saturating_add(other.gpu_native_composites);
        self.gpu_with_upload_composites =
            self.gpu_with_upload_composites.saturating_add(other.gpu_with_upload_composites);
        self.data_texture_uploads =
            self.data_texture_uploads.saturating_add(other.data_texture_uploads);
        self.gpu_cross_dissolve_passes =
            self.gpu_cross_dissolve_passes.saturating_add(other.gpu_cross_dissolve_passes);
        self.cpu_fallback_composites =
            self.cpu_fallback_composites.saturating_add(other.cpu_fallback_composites);
        self.gpu_composited_pixels =
            self.gpu_composited_pixels.saturating_add(other.gpu_composited_pixels);
        self.cpu_composited_pixels =
            self.cpu_composited_pixels.saturating_add(other.cpu_composited_pixels);
        if self.first_blocker.is_none() {
            self.first_blocker = other.first_blocker;
        }
    }

    /// Whether any compositing used the GPU-native path.
    pub fn has_gpu_native(&self) -> bool {
        self.gpu_passthrough_frames > 0 || self.gpu_native_composites > 0
    }

    /// Whether any compositing fell back to CPU.
    pub fn has_cpu_fallback(&self) -> bool {
        self.cpu_fallback_composites > 0
    }
}

/// Evaluate GPU compositing capability for a set of timeline layers.
///
/// Returns the capability classification and structured diagnostics about
/// why GPU compositing is or is not possible.
pub fn evaluate_gpu_compositing_capability(
    has_any_unsupported_transform: bool,
    all_frames_gpu_resident: bool,
) -> GpuCompositingCapability {
    if has_any_unsupported_transform {
        return GpuCompositingCapability::CpuFallback {
            reason: GpuCompositingBlockerReason::UnsupportedTransform,
        };
    }
    if all_frames_gpu_resident {
        GpuCompositingCapability::GpuNative
    } else {
        GpuCompositingCapability::GpuWithUpload
    }
}

/// Source layer accepted by the native GPU working-space compositor.
#[derive(Debug, Clone, Copy)]
pub enum GpuCompositeLayerSource<'a> {
    /// CPU working-space frame that will be uploaded to an Rgba32Float texture.
    CpuFrame(&'a CpuColorFrame),
    /// CPU RGBA numeric data uploaded as `NonColorData + DataTexture` and
    /// admitted to working compositing only through the explicit bypass.
    CpuDataTexture(&'a CpuColorFrame),
    /// GPU-resident working-space frame that will be sampled directly.
    GpuFrame(&'a GpuColorFrameHandle),
    /// Solid working-space color drawn directly by shader uniform.
    SolidColor(Color),
    /// Adjustment layer that processes the current working-space accumulator.
    Adjustment,
}

/// One layer in a GPU working-space composite request.
#[derive(Debug, Clone, Copy)]
pub struct GpuCompositeLayer<'a> {
    /// Source pixels or procedural solid.
    pub source: GpuCompositeLayerSource<'a>,
    /// Straight-alpha layer opacity.
    pub opacity: f32,
    /// Canonical Timeline blend mode evaluated in scene-linear working space.
    pub blend_mode: BlendMode,
    /// Timeline affine transform. Media and solids accept invertible transforms.
    /// Adjustment layers require identity because they process the destination accumulator.
    pub transform: [f32; 6],
    /// Lowered pointwise effect plan evaluated in working-linear space.
    pub effect_plan: Option<&'a CompiledEffectGpuPlan>,
    /// Stable timeline frame seed used by temporal effect operations.
    pub frame_seed: i64,
}

/// Request for recording a GPU working-space composite.
pub struct GpuCompositeRequest<'a> {
    /// Output width.
    pub width: u32,
    /// Output height.
    pub height: u32,
    /// Working color space of the composite output.
    pub working_color_space: mondrian_core::WorkingColorSpace,
    /// Layers in bottom-to-top order.
    pub layers: &'a [GpuCompositeLayer<'a>],
}

/// Result of recording a GPU working-space composite.
pub struct GpuCompositeRecord {
    /// GPU handle for the working-space composite texture.
    pub output: GpuColorFrameHandle,
    /// Diagnostics proving which compositing path executed.
    pub diagnostics: GpuCompositingDiagnostics,
}

/// Result of recording one standalone GPU point-effect pass.
pub struct GpuPointEffectRecord {
    /// GPU-resident frame carrying the same effect-domain identity as the input.
    pub output: GpuColorFrameHandle,
    /// Number of pixels evaluated by the point-effect shader.
    pub processed_pixels: u64,
}

/// Result of recording one refined GPU qualifier dispatch.
pub(crate) struct GpuQualifierRecord {
    /// AlphaMask-domain output retained in the shared resource table.
    pub output: GpuColorFrameHandle,
    /// Internal pass targets that must live until command submission.
    pub scratch: Vec<GpuColorFrameResource<GpuColorFrameWgpuResource>>,
}

/// Result of materializing one procedural solid into a GPU working frame.
pub struct GpuSolidSourceRecord {
    /// GPU-resident working-linear frame containing the unblended solid color.
    pub output: GpuColorFrameHandle,
    /// Number of pixels generated by the procedural source pass.
    pub materialized_pixels: u64,
}

/// Paged uniform-arena evidence for the compositor hot path.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuCompositorUniformArenaDiagnostics {
    /// Persistent GPU buffers created for this arena.
    pub buffer_creations: u64,
    /// Uniform payloads written into reserved slots.
    pub uniform_writes: u64,
    /// Peak slots reserved within one frame lifetime.
    pub high_watermark_slots: u32,
    /// Peak reusable uniform-buffer pages required by one frame lifetime.
    pub high_watermark_pages: u32,
    /// Frame-lifetime resets after ordered submission.
    pub frame_resets: u64,
}

/// Point-in-time evidence for compositor texture-binding object reuse.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuCompositorTextureBindingDiagnostics {
    /// Texture bind groups created since compositor construction.
    pub bind_group_creations: u64,
    /// Existing texture bind groups reused without backend object creation.
    pub cache_hits: u64,
}

/// Errors returned by native GPU working-space compositing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GpuCompositeError {
    /// A creative LUT could not be materialized under the device/cache contract.
    #[error(transparent)]
    CreativeLut(#[from] crate::GpuCreativeLutError),
    /// Output dimensions must be non-zero.
    #[error("GPU composite output dimensions must be non-zero, got {width}x{height}")]
    EmptyExtent {
        /// Requested width.
        width: u32,
        /// Requested height.
        height: u32,
    },
    /// A layer shape requires CPU fallback.
    #[error("GPU composite is blocked by {reason:?}")]
    Blocked {
        /// First blocker reason.
        reason: GpuCompositingBlockerReason,
    },
    /// A source frame has the wrong descriptor for this composite.
    #[error("GPU composite source frame descriptor mismatch")]
    SourceDescriptorMismatch {
        /// Expected descriptor.
        expected: ColorFrameDescriptor,
        /// Actual descriptor.
        actual: ColorFrameDescriptor,
    },
    /// Public compositing and point-effect inputs must be straight-compatible.
    #[error("GPU composite input must carry straight-compatible coverage, got {actual:?}")]
    InputNotStraightCompatibleAlpha {
        /// Rejected RGB/coverage association.
        actual: crate::ColorFrameAlpha,
    },
    /// An adjustment layer did not provide a non-identity GPU effect plan.
    #[error("GPU adjustment layer requires a non-identity effect plan")]
    AdjustmentMissingEffectPlan,
    /// An effect must be surrounded by renderer-owned OCIO passes before composition.
    #[error("GPU working compositor cannot execute effect domain {domain:?} inline")]
    EffectDomainRequiresExternalPass {
        /// Exact domain that the external pass sequence must materialize.
        domain: EffectColorDomain,
    },
    /// A standalone point pass was given a non-RGB data or alpha domain.
    #[error("GPU point-effect pass requires an RGB frame, got {domain:?}")]
    PointEffectRequiresRgbIntermediate {
        /// Unsupported plan domain.
        domain: EffectColorDomain,
    },
    /// The intermediate frame does not match the effect plan's exact domain contract.
    #[error("GPU point-effect intermediate descriptor mismatch")]
    PointEffectDescriptorMismatch {
        /// Descriptor required by the plan.
        expected: ColorFrameDescriptor,
        /// Supplied frame descriptor.
        actual: ColorFrameDescriptor,
    },
    /// The point pass requires a floating-point sampled texture.
    #[error("GPU point-effect input texture must be floating point, got {texture_format:?}")]
    PointEffectTextureFormatUnsupported {
        /// Unsupported input texture format.
        texture_format: GpuColorFrameTextureFormat,
    },
    /// Alpha-mask execution requires RGBA32F source and mask textures.
    #[error("GPU alpha-mask pass requires RGBA32F textures, got {texture_format:?}")]
    AlphaMaskTextureFormatUnsupported {
        /// Unsupported source or mask texture format.
        texture_format: GpuColorFrameTextureFormat,
    },
    /// Internal qualifier pass planning produced no output target.
    #[error("GPU qualifier pass plan is empty")]
    QualifierPassPlanEmpty,
    /// Renderer frame identity allocation is exhausted.
    #[error(transparent)]
    FrameId(#[from] GpuColorFrameIdAllocationError),
    /// A renderer frame handle could not be created.
    #[error("GPU composite output handle error: {0}")]
    OutputHandle(#[from] crate::GpuColorFrameHandleError),
    /// Uploading a CPU source layer failed.
    #[error("GPU composite layer upload error: {0:?}")]
    Upload(crate::GpuColorFrameUploadError),
    /// The shared resource table rejected an inserted resource.
    #[error("GPU composite resource table error: {0:?}")]
    ResourceTable(crate::GpuColorFrameResourceTableError),
    /// Transition progress must be a finite coefficient before clamping.
    #[error("GPU Cross Dissolve progress must be finite, got IEEE-754 bits {progress_bits:#010x}")]
    NonFiniteTransitionProgress {
        /// Rejected coefficient encoded without weakening structural equality.
        progress_bits: u32,
    },
}

/// Runtime for recording GPU working-space composites.
pub struct GpuFrameCompositor {
    pipeline: wgpu::RenderPipeline,
    matte_mix_pipeline: wgpu::RenderPipeline,
    matte_mix_texture_layout: wgpu::BindGroupLayout,
    layer_texture_layout: wgpu::BindGroupLayout,
    accum_texture_layout: wgpu::BindGroupLayout,
    layer_texture_cache_key: GpuColorFrameBindGroupCacheKey,
    accum_texture_cache_key: GpuColorFrameBindGroupCacheKey,
    uniform_layout: wgpu::BindGroupLayout,
    uniform_size: u64,
    uniform_stride: u64,
    uniform_arena: Mutex<GpuCompositeUniformArenaState>,
    texture_bind_group_creations: AtomicU64,
    texture_bind_group_cache_hits: AtomicU64,
    sampler: wgpu::Sampler,
    procedural_layer_bind_group: wgpu::BindGroup,
    procedural_accum_bind_group: wgpu::BindGroup,
    creative_luts: GpuCreativeLutRuntime,
}

#[derive(Clone, Copy)]
enum GpuCompositeTextureBinding<'a> {
    Resource(&'a GpuColorFrameWgpuResource),
    ProceduralDummy,
}

struct GpuCompositeUniformArenaState {
    pages: Vec<GpuCompositeUniformPage>,
    next_slot: usize,
    diagnostics: GpuCompositorUniformArenaDiagnostics,
}

struct GpuCompositeUniformPage {
    buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

fn create_uniform_page(
    device: &wgpu::Device,
    uniform_layout: &wgpu::BindGroupLayout,
    uniform_size: u64,
    uniform_stride: u64,
) -> GpuCompositeUniformPage {
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("mondrian_gpu_working_compositor_uniform_page"),
        size: uniform_stride.saturating_mul(GPU_COMPOSITOR_UNIFORM_PAGE_SLOTS as u64),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("mondrian_gpu_working_compositor_uniform_page_bind_group"),
        layout: uniform_layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                buffer: &buffer,
                offset: 0,
                size: wgpu::BufferSize::new(uniform_size),
            }),
        }],
    });
    GpuCompositeUniformPage { buffer, bind_group }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuCompositeUniforms {
    opacity: f32,
    source_kind: u32,
    effect_count: u32,
    blend_mode: u32,
    frame_seed_lo: u32,
    frame_seed_hi: u32,
    mask_controls: [u32; 2],
    solid_color: [f32; 4],
    inv_transform0: [f32; 4],
    inv_transform1: [f32; 4],
    geometry: [f32; 4],
    effects: [GpuEffectUniform; MAX_FUSED_GPU_EFFECT_OPS],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuEffectUniform {
    header: [u32; 4],
    params: [f32; 4],
    color: [f32; 4],
    extra0: [f32; 4],
    extra1: [f32; 4],
}

impl GpuFrameCompositor {
    /// Create a GPU working-space compositor runtime for a wgpu device.
    pub fn new(
        device: &wgpu::Device,
    ) -> Result<Self, GpuColorFrameBindGroupCacheKeyAllocationError> {
        Self::new_with_creative_lut_cache(device, crate::GpuCreativeLutCacheConfig::default())
    }

    /// Create a compositor with explicit bounded creative-LUT residency.
    pub fn new_with_creative_lut_cache(
        device: &wgpu::Device,
        creative_lut_cache_config: crate::GpuCreativeLutCacheConfig,
    ) -> Result<Self, GpuColorFrameBindGroupCacheKeyAllocationError> {
        let layer_texture_cache_key = GpuColorFrameBindGroupCacheKey::allocate()?;
        let accum_texture_cache_key = GpuColorFrameBindGroupCacheKey::allocate()?;
        let creative_luts = GpuCreativeLutRuntime::new(device, creative_lut_cache_config);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mondrian_gpu_working_compositor_shader"),
            source: wgpu::ShaderSource::Wgsl(GPU_COMPOSITOR_SHADER.into()),
        });
        let layer_texture_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("mondrian_gpu_working_compositor_layer_texture"),
                entries: &[
                    texture_binding(0),
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                        count: None,
                    },
                ],
            });
        let accum_texture_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("mondrian_gpu_working_compositor_accum_texture"),
                entries: &[texture_binding(0)],
            });
        let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mondrian_gpu_working_compositor_uniforms"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(
                        std::mem::size_of::<GpuCompositeUniforms>() as u64,
                    ),
                },
                count: None,
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mondrian_gpu_working_compositor_layout"),
            bind_group_layouts: &[
                Some(&layer_texture_layout),
                Some(&accum_texture_layout),
                Some(&uniform_layout),
                Some(creative_luts.layout()),
            ],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian_gpu_working_compositor_pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba32Float,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                ..wgpu::PrimitiveState::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let matte_mix_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mondrian_gpu_matte_mix_shader"),
            source: wgpu::ShaderSource::Wgsl(GPU_MATTE_MIX_SHADER.into()),
        });
        let matte_mix_texture_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("mondrian_gpu_matte_mix_textures"),
                entries: &[texture_binding(0), texture_binding(1), texture_binding(2)],
            });
        let matte_mix_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mondrian_gpu_matte_mix_layout"),
            bind_group_layouts: &[Some(&matte_mix_texture_layout)],
            immediate_size: 0,
        });
        let matte_mix_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian_gpu_matte_mix_pipeline"),
            layout: Some(&matte_mix_layout),
            vertex: wgpu::VertexState {
                module: &matte_mix_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &matte_mix_shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba32Float,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                ..wgpu::PrimitiveState::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("mondrian_gpu_working_compositor_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..wgpu::SamplerDescriptor::default()
        });
        let uniform_size = std::mem::size_of::<GpuCompositeUniforms>() as u64;
        let uniform_alignment =
            u64::from(device.limits().min_uniform_buffer_offset_alignment.max(1));
        let uniform_stride = uniform_size.div_ceil(uniform_alignment) * uniform_alignment;
        let initial_uniform_page =
            create_uniform_page(device, &uniform_layout, uniform_size, uniform_stride);
        let procedural_dummy = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("mondrian_gpu_procedural_dummy"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let procedural_dummy_view =
            procedural_dummy.create_view(&wgpu::TextureViewDescriptor::default());
        let procedural_layer_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mondrian_gpu_working_compositor_procedural_layer_binding"),
            layout: &layer_texture_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&procedural_dummy_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        let procedural_accum_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mondrian_gpu_working_compositor_procedural_accum_binding"),
            layout: &accum_texture_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&procedural_dummy_view),
            }],
        });
        Ok(Self {
            pipeline,
            matte_mix_pipeline,
            matte_mix_texture_layout,
            layer_texture_layout,
            accum_texture_layout,
            layer_texture_cache_key,
            accum_texture_cache_key,
            uniform_layout,
            uniform_size,
            uniform_stride,
            uniform_arena: Mutex::new(GpuCompositeUniformArenaState {
                pages: vec![initial_uniform_page],
                next_slot: 0,
                diagnostics: GpuCompositorUniformArenaDiagnostics {
                    buffer_creations: 1,
                    ..GpuCompositorUniformArenaDiagnostics::default()
                },
            }),
            texture_bind_group_creations: AtomicU64::new(2),
            texture_bind_group_cache_hits: AtomicU64::new(0),
            sampler,
            procedural_layer_bind_group,
            procedural_accum_bind_group,
            creative_luts,
        })
    }

    /// Reset frame-local uniform slots after the caller has ordered submission.
    pub fn clear_frame_resources(&self) {
        let mut arena = self.uniform_arena.lock();
        arena.next_slot = 0;
        arena.diagnostics.frame_resets = arena.diagnostics.frame_resets.saturating_add(1);
    }

    /// Return point-in-time evidence for persistent uniform-arena reuse.
    pub fn uniform_arena_diagnostics(&self) -> GpuCompositorUniformArenaDiagnostics {
        self.uniform_arena.lock().diagnostics
    }

    /// Return point-in-time compositor texture-binding reuse evidence.
    pub fn texture_binding_diagnostics(&self) -> GpuCompositorTextureBindingDiagnostics {
        GpuCompositorTextureBindingDiagnostics {
            bind_group_creations: self.texture_bind_group_creations.load(Ordering::Relaxed),
            cache_hits: self.texture_bind_group_cache_hits.load(Ordering::Relaxed),
        }
    }

    /// Return device-resident creative-LUT upload and cache evidence.
    pub fn creative_lut_diagnostics(&self) -> crate::GpuCreativeLutCacheDiagnostics {
        self.creative_luts.diagnostics()
    }

    /// Record a GPU working-space composite into the supplied command encoder
    /// and resource table.
    pub fn record(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        ids: &mut GpuColorFrameIdAllocator,
        table: &mut GpuColorFrameResourceTable<GpuColorFrameWgpuResource>,
        resource_pool: Option<&GpuColorFrameWgpuResourcePool>,
        request: GpuCompositeRequest<'_>,
    ) -> Result<GpuCompositeRecord, GpuCompositeError> {
        validate_request(&request)?;
        if let Some(output) = single_layer_gpu_passthrough(&request) {
            table.get(output).map_err(GpuCompositeError::ResourceTable)?;
            return Ok(GpuCompositeRecord {
                output: output.clone(),
                diagnostics: GpuCompositingDiagnostics {
                    gpu_passthrough_frames: 1,
                    ..GpuCompositingDiagnostics::default()
                },
            });
        }
        let width = request.width;
        let height = request.height;
        let output_descriptor = ColorFrameDescriptor {
            width,
            height,
            color_space: request.working_color_space.into(),
            domain: ColorFrameDomain::Working,
            encoding: ColorFrameEncoding::LinearFloat,
            residency: ColorFrameResidency::Gpu,
            alpha: crate::ColorFrameAlpha::StraightCoverage,
        };
        let target_a = create_working_resource(
            device,
            ids,
            output_descriptor,
            "gpu-composite-accum-a",
            resource_pool,
        )?;
        let target_b = create_working_resource(
            device,
            ids,
            output_descriptor,
            "gpu-composite-accum-b",
            resource_pool,
        )?;
        clear_working_texture(
            encoder,
            &target_a.resource().texture_view,
            wgpu::Color::TRANSPARENT,
        );

        let mut transient_uploads = Vec::new();
        let mut uploaded_cpu_layers = false;
        let mut data_texture_uploads = 0_u64;
        let mut src_is_a = true;
        for (index, layer) in request.layers.iter().enumerate() {
            if layer_has_zero_contribution(layer) {
                continue;
            }
            let (accum, dst) = if src_is_a {
                (&target_a, &target_b)
            } else {
                (&target_b, &target_a)
            };
            let (layer_binding, source_kind, solid_color, source_size) = match layer.source {
                GpuCompositeLayerSource::CpuFrame(frame) => {
                    uploaded_cpu_layers = true;
                    let descriptor = frame.descriptor();
                    let upload = GpuColorFrameUploadPlan::from_cpu_color_frame(
                        ids.allocate()?,
                        frame,
                        GpuColorFrameTextureFormat::Rgba32Float,
                        format!("gpu-composite-layer-{index}"),
                    )
                    .map_err(GpuCompositeError::Upload)?;
                    let uploaded = GpuColorFrameUploader::upload(device, queue, &upload);
                    transient_uploads.push(uploaded);
                    (
                        GpuCompositeTextureBinding::Resource(
                            transient_uploads
                                .last()
                                .expect("uploaded layer just pushed")
                                .resource(),
                        ),
                        0,
                        [0.0, 0.0, 0.0, 0.0],
                        [descriptor.width as f32, descriptor.height as f32],
                    )
                }
                GpuCompositeLayerSource::CpuDataTexture(frame) => {
                    uploaded_cpu_layers = true;
                    data_texture_uploads = data_texture_uploads.saturating_add(1);
                    let descriptor = frame.descriptor();
                    let upload = GpuColorFrameUploadPlan::from_cpu_data_texture(
                        ids.allocate()?,
                        frame,
                        format!("gpu-composite-data-texture-{index}"),
                    )
                    .map_err(GpuCompositeError::Upload)?;
                    let uploaded = GpuColorFrameUploader::upload(device, queue, &upload);
                    transient_uploads.push(uploaded);
                    (
                        GpuCompositeTextureBinding::Resource(
                            transient_uploads
                                .last()
                                .expect("uploaded data texture just pushed")
                                .resource(),
                        ),
                        0,
                        [0.0, 0.0, 0.0, 0.0],
                        [descriptor.width as f32, descriptor.height as f32],
                    )
                }
                GpuCompositeLayerSource::GpuFrame(handle) => {
                    let resource = table.get(handle).map_err(GpuCompositeError::ResourceTable)?;
                    let descriptor = handle.descriptor();
                    (
                        GpuCompositeTextureBinding::Resource(resource.resource()),
                        0,
                        [0.0, 0.0, 0.0, 0.0],
                        [descriptor.width as f32, descriptor.height as f32],
                    )
                }
                GpuCompositeLayerSource::SolidColor(color) => (
                    GpuCompositeTextureBinding::ProceduralDummy,
                    1,
                    [color.r, color.g, color.b, color.a],
                    [width as f32, height as f32],
                ),
                GpuCompositeLayerSource::Adjustment => (
                    GpuCompositeTextureBinding::ProceduralDummy,
                    2,
                    [0.0, 0.0, 0.0, 0.0],
                    [width as f32, height as f32],
                ),
            };
            let inv_transform = invert_affine(layer.transform)
                .expect("validate_request rejects unsupported transforms");
            self.record_layer_pass(
                device,
                queue,
                encoder,
                GpuCompositeTextureBinding::Resource(accum.resource()),
                &dst.resource().texture_view,
                layer_binding,
                GpuCompositeUniforms {
                    opacity: layer.opacity.clamp(0.0, 1.0),
                    source_kind,
                    effect_count: layer
                        .effect_plan
                        .map_or(0, |plan| plan.operations().len() as u32),
                    blend_mode: gpu_blend_mode_id(layer.blend_mode),
                    frame_seed_lo: layer.frame_seed as u32,
                    frame_seed_hi: (layer.frame_seed >> 32) as u32,
                    mask_controls: [0; 2],
                    solid_color,
                    inv_transform0: [
                        inv_transform[0],
                        inv_transform[1],
                        inv_transform[2],
                        inv_transform[3],
                    ],
                    inv_transform1: [inv_transform[4], inv_transform[5], 0.0, 0.0],
                    geometry: [width as f32, height as f32, source_size[0], source_size[1]],
                    effects: [GpuEffectUniform::zeroed(); MAX_FUSED_GPU_EFFECT_OPS],
                },
                layer.effect_plan,
            )?;
            src_is_a = !src_is_a;
        }

        let (output_resource, retained_resource) = if src_is_a {
            (target_a, target_b)
        } else {
            (target_b, target_a)
        };
        let output = output_resource.handle().clone();
        table.insert(retained_resource).map_err(GpuCompositeError::ResourceTable)?;
        table.insert(output_resource).map_err(GpuCompositeError::ResourceTable)?;
        let mut diagnostics = GpuCompositingDiagnostics {
            gpu_composited_pixels: u64::from(width).saturating_mul(u64::from(height)),
            ..GpuCompositingDiagnostics::default()
        };
        if uploaded_cpu_layers {
            diagnostics.gpu_with_upload_composites = 1;
        } else {
            diagnostics.gpu_native_composites = 1;
        }
        diagnostics.data_texture_uploads = data_texture_uploads;
        Ok(GpuCompositeRecord { output, diagnostics })
    }

    /// Record a standalone point-effect pass over a typed effect-domain frame.
    ///
    /// The pass does not blend or relabel pixels. Renderer-owned OCIO passes
    /// are responsible for entering and leaving this exact intermediate domain.
    pub fn record_point_effect_pass(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        ids: &mut GpuColorFrameIdAllocator,
        table: &mut GpuColorFrameResourceTable<GpuColorFrameWgpuResource>,
        resource_pool: Option<&GpuColorFrameWgpuResourcePool>,
        input: &GpuColorFrameHandle,
        plan: &CompiledEffectGpuPlan,
        frame_seed: i64,
    ) -> Result<GpuPointEffectRecord, GpuCompositeError> {
        validate_point_effect_input(input, plan)?;
        let input_resource = table.get(input).map_err(GpuCompositeError::ResourceTable)?;
        let descriptor = input.descriptor();
        let output_resource = create_working_resource(
            device,
            ids,
            descriptor,
            "gpu-point-effect-output",
            resource_pool,
        )?;
        let width = descriptor.width;
        let height = descriptor.height;
        self.record_layer_pass(
            device,
            queue,
            encoder,
            GpuCompositeTextureBinding::ProceduralDummy,
            &output_resource.resource().texture_view,
            GpuCompositeTextureBinding::Resource(input_resource.resource()),
            GpuCompositeUniforms {
                opacity: 1.0,
                source_kind: 3,
                effect_count: plan.operations().len() as u32,
                blend_mode: gpu_blend_mode_id(BlendMode::Normal),
                frame_seed_lo: frame_seed as u32,
                frame_seed_hi: (frame_seed >> 32) as u32,
                mask_controls: [0; 2],
                solid_color: [0.0; 4],
                inv_transform0: [1.0, 0.0, 0.0, 0.0],
                inv_transform1: [1.0, 0.0, 0.0, 0.0],
                geometry: [width as f32, height as f32, width as f32, height as f32],
                effects: [GpuEffectUniform::zeroed(); MAX_FUSED_GPU_EFFECT_OPS],
            },
            Some(plan),
        )?;
        let output = output_resource.handle().clone();
        table.insert(output_resource).map_err(GpuCompositeError::ResourceTable)?;
        Ok(GpuPointEffectRecord {
            output,
            processed_pixels: u64::from(width).saturating_mul(u64::from(height)),
        })
    }

    /// Interpolate two already prepared working-linear source branches.
    ///
    /// RGB is weighted in premultiplied coverage and converted back to the
    /// public straight-alpha contract. This is not equivalent to submitting
    /// two ordinary source-over layers with complementary opacity.
    #[allow(clippy::too_many_arguments)]
    pub fn record_cross_dissolve_pass(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        ids: &mut GpuColorFrameIdAllocator,
        table: &mut GpuColorFrameResourceTable<GpuColorFrameWgpuResource>,
        resource_pool: Option<&GpuColorFrameWgpuResourcePool>,
        left: &GpuColorFrameHandle,
        right: &GpuColorFrameHandle,
        progress: f32,
        working_color_space: mondrian_core::WorkingColorSpace,
    ) -> Result<GpuCompositeRecord, GpuCompositeError> {
        if !progress.is_finite() {
            return Err(GpuCompositeError::NonFiniteTransitionProgress {
                progress_bits: progress.to_bits(),
            });
        }
        let left_descriptor = left.descriptor();
        let right_descriptor = right.descriptor();
        let validation_layers = [
            GpuCompositeLayer {
                source: GpuCompositeLayerSource::GpuFrame(left),
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_plan: None,
                frame_seed: 0,
            },
            GpuCompositeLayer {
                source: GpuCompositeLayerSource::GpuFrame(right),
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_plan: None,
                frame_seed: 0,
            },
        ];
        validate_request(&GpuCompositeRequest {
            width: left_descriptor.width,
            height: left_descriptor.height,
            working_color_space,
            layers: &validation_layers,
        })?;
        if right_descriptor.width != left_descriptor.width
            || right_descriptor.height != left_descriptor.height
        {
            let expected = ColorFrameDescriptor {
                width: left_descriptor.width,
                height: left_descriptor.height,
                ..right_descriptor
            };
            return Err(GpuCompositeError::SourceDescriptorMismatch {
                expected,
                actual: right_descriptor,
            });
        }
        let left_resource = table.get(left).map_err(GpuCompositeError::ResourceTable)?;
        let right_resource = table.get(right).map_err(GpuCompositeError::ResourceTable)?;
        if progress <= 0.0 {
            return Ok(GpuCompositeRecord {
                output: left.clone(),
                diagnostics: GpuCompositingDiagnostics {
                    gpu_passthrough_frames: 1,
                    ..GpuCompositingDiagnostics::default()
                },
            });
        }
        if progress >= 1.0 {
            return Ok(GpuCompositeRecord {
                output: right.clone(),
                diagnostics: GpuCompositingDiagnostics {
                    gpu_passthrough_frames: 1,
                    ..GpuCompositingDiagnostics::default()
                },
            });
        }
        let descriptor = ColorFrameDescriptor {
            alpha: crate::ColorFrameAlpha::StraightCoverage,
            ..left_descriptor
        };
        let output_resource = create_working_resource(
            device,
            ids,
            descriptor,
            "gpu-cross-dissolve-output",
            resource_pool,
        )?;
        self.record_layer_pass(
            device,
            queue,
            encoder,
            GpuCompositeTextureBinding::Resource(left_resource.resource()),
            &output_resource.resource().texture_view,
            GpuCompositeTextureBinding::Resource(right_resource.resource()),
            GpuCompositeUniforms {
                opacity: progress,
                source_kind: 5,
                effect_count: 0,
                blend_mode: gpu_blend_mode_id(BlendMode::Normal),
                frame_seed_lo: 0,
                frame_seed_hi: 0,
                mask_controls: [0; 2],
                solid_color: [0.0; 4],
                inv_transform0: [1.0, 0.0, 0.0, 0.0],
                inv_transform1: [1.0, 0.0, 0.0, 0.0],
                geometry: [
                    descriptor.width as f32,
                    descriptor.height as f32,
                    descriptor.width as f32,
                    descriptor.height as f32,
                ],
                effects: [GpuEffectUniform::zeroed(); MAX_FUSED_GPU_EFFECT_OPS],
            },
            None,
        )?;
        let output = output_resource.handle().clone();
        table.insert(output_resource).map_err(GpuCompositeError::ResourceTable)?;
        Ok(GpuCompositeRecord {
            output,
            diagnostics: GpuCompositingDiagnostics {
                gpu_native_composites: 1,
                gpu_cross_dissolve_passes: 1,
                gpu_composited_pixels: u64::from(descriptor.width)
                    .saturating_mul(u64::from(descriptor.height)),
                ..GpuCompositingDiagnostics::default()
            },
        })
    }

    /// Blend an already processed adjustment frame over its original accumulator.
    ///
    /// Unlike a two-layer general composite, this graph node samples the base
    /// accumulator directly and therefore records one full-frame pass and one
    /// output allocation.
    #[allow(clippy::too_many_arguments)]
    pub fn record_adjustment_blend_pass(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        ids: &mut GpuColorFrameIdAllocator,
        table: &mut GpuColorFrameResourceTable<GpuColorFrameWgpuResource>,
        resource_pool: Option<&GpuColorFrameWgpuResourcePool>,
        base: &GpuColorFrameHandle,
        processed: &GpuColorFrameHandle,
        opacity: f32,
        blend_mode: BlendMode,
        frame_seed: i64,
        working_color_space: mondrian_core::WorkingColorSpace,
    ) -> Result<GpuCompositeRecord, GpuCompositeError> {
        let validation_layers = [
            GpuCompositeLayer {
                source: GpuCompositeLayerSource::GpuFrame(base),
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_plan: None,
                frame_seed: 0,
            },
            GpuCompositeLayer {
                source: GpuCompositeLayerSource::GpuFrame(processed),
                opacity,
                blend_mode,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_plan: None,
                frame_seed: 0,
            },
        ];
        let descriptor = base.descriptor();
        validate_request(&GpuCompositeRequest {
            width: descriptor.width,
            height: descriptor.height,
            working_color_space,
            layers: &validation_layers,
        })?;
        let base_resource = table.get(base).map_err(GpuCompositeError::ResourceTable)?;
        let processed_resource = table.get(processed).map_err(GpuCompositeError::ResourceTable)?;
        let output_resource = create_working_resource(
            device,
            ids,
            descriptor,
            "gpu-adjustment-blend-output",
            resource_pool,
        )?;
        self.record_layer_pass(
            device,
            queue,
            encoder,
            GpuCompositeTextureBinding::Resource(base_resource.resource()),
            &output_resource.resource().texture_view,
            GpuCompositeTextureBinding::Resource(processed_resource.resource()),
            GpuCompositeUniforms {
                opacity: opacity.clamp(0.0, 1.0),
                source_kind: 0,
                effect_count: 0,
                blend_mode: gpu_blend_mode_id(blend_mode),
                frame_seed_lo: frame_seed as u32,
                frame_seed_hi: (frame_seed >> 32) as u32,
                mask_controls: [0; 2],
                solid_color: [0.0; 4],
                inv_transform0: [1.0, 0.0, 0.0, 0.0],
                inv_transform1: [1.0, 0.0, 0.0, 0.0],
                geometry: [
                    descriptor.width as f32,
                    descriptor.height as f32,
                    descriptor.width as f32,
                    descriptor.height as f32,
                ],
                effects: [GpuEffectUniform::zeroed(); MAX_FUSED_GPU_EFFECT_OPS],
            },
            None,
        )?;
        let output = output_resource.handle().clone();
        table.insert(output_resource).map_err(GpuCompositeError::ResourceTable)?;
        Ok(GpuCompositeRecord {
            output,
            diagnostics: GpuCompositingDiagnostics {
                gpu_native_composites: 1,
                gpu_composited_pixels: u64::from(descriptor.width)
                    .saturating_mul(u64::from(descriptor.height)),
                ..GpuCompositingDiagnostics::default()
            },
        })
    }

    /// Apply one AlphaMask-domain texture to a scene-linear working frame.
    ///
    /// RGB samples pass through unchanged by the Mask algebra; only straight
    /// coverage alpha is recomputed with the canonical MaskOp formula.
    #[allow(clippy::too_many_arguments)]
    pub fn record_alpha_mask_pass(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        ids: &mut GpuColorFrameIdAllocator,
        table: &mut GpuColorFrameResourceTable<GpuColorFrameWgpuResource>,
        resource_pool: Option<&GpuColorFrameWgpuResourcePool>,
        input: &GpuColorFrameHandle,
        mask: &GpuColorFrameHandle,
        invert: bool,
        mask_op: MaskOp,
        working_color_space: mondrian_core::WorkingColorSpace,
    ) -> Result<GpuCompositeRecord, GpuCompositeError> {
        validate_alpha_mask_inputs(input, mask, working_color_space)?;
        let descriptor = input.descriptor();
        let input_resource = table.get(input).map_err(GpuCompositeError::ResourceTable)?;
        let mask_resource = table.get(mask).map_err(GpuCompositeError::ResourceTable)?;
        let output_resource = create_working_resource(
            device,
            ids,
            descriptor,
            "gpu-alpha-mask-output",
            resource_pool,
        )?;
        self.record_layer_pass(
            device,
            queue,
            encoder,
            GpuCompositeTextureBinding::Resource(input_resource.resource()),
            &output_resource.resource().texture_view,
            GpuCompositeTextureBinding::Resource(mask_resource.resource()),
            GpuCompositeUniforms {
                opacity: 1.0,
                source_kind: 6,
                effect_count: 0,
                blend_mode: gpu_blend_mode_id(BlendMode::Normal),
                frame_seed_lo: 0,
                frame_seed_hi: 0,
                mask_controls: [gpu_mask_op_id(mask_op), u32::from(invert)],
                solid_color: [0.0; 4],
                inv_transform0: [1.0, 0.0, 0.0, 0.0],
                inv_transform1: [1.0, 0.0, 0.0, 0.0],
                geometry: [
                    descriptor.width as f32,
                    descriptor.height as f32,
                    descriptor.width as f32,
                    descriptor.height as f32,
                ],
                effects: [GpuEffectUniform::zeroed(); MAX_FUSED_GPU_EFFECT_OPS],
            },
            None,
        )?;
        let output = output_resource.handle().clone();
        table.insert(output_resource).map_err(GpuCompositeError::ResourceTable)?;
        Ok(GpuCompositeRecord {
            output,
            diagnostics: GpuCompositingDiagnostics {
                gpu_native_composites: 1,
                gpu_composited_pixels: u64::from(descriptor.width)
                    .saturating_mul(u64::from(descriptor.height)),
                ..GpuCompositingDiagnostics::default()
            },
        })
    }

    /// Combine two AlphaMask-domain textures without crossing into picture RGB.
    #[allow(clippy::too_many_arguments)]
    pub fn record_alpha_mask_combine_pass(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        ids: &mut GpuColorFrameIdAllocator,
        table: &mut GpuColorFrameResourceTable<GpuColorFrameWgpuResource>,
        resource_pool: Option<&GpuColorFrameWgpuResourcePool>,
        left: &GpuColorFrameHandle,
        right: &GpuColorFrameHandle,
        mask_op: MaskOp,
    ) -> Result<GpuCompositeRecord, GpuCompositeError> {
        validate_alpha_mask_pair(left, right)?;
        let descriptor = left.descriptor();
        let left_resource = table.get(left).map_err(GpuCompositeError::ResourceTable)?;
        let right_resource = table.get(right).map_err(GpuCompositeError::ResourceTable)?;
        let output_resource = create_working_resource(
            device,
            ids,
            descriptor,
            "gpu-alpha-mask-combine-output",
            resource_pool,
        )?;
        self.record_layer_pass(
            device,
            queue,
            encoder,
            GpuCompositeTextureBinding::Resource(left_resource.resource()),
            &output_resource.resource().texture_view,
            GpuCompositeTextureBinding::Resource(right_resource.resource()),
            GpuCompositeUniforms {
                opacity: 1.0,
                source_kind: 10,
                effect_count: 0,
                blend_mode: gpu_blend_mode_id(BlendMode::Normal),
                frame_seed_lo: 0,
                frame_seed_hi: 0,
                mask_controls: [gpu_mask_op_id(mask_op), 0],
                solid_color: [0.0; 4],
                inv_transform0: [1.0, 0.0, 0.0, 0.0],
                inv_transform1: [1.0, 0.0, 0.0, 0.0],
                geometry: [
                    descriptor.width as f32,
                    descriptor.height as f32,
                    descriptor.width as f32,
                    descriptor.height as f32,
                ],
                effects: [GpuEffectUniform::zeroed(); MAX_FUSED_GPU_EFFECT_OPS],
            },
            None,
        )?;
        let output = output_resource.handle().clone();
        table.insert(output_resource).map_err(GpuCompositeError::ResourceTable)?;
        Ok(GpuCompositeRecord {
            output,
            diagnostics: GpuCompositingDiagnostics {
                gpu_native_composites: 1,
                gpu_composited_pixels: u64::from(descriptor.width)
                    .saturating_mul(u64::from(descriptor.height)),
                ..GpuCompositingDiagnostics::default()
            },
        })
    }

    /// Mix working-RGB base/graded values through one AlphaMask while retaining
    /// base coverage alpha exactly.
    #[allow(clippy::too_many_arguments)]
    pub fn record_matte_mix_pass(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        ids: &mut GpuColorFrameIdAllocator,
        table: &mut GpuColorFrameResourceTable<GpuColorFrameWgpuResource>,
        resource_pool: Option<&GpuColorFrameWgpuResourcePool>,
        base: &GpuColorFrameHandle,
        graded: &GpuColorFrameHandle,
        matte: &GpuColorFrameHandle,
        working_color_space: mondrian_core::WorkingColorSpace,
    ) -> Result<GpuCompositeRecord, GpuCompositeError> {
        validate_matte_mix_inputs(base, graded, matte, working_color_space)?;
        let descriptor = base.descriptor();
        let base_resource = table.get(base).map_err(GpuCompositeError::ResourceTable)?;
        let graded_resource = table.get(graded).map_err(GpuCompositeError::ResourceTable)?;
        let matte_resource = table.get(matte).map_err(GpuCompositeError::ResourceTable)?;
        let output_resource = create_working_resource(
            device,
            ids,
            descriptor,
            "gpu-matte-mix-output",
            resource_pool,
        )?;
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mondrian_gpu_matte_mix_binding"),
            layout: &self.matte_mix_texture_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(
                        &base_resource.resource().texture_view,
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(
                        &graded_resource.resource().texture_view,
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(
                        &matte_resource.resource().texture_view,
                    ),
                },
            ],
        });
        self.texture_bind_group_creations.fetch_add(1, Ordering::Relaxed);
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("mondrian_gpu_matte_mix_pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &output_resource.resource().texture_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.matte_mix_pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..4, 0..1);
        drop(pass);
        let output = output_resource.handle().clone();
        table.insert(output_resource).map_err(GpuCompositeError::ResourceTable)?;
        Ok(GpuCompositeRecord {
            output,
            diagnostics: GpuCompositingDiagnostics {
                gpu_native_composites: 1,
                gpu_composited_pixels: u64::from(descriptor.width)
                    .saturating_mul(u64::from(descriptor.height)),
                ..GpuCompositingDiagnostics::default()
            },
        })
    }

    /// Generate one refined AlphaMask from a scene-linear working frame.
    ///
    /// Denoise and Gaussian feather are exact separable passes. Every private
    /// target is returned to the caller so its physical residency remains
    /// charged and alive until the owning command buffer is submitted.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_qualifier_pass(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        ids: &mut GpuColorFrameIdAllocator,
        table: &mut GpuColorFrameResourceTable<GpuColorFrameWgpuResource>,
        resource_pool: Option<&GpuColorFrameWgpuResourcePool>,
        input: &GpuColorFrameHandle,
        qualifier: &PreparedQualifier,
        working_color_space: mondrian_core::WorkingColorSpace,
    ) -> Result<GpuQualifierRecord, GpuCompositeError> {
        validate_qualifier_input(input, working_color_space)?;
        let input_resource = table.get(input).map_err(GpuCompositeError::ResourceTable)?;
        let input_descriptor = input.descriptor();
        let output_descriptor = ColorFrameDescriptor {
            color_space: crate::ColorFrameSpace::NonColorData,
            domain: ColorFrameDomain::AlphaMask,
            encoding: ColorFrameEncoding::LinearFloat,
            residency: ColorFrameResidency::Gpu,
            alpha: crate::ColorFrameAlpha::StraightCoverage,
            ..input_descriptor
        };
        let mut stages = Vec::with_capacity(4);
        if qualifier.denoise_radius() > 0 {
            stages.push((1_u32, false));
            stages.push((1_u32, true));
        }
        if qualifier.blur_radius() > f32::EPSILON {
            stages.push((2_u32, false));
            stages.push((2_u32, true));
        }
        if stages.is_empty() {
            stages.push((0_u32, false));
        }

        let mut current = None::<GpuColorFrameResource<GpuColorFrameWgpuResource>>;
        let mut scratch = Vec::with_capacity(stages.len().saturating_sub(1));
        for (index, (filter_kind, vertical)) in stages.iter().copied().enumerate() {
            let target = create_working_resource(
                device,
                ids,
                output_descriptor,
                "gpu-qualifier-pass",
                resource_pool,
            )?;
            let source = current
                .as_ref()
                .map_or(input_resource.resource(), |resource| resource.resource());
            let final_stage = index + 1 == stages.len();
            self.record_layer_pass(
                device,
                queue,
                encoder,
                GpuCompositeTextureBinding::Resource(source),
                &target.resource().texture_view,
                GpuCompositeTextureBinding::ProceduralDummy,
                qualifier_uniforms(
                    qualifier,
                    input_descriptor.width,
                    input_descriptor.height,
                    index == 0,
                    filter_kind,
                    vertical,
                    final_stage,
                ),
                None,
            )?;
            if let Some(previous) = current.replace(target) {
                scratch.push(previous);
            }
        }
        let Some(output_resource) = current else {
            return Err(GpuCompositeError::QualifierPassPlanEmpty);
        };
        let output = output_resource.handle().clone();
        table.insert(output_resource).map_err(GpuCompositeError::ResourceTable)?;
        Ok(GpuQualifierRecord { output, scratch })
    }

    /// Observe one AlphaMask as opaque black/white scene-linear RGB.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_matte_preview_pass(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        ids: &mut GpuColorFrameIdAllocator,
        table: &mut GpuColorFrameResourceTable<GpuColorFrameWgpuResource>,
        resource_pool: Option<&GpuColorFrameWgpuResourcePool>,
        input: &GpuColorFrameHandle,
        invert: bool,
        working_color_space: mondrian_core::WorkingColorSpace,
    ) -> Result<GpuPointEffectRecord, GpuCompositeError> {
        validate_matte_preview_input(input)?;
        let input_resource = table.get(input).map_err(GpuCompositeError::ResourceTable)?;
        let input_descriptor = input.descriptor();
        let output_descriptor = ColorFrameDescriptor {
            color_space: working_color_space.into(),
            domain: ColorFrameDomain::Working,
            encoding: ColorFrameEncoding::LinearFloat,
            residency: ColorFrameResidency::Gpu,
            alpha: crate::ColorFrameAlpha::StraightCoverage,
            ..input_descriptor
        };
        let output_resource = create_working_resource(
            device,
            ids,
            output_descriptor,
            "gpu-matte-preview-output",
            resource_pool,
        )?;
        self.record_layer_pass(
            device,
            queue,
            encoder,
            GpuCompositeTextureBinding::Resource(input_resource.resource()),
            &output_resource.resource().texture_view,
            GpuCompositeTextureBinding::ProceduralDummy,
            GpuCompositeUniforms {
                opacity: 1.0,
                source_kind: 9,
                effect_count: 0,
                blend_mode: 0,
                frame_seed_lo: 0,
                frame_seed_hi: 0,
                mask_controls: [0, u32::from(invert)],
                solid_color: [0.0; 4],
                inv_transform0: [1.0, 0.0, 0.0, 0.0],
                inv_transform1: [1.0, 0.0, 0.0, 0.0],
                geometry: [
                    input_descriptor.width as f32,
                    input_descriptor.height as f32,
                    input_descriptor.width as f32,
                    input_descriptor.height as f32,
                ],
                effects: [GpuEffectUniform::zeroed(); MAX_FUSED_GPU_EFFECT_OPS],
            },
            None,
        )?;
        let output = output_resource.handle().clone();
        table.insert(output_resource).map_err(GpuCompositeError::ResourceTable)?;
        Ok(GpuPointEffectRecord {
            output,
            processed_pixels: u64::from(input_descriptor.width)
                .saturating_mul(u64::from(input_descriptor.height)),
        })
    }

    /// Materialize an unblended procedural solid as a working-linear GPU frame.
    ///
    /// Layer opacity, affine transformation, blending, and effects are
    /// intentionally deferred to later graph nodes so their authored order is
    /// preserved when an OCIO effect-domain round trip is inserted.
    #[allow(clippy::too_many_arguments)]
    pub fn record_solid_source_pass(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        ids: &mut GpuColorFrameIdAllocator,
        table: &mut GpuColorFrameResourceTable<GpuColorFrameWgpuResource>,
        resource_pool: Option<&GpuColorFrameWgpuResourcePool>,
        width: u32,
        height: u32,
        working_color_space: mondrian_core::WorkingColorSpace,
        color: Color,
    ) -> Result<GpuSolidSourceRecord, GpuCompositeError> {
        if width == 0 || height == 0 {
            return Err(GpuCompositeError::EmptyExtent { width, height });
        }
        let output_resource = create_working_resource(
            device,
            ids,
            ColorFrameDescriptor {
                width,
                height,
                color_space: working_color_space.into(),
                domain: ColorFrameDomain::Working,
                encoding: ColorFrameEncoding::LinearFloat,
                residency: ColorFrameResidency::Gpu,
                alpha: crate::ColorFrameAlpha::StraightCoverage,
            },
            "gpu-solid-source-output",
            resource_pool,
        )?;
        self.record_layer_pass(
            device,
            queue,
            encoder,
            GpuCompositeTextureBinding::ProceduralDummy,
            &output_resource.resource().texture_view,
            GpuCompositeTextureBinding::ProceduralDummy,
            GpuCompositeUniforms {
                opacity: 1.0,
                source_kind: 4,
                effect_count: 0,
                blend_mode: gpu_blend_mode_id(BlendMode::Normal),
                frame_seed_lo: 0,
                frame_seed_hi: 0,
                mask_controls: [0; 2],
                solid_color: [color.r, color.g, color.b, color.a],
                inv_transform0: [1.0, 0.0, 0.0, 0.0],
                inv_transform1: [1.0, 0.0, 0.0, 0.0],
                geometry: [width as f32, height as f32, width as f32, height as f32],
                effects: [GpuEffectUniform::zeroed(); MAX_FUSED_GPU_EFFECT_OPS],
            },
            None,
        )?;
        let output = output_resource.handle().clone();
        table.insert(output_resource).map_err(GpuCompositeError::ResourceTable)?;
        Ok(GpuSolidSourceRecord {
            output,
            materialized_pixels: u64::from(width).saturating_mul(u64::from(height)),
        })
    }

    fn record_layer_pass(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        accum_binding: GpuCompositeTextureBinding<'_>,
        dst_view: &wgpu::TextureView,
        layer_binding: GpuCompositeTextureBinding<'_>,
        mut uniforms: GpuCompositeUniforms,
        effect_plan: Option<&CompiledEffectGpuPlan>,
    ) -> Result<(), GpuCompositeError> {
        let creative_lut_binding = self.creative_luts.prepare_plan(device, queue, effect_plan)?;
        uniforms.effect_count = effect_plan.map_or(0, |plan| plan.operations().len() as u32);
        uniforms.effects = effect_uniforms(effect_plan, Some(&creative_lut_binding))?;
        let (uniform_buffer, uniform_bind_group, uniform_offset) = {
            let mut arena = self.uniform_arena.lock();
            let slot = arena.next_slot;
            let page_index = slot / GPU_COMPOSITOR_UNIFORM_PAGE_SLOTS;
            let page_slot = slot % GPU_COMPOSITOR_UNIFORM_PAGE_SLOTS;
            if page_index == arena.pages.len() {
                arena.pages.push(create_uniform_page(
                    device,
                    &self.uniform_layout,
                    self.uniform_size,
                    self.uniform_stride,
                ));
                arena.diagnostics.buffer_creations =
                    arena.diagnostics.buffer_creations.saturating_add(1);
            }
            arena.next_slot = arena.next_slot.saturating_add(1);
            arena.diagnostics.uniform_writes = arena.diagnostics.uniform_writes.saturating_add(1);
            arena.diagnostics.high_watermark_slots = arena
                .diagnostics
                .high_watermark_slots
                .max(u32::try_from(arena.next_slot).unwrap_or(u32::MAX));
            arena.diagnostics.high_watermark_pages = arena
                .diagnostics
                .high_watermark_pages
                .max(u32::try_from(page_index.saturating_add(1)).unwrap_or(u32::MAX));
            let page = &arena.pages[page_index];
            (
                page.buffer.clone(),
                page.bind_group.clone(),
                u64::try_from(page_slot).unwrap_or(u64::MAX).saturating_mul(self.uniform_stride),
            )
        };
        queue.write_buffer(
            &uniform_buffer,
            uniform_offset,
            bytemuck::bytes_of(&uniforms),
        );
        let layer_bind_group = match layer_binding {
            GpuCompositeTextureBinding::Resource(resource) => {
                let (bind_group, cache_hit) =
                    resource.cached_bind_group(self.layer_texture_cache_key, |texture_view| {
                        device.create_bind_group(&wgpu::BindGroupDescriptor {
                            label: Some("mondrian_gpu_working_compositor_layer_binding"),
                            layout: &self.layer_texture_layout,
                            entries: &[
                                wgpu::BindGroupEntry {
                                    binding: 0,
                                    resource: wgpu::BindingResource::TextureView(texture_view),
                                },
                                wgpu::BindGroupEntry {
                                    binding: 1,
                                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                                },
                            ],
                        })
                    });
                self.record_texture_binding_cache_result(cache_hit);
                bind_group
            }
            GpuCompositeTextureBinding::ProceduralDummy => {
                self.texture_bind_group_cache_hits.fetch_add(1, Ordering::Relaxed);
                self.procedural_layer_bind_group.clone()
            }
        };
        let accum_bind_group = match accum_binding {
            GpuCompositeTextureBinding::Resource(resource) => {
                let (bind_group, cache_hit) =
                    resource.cached_bind_group(self.accum_texture_cache_key, |texture_view| {
                        device.create_bind_group(&wgpu::BindGroupDescriptor {
                            label: Some("mondrian_gpu_working_compositor_accum_binding"),
                            layout: &self.accum_texture_layout,
                            entries: &[wgpu::BindGroupEntry {
                                binding: 0,
                                resource: wgpu::BindingResource::TextureView(texture_view),
                            }],
                        })
                    });
                self.record_texture_binding_cache_result(cache_hit);
                bind_group
            }
            GpuCompositeTextureBinding::ProceduralDummy => {
                self.texture_bind_group_cache_hits.fetch_add(1, Ordering::Relaxed);
                self.procedural_accum_bind_group.clone()
            }
        };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("mondrian_gpu_working_compositor_pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: dst_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &layer_bind_group, &[]);
        pass.set_bind_group(1, &accum_bind_group, &[]);
        pass.set_bind_group(2, &uniform_bind_group, &[uniform_offset as u32]);
        pass.set_bind_group(3, creative_lut_binding.bind_group(), &[]);
        pass.draw(0..4, 0..1);
        Ok(())
    }

    fn record_texture_binding_cache_result(&self, cache_hit: bool) {
        if cache_hit {
            self.texture_bind_group_cache_hits.fetch_add(1, Ordering::Relaxed);
        } else {
            self.texture_bind_group_creations.fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn validate_point_effect_input(
    input: &GpuColorFrameHandle,
    plan: &CompiledEffectGpuPlan,
) -> Result<(), GpuCompositeError> {
    let actual = input.descriptor();
    require_straight_compatible_alpha(actual.alpha)?;
    let (color_space, domain, encoding) = match plan.processing_domain() {
        EffectColorDomain::SceneLinearRgb => {
            let Some(working_color_space) = actual.color_space.working() else {
                return Err(GpuCompositeError::PointEffectDescriptorMismatch {
                    expected: ColorFrameDescriptor {
                        color_space: mondrian_core::WorkingColorSpace::LinearRec709.into(),
                        domain: ColorFrameDomain::Working,
                        encoding: ColorFrameEncoding::LinearFloat,
                        ..actual
                    },
                    actual,
                });
            };
            (
                working_color_space.into(),
                ColorFrameDomain::Working,
                ColorFrameEncoding::LinearFloat,
            )
        }
        EffectColorDomain::LogPerceptualRgb { color_space }
        | EffectColorDomain::DisplayEncodedRgb { color_space } => (
            color_space.into(),
            ColorFrameDomain::Effect,
            ColorFrameEncoding::EncodedFloat,
        ),
        EffectColorDomain::DisplayLinearRgb { color_space } => (
            color_space.into(),
            ColorFrameDomain::Effect,
            ColorFrameEncoding::LinearFloat,
        ),
        domain @ (EffectColorDomain::Data | EffectColorDomain::AlphaMask) => {
            return Err(GpuCompositeError::PointEffectRequiresRgbIntermediate { domain });
        }
    };
    let expected = ColorFrameDescriptor {
        width: actual.width,
        height: actual.height,
        color_space,
        domain,
        encoding,
        residency: ColorFrameResidency::Gpu,
        alpha: actual.alpha,
    };
    if actual != expected {
        return Err(GpuCompositeError::PointEffectDescriptorMismatch { expected, actual });
    }
    if input.texture_format() == GpuColorFrameTextureFormat::Rgba8Unorm {
        return Err(GpuCompositeError::PointEffectTextureFormatUnsupported {
            texture_format: input.texture_format(),
        });
    }
    Ok(())
}

fn validate_alpha_mask_inputs(
    input: &GpuColorFrameHandle,
    mask: &GpuColorFrameHandle,
    working_color_space: mondrian_core::WorkingColorSpace,
) -> Result<(), GpuCompositeError> {
    let input_actual = input.descriptor();
    let input_expected = ColorFrameDescriptor {
        width: input_actual.width,
        height: input_actual.height,
        color_space: working_color_space.into(),
        domain: ColorFrameDomain::Working,
        encoding: ColorFrameEncoding::LinearFloat,
        residency: ColorFrameResidency::Gpu,
        alpha: crate::ColorFrameAlpha::StraightCoverage,
    };
    require_straight_compatible_alpha(input_actual.alpha)?;
    if input_actual != input_expected {
        return Err(GpuCompositeError::SourceDescriptorMismatch {
            expected: input_expected,
            actual: input_actual,
        });
    }
    let mask_actual = mask.descriptor();
    let mask_expected = ColorFrameDescriptor {
        width: input_actual.width,
        height: input_actual.height,
        color_space: crate::ColorFrameSpace::NonColorData,
        domain: ColorFrameDomain::AlphaMask,
        encoding: ColorFrameEncoding::LinearFloat,
        residency: ColorFrameResidency::Gpu,
        alpha: crate::ColorFrameAlpha::StraightCoverage,
    };
    require_straight_compatible_alpha(mask_actual.alpha)?;
    if mask_actual != mask_expected {
        return Err(GpuCompositeError::SourceDescriptorMismatch {
            expected: mask_expected,
            actual: mask_actual,
        });
    }
    for texture_format in [input.texture_format(), mask.texture_format()] {
        if texture_format != GpuColorFrameTextureFormat::Rgba32Float {
            return Err(GpuCompositeError::AlphaMaskTextureFormatUnsupported { texture_format });
        }
    }
    Ok(())
}

fn validate_alpha_mask_pair(
    left: &GpuColorFrameHandle,
    right: &GpuColorFrameHandle,
) -> Result<(), GpuCompositeError> {
    let left_actual = left.descriptor();
    let expected = ColorFrameDescriptor {
        width: left_actual.width,
        height: left_actual.height,
        color_space: crate::ColorFrameSpace::NonColorData,
        domain: ColorFrameDomain::AlphaMask,
        encoding: ColorFrameEncoding::LinearFloat,
        residency: ColorFrameResidency::Gpu,
        alpha: crate::ColorFrameAlpha::StraightCoverage,
    };
    for actual in [left_actual, right.descriptor()] {
        require_straight_compatible_alpha(actual.alpha)?;
        if actual != expected {
            return Err(GpuCompositeError::SourceDescriptorMismatch { expected, actual });
        }
    }
    for texture_format in [left.texture_format(), right.texture_format()] {
        if texture_format != GpuColorFrameTextureFormat::Rgba32Float {
            return Err(GpuCompositeError::AlphaMaskTextureFormatUnsupported { texture_format });
        }
    }
    Ok(())
}

fn validate_matte_mix_inputs(
    base: &GpuColorFrameHandle,
    graded: &GpuColorFrameHandle,
    matte: &GpuColorFrameHandle,
    working_color_space: mondrian_core::WorkingColorSpace,
) -> Result<(), GpuCompositeError> {
    let base_actual = base.descriptor();
    let working_expected = ColorFrameDescriptor {
        width: base_actual.width,
        height: base_actual.height,
        color_space: working_color_space.into(),
        domain: ColorFrameDomain::Working,
        encoding: ColorFrameEncoding::LinearFloat,
        residency: ColorFrameResidency::Gpu,
        alpha: crate::ColorFrameAlpha::StraightCoverage,
    };
    for actual in [base_actual, graded.descriptor()] {
        require_straight_compatible_alpha(actual.alpha)?;
        if actual != working_expected {
            return Err(GpuCompositeError::SourceDescriptorMismatch {
                expected: working_expected,
                actual,
            });
        }
    }
    let matte_expected = ColorFrameDescriptor {
        width: base_actual.width,
        height: base_actual.height,
        color_space: crate::ColorFrameSpace::NonColorData,
        domain: ColorFrameDomain::AlphaMask,
        encoding: ColorFrameEncoding::LinearFloat,
        residency: ColorFrameResidency::Gpu,
        alpha: crate::ColorFrameAlpha::StraightCoverage,
    };
    let matte_actual = matte.descriptor();
    require_straight_compatible_alpha(matte_actual.alpha)?;
    if matte_actual != matte_expected {
        return Err(GpuCompositeError::SourceDescriptorMismatch {
            expected: matte_expected,
            actual: matte_actual,
        });
    }
    for texture_format in [
        base.texture_format(),
        graded.texture_format(),
        matte.texture_format(),
    ] {
        if texture_format != GpuColorFrameTextureFormat::Rgba32Float {
            return Err(GpuCompositeError::AlphaMaskTextureFormatUnsupported { texture_format });
        }
    }
    Ok(())
}

fn validate_qualifier_input(
    input: &GpuColorFrameHandle,
    working_color_space: mondrian_core::WorkingColorSpace,
) -> Result<(), GpuCompositeError> {
    let actual = input.descriptor();
    let expected = ColorFrameDescriptor {
        width: actual.width,
        height: actual.height,
        color_space: working_color_space.into(),
        domain: ColorFrameDomain::Working,
        encoding: ColorFrameEncoding::LinearFloat,
        residency: ColorFrameResidency::Gpu,
        alpha: crate::ColorFrameAlpha::StraightCoverage,
    };
    require_straight_compatible_alpha(actual.alpha)?;
    if actual != expected {
        return Err(GpuCompositeError::SourceDescriptorMismatch { expected, actual });
    }
    if input.texture_format() != GpuColorFrameTextureFormat::Rgba32Float {
        return Err(GpuCompositeError::AlphaMaskTextureFormatUnsupported {
            texture_format: input.texture_format(),
        });
    }
    Ok(())
}

fn validate_matte_preview_input(input: &GpuColorFrameHandle) -> Result<(), GpuCompositeError> {
    let actual = input.descriptor();
    let expected = ColorFrameDescriptor {
        width: actual.width,
        height: actual.height,
        color_space: crate::ColorFrameSpace::NonColorData,
        domain: ColorFrameDomain::AlphaMask,
        encoding: ColorFrameEncoding::LinearFloat,
        residency: ColorFrameResidency::Gpu,
        alpha: crate::ColorFrameAlpha::StraightCoverage,
    };
    require_straight_compatible_alpha(actual.alpha)?;
    if actual != expected {
        return Err(GpuCompositeError::SourceDescriptorMismatch { expected, actual });
    }
    if input.texture_format() != GpuColorFrameTextureFormat::Rgba32Float {
        return Err(GpuCompositeError::AlphaMaskTextureFormatUnsupported {
            texture_format: input.texture_format(),
        });
    }
    Ok(())
}

fn qualifier_uniforms(
    qualifier: &PreparedQualifier,
    width: u32,
    height: u32,
    raw_source: bool,
    filter_kind: u32,
    vertical: bool,
    final_stage: bool,
) -> GpuCompositeUniforms {
    let mut effects = [GpuEffectUniform::zeroed(); MAX_FUSED_GPU_EFFECT_OPS];
    let hue = qualifier.hue_controls();
    let saturation = qualifier.saturation_controls();
    let luminance = qualifier.luminance_controls();
    let three_d = qualifier.three_d_controls();
    let clean = qualifier.clean_controls();
    let sample_count = qualifier.samples().len();
    effects[0] = GpuEffectUniform {
        header: [
            match qualifier.mode() {
                QualifierMode::Hsl => 0,
                QualifierMode::ThreeDimensional => 1,
            },
            sample_count as u32,
            qualifier.denoise_radius(),
            0,
        ],
        params: [hue[0], hue[1], hue[2], 0.0],
        color: [saturation[0], saturation[1], saturation[2], 0.0],
        extra0: [luminance[0], luminance[1], luminance[2], 0.0],
        extra1: [three_d[0], three_d[1], clean[0], clean[1]],
    };
    let coefficients = qualifier.luminance_coefficients();
    effects[1].params = [
        coefficients[0],
        coefficients[1],
        coefficients[2],
        qualifier.blur_radius(),
    ];
    for (index, (coordinate, operation)) in qualifier.samples().enumerate() {
        let slot = 2 + index / 4;
        let lane = index % 4;
        let packed = [coordinate[0], coordinate[1], coordinate[2], 0.0];
        match lane {
            0 => effects[slot].params = packed,
            1 => effects[slot].color = packed,
            2 => effects[slot].extra0 = packed,
            _ => effects[slot].extra1 = packed,
        }
        if operation == QualifierSampleOperation::Exclude {
            effects[slot].header[0] |= 1_u32 << lane;
        }
    }
    GpuCompositeUniforms {
        opacity: 1.0,
        source_kind: if raw_source { 7 } else { 8 },
        effect_count: (2 + sample_count.div_ceil(4)) as u32,
        blend_mode: 0,
        frame_seed_lo: 0,
        frame_seed_hi: 0,
        mask_controls: [
            filter_kind,
            u32::from(vertical) | (u32::from(final_stage) << 1),
        ],
        solid_color: [0.0; 4],
        inv_transform0: [1.0, 0.0, 0.0, 0.0],
        inv_transform1: [1.0, 0.0, 0.0, 0.0],
        geometry: [width as f32, height as f32, width as f32, height as f32],
        effects,
    }
}

const fn gpu_mask_op_id(mask_op: MaskOp) -> u32 {
    match mask_op {
        MaskOp::Add => 0,
        MaskOp::Subtract => 1,
        MaskOp::Intersect => 2,
        MaskOp::Difference => 3,
    }
}

fn single_layer_gpu_passthrough<'a>(
    request: &'a GpuCompositeRequest<'a>,
) -> Option<&'a GpuColorFrameHandle> {
    let mut contributing_layers =
        request.layers.iter().filter(|layer| !layer_has_zero_contribution(layer));
    let layer = contributing_layers.next()?;
    if contributing_layers.next().is_some() {
        return None;
    }
    let GpuCompositeLayerSource::GpuFrame(handle) = layer.source else {
        return None;
    };
    let descriptor = handle.descriptor();
    let preserves_pixels = layer.opacity.clamp(0.0, 1.0) == 1.0
        && layer.blend_mode == BlendMode::Normal
        && is_identity_transform(layer.transform)
        && layer.effect_plan.is_none_or(CompiledEffectGpuPlan::is_identity)
        && descriptor.width == request.width
        && descriptor.height == request.height
        && handle.texture_format() == GpuColorFrameTextureFormat::Rgba32Float;
    preserves_pixels.then_some(handle)
}

fn gpu_blend_mode_id(mode: BlendMode) -> u32 {
    match mode {
        BlendMode::Normal => 0,
        BlendMode::Dissolve => 1,
        BlendMode::Multiply => 2,
        BlendMode::Screen => 3,
        BlendMode::Overlay => 4,
        BlendMode::Darken => 5,
        BlendMode::Lighten => 6,
        BlendMode::ColorDodge => 7,
        BlendMode::ColorBurn => 8,
        BlendMode::HardLight => 9,
        BlendMode::SoftLight => 10,
        BlendMode::Difference => 11,
        BlendMode::Exclusion => 12,
        BlendMode::Subtract => 13,
        BlendMode::DarkerColor => 14,
        BlendMode::LighterColor => 15,
        BlendMode::LinearBurn => 16,
        BlendMode::LinearDodge => 17,
        BlendMode::VividLight => 18,
        BlendMode::LinearLight => 19,
        BlendMode::PinLight => 20,
        BlendMode::HardMix => 21,
        BlendMode::Divide => 22,
        BlendMode::Hue => 23,
        BlendMode::Saturation => 24,
        BlendMode::Color => 25,
        BlendMode::Luminosity => 26,
    }
}

fn effect_uniforms(
    plan: Option<&CompiledEffectGpuPlan>,
    creative_luts: Option<&GpuCreativeLutPreparedBinding>,
) -> Result<[GpuEffectUniform; MAX_FUSED_GPU_EFFECT_OPS], GpuCompositeError> {
    let mut uniforms = [GpuEffectUniform::zeroed(); MAX_FUSED_GPU_EFFECT_OPS];
    let Some(plan) = plan else {
        return Ok(uniforms);
    };
    for (target, operation) in uniforms.iter_mut().zip(plan.operations()) {
        *target = match operation {
            EffectGpuPointOp::ColorAdjust {
                exposure,
                contrast,
                saturation,
                luminance_coefficients,
            } => GpuEffectUniform {
                header: [1, 0, 0, 0],
                params: [*exposure, *contrast, *saturation, 0.0],
                color: [
                    luminance_coefficients[0],
                    luminance_coefficients[1],
                    luminance_coefficients[2],
                    0.0,
                ],
                extra0: [0.0; 4],
                extra1: [0.0; 4],
            },
            EffectGpuPointOp::WhiteBalance { grade } => {
                let matrix = grade.matrix();
                GpuEffectUniform {
                    header: [6, 0, 0, 0],
                    params: [matrix[0][0], matrix[0][1], matrix[0][2], 0.0],
                    color: [matrix[1][0], matrix[1][1], matrix[1][2], 0.0],
                    extra0: [matrix[2][0], matrix[2][1], matrix[2][2], 0.0],
                    extra1: [0.0; 4],
                }
            }
            EffectGpuPointOp::Primaries { grade } => {
                let offset = grade.offset();
                let lift_delta = grade.lift_delta();
                let inverse_gamma = grade.inverse_gamma();
                let gain = grade.gain();
                GpuEffectUniform {
                    header: [7, 0, 0, 0],
                    params: [offset[0], offset[1], offset[2], 0.0],
                    color: [lift_delta[0], lift_delta[1], lift_delta[2], 0.0],
                    extra0: [inverse_gamma[0], inverse_gamma[1], inverse_gamma[2], 0.0],
                    extra1: [gain[0], gain[1], gain[2], 0.0],
                }
            }
            EffectGpuPointOp::HdrGrading { grade } => {
                let location = creative_luts
                    .and_then(|binding| binding.hdr_grading_location(grade))
                    .ok_or_else(|| {
                        GpuCompositeError::CreativeLut(
                            crate::GpuCreativeLutError::PreparedHdrGradingBindingMissing {
                                fingerprint: *grade.semantic_fingerprint(),
                            },
                        )
                    })?;
                let luminance = grade.luminance_coefficients();
                GpuEffectUniform {
                    header: [12, location.base_layer, location.sample_count, 0],
                    params: [luminance[0], luminance[1], luminance[2], 0.0],
                    color: [0.0; 4],
                    extra0: [0.0; 4],
                    extra1: [0.0; 4],
                }
            }
            EffectGpuPointOp::AscCdl { grade } => {
                let slope = grade.slope();
                let offset = grade.offset();
                let power = grade.power();
                GpuEffectUniform {
                    header: [8, 0, 0, 0],
                    params: [slope[0], slope[1], slope[2], 0.0],
                    color: [offset[0], offset[1], offset[2], 0.0],
                    extra0: [power[0], power[1], power[2], 0.0],
                    extra1: [grade.saturation(), 0.0, 0.0, 0.0],
                }
            }
            EffectGpuPointOp::GamutCompression { grade } => {
                let working_space = match grade.working_color_space() {
                    mondrian_core::WorkingColorSpace::LinearRec709 => 0,
                    mondrian_core::WorkingColorSpace::LinearRec2020 => 1,
                    mondrian_core::WorkingColorSpace::LinearP3D65 => 2,
                    mondrian_core::WorkingColorSpace::AcesCg => 3,
                };
                GpuEffectUniform {
                    header: [10, working_space, 0, 0],
                    params: [grade.amount(), 0.0, 0.0, 0.0],
                    color: [0.0; 4],
                    extra0: [0.0; 4],
                    extra1: [0.0; 4],
                }
            }
            EffectGpuPointOp::HighlightRecovery { grade } => {
                let luminance = grade.luminance_coefficients();
                GpuEffectUniform {
                    header: [11, 0, 0, 0],
                    params: [grade.threshold(), grade.rolloff(), grade.strength(), 0.0],
                    color: [luminance[0], luminance[1], luminance[2], 0.0],
                    extra0: [0.0; 4],
                    extra1: [0.0; 4],
                }
            }
            EffectGpuPointOp::ColorCurves { curves } => {
                let location = creative_luts
                    .and_then(|binding| binding.curve_location(curves))
                    .ok_or_else(|| {
                        GpuCompositeError::CreativeLut(
                            crate::GpuCreativeLutError::PreparedCurveBindingMissing {
                                fingerprint: *curves.semantic_fingerprint(),
                            },
                        )
                    })?;
                let luminance = curves.luminance_coefficients();
                GpuEffectUniform {
                    header: [
                        9,
                        location.base_layer,
                        location.sample_count,
                        u32::from(matches!(
                            curves.mode(),
                            mondrian_effects::ColorCurvesMode::YRgb
                        )),
                    ],
                    params: [
                        luminance[0],
                        luminance[1],
                        luminance[2],
                        f32::from(!curves.secondary_identity()),
                    ],
                    color: [0.0; 4],
                    extra0: [0.0; 4],
                    extra1: [0.0; 4],
                }
            }
            EffectGpuPointOp::Vignette { intensity, feather } => GpuEffectUniform {
                header: [3, 0, 0, 0],
                params: [*intensity, *feather, 0.0, 0.0],
                color: [0.0; 4],
                extra0: [0.0; 4],
                extra1: [0.0; 4],
            },
            EffectGpuPointOp::Grain { amount } => GpuEffectUniform {
                header: [4, 0, 0, 0],
                params: [*amount, 0.0, 0.0, 0.0],
                color: [0.0; 4],
                extra0: [0.0; 4],
                extra1: [0.0; 4],
            },
            EffectGpuPointOp::Crop { left, top, right, bottom } => GpuEffectUniform {
                header: [5, 0, 0, 0],
                params: [*left, *top, *right, *bottom],
                color: [0.0; 4],
                extra0: [0.0; 4],
                extra1: [0.0; 4],
            },
            EffectGpuPointOp::Lut3D { lut, intensity } => {
                let effective_intensity = intensity.clamp(0.0, 1.0);
                let location = if effective_intensity <= 1.0e-4 {
                    crate::creative_lut_gpu::GpuCreativeLutLocation { base_layer: 0, edge_size: 2 }
                } else {
                    creative_luts.and_then(|binding| binding.location(lut)).ok_or_else(|| {
                        GpuCompositeError::CreativeLut(
                            crate::GpuCreativeLutError::PreparedBindingMissing {
                                fingerprint: *lut.semantic_fingerprint(),
                            },
                        )
                    })?
                };
                GpuEffectUniform {
                    header: [2, location.base_layer, location.edge_size, 0],
                    params: [
                        *intensity,
                        lut.domain_min[0],
                        lut.domain_min[1],
                        lut.domain_min[2],
                    ],
                    color: [lut.domain_max[0], lut.domain_max[1], lut.domain_max[2], 0.0],
                    extra0: [0.0; 4],
                    extra1: [0.0; 4],
                }
            }
        };
    }
    Ok(uniforms)
}

fn validate_request(request: &GpuCompositeRequest<'_>) -> Result<(), GpuCompositeError> {
    if request.width == 0 || request.height == 0 {
        return Err(GpuCompositeError::EmptyExtent {
            width: request.width,
            height: request.height,
        });
    }
    let contributing_layers =
        request.layers.iter().filter(|layer| !layer_has_zero_contribution(layer));
    let capability = evaluate_gpu_compositing_capability(
        contributing_layers.clone().any(|layer| !gpu_transform_supported(layer)),
        contributing_layers.clone().all(|layer| {
            !matches!(
                layer.source,
                GpuCompositeLayerSource::CpuFrame(_) | GpuCompositeLayerSource::CpuDataTexture(_)
            )
        }),
    );
    if let GpuCompositingCapability::CpuFallback { reason } = capability {
        return Err(GpuCompositeError::Blocked { reason });
    }
    for layer in request.layers {
        if layer_has_zero_contribution(layer) {
            continue;
        }
        if let Some(plan) = layer.effect_plan {
            let domain = plan.processing_domain();
            if domain != EffectColorDomain::SceneLinearRgb {
                return Err(GpuCompositeError::EffectDomainRequiresExternalPass { domain });
            }
        }
        if matches!(layer.source, GpuCompositeLayerSource::Adjustment)
            && layer.effect_plan.is_none_or(CompiledEffectGpuPlan::is_identity)
        {
            return Err(GpuCompositeError::AdjustmentMissingEffectPlan);
        }
        if let Some(actual) = layer_source_descriptor(layer.source) {
            let expected_residency = match layer.source {
                GpuCompositeLayerSource::CpuFrame(_) => ColorFrameResidency::Cpu,
                GpuCompositeLayerSource::CpuDataTexture(_) => ColorFrameResidency::Gpu,
                GpuCompositeLayerSource::GpuFrame(_) => ColorFrameResidency::Gpu,
                GpuCompositeLayerSource::SolidColor(_) | GpuCompositeLayerSource::Adjustment => {
                    unreachable!("procedural layers have no descriptor")
                }
            };
            let expected = match layer.source {
                GpuCompositeLayerSource::CpuDataTexture(_) => ColorFrameDescriptor {
                    width: actual.width,
                    height: actual.height,
                    color_space: crate::ColorFrameSpace::NonColorData,
                    domain: ColorFrameDomain::DataTexture,
                    encoding: ColorFrameEncoding::LinearFloat,
                    residency: expected_residency,
                    alpha: actual.alpha,
                },
                _ => ColorFrameDescriptor {
                    width: actual.width,
                    height: actual.height,
                    color_space: request.working_color_space.into(),
                    domain: ColorFrameDomain::Working,
                    encoding: ColorFrameEncoding::LinearFloat,
                    residency: expected_residency,
                    alpha: actual.alpha,
                },
            };
            require_straight_compatible_alpha(actual.alpha)?;
            if actual != expected {
                return Err(GpuCompositeError::SourceDescriptorMismatch { expected, actual });
            }
        }
    }
    Ok(())
}

fn require_straight_compatible_alpha(
    alpha: crate::ColorFrameAlpha,
) -> Result<(), GpuCompositeError> {
    if alpha.is_straight_compatible() {
        Ok(())
    } else {
        Err(GpuCompositeError::InputNotStraightCompatibleAlpha { actual: alpha })
    }
}

fn layer_has_zero_contribution(layer: &GpuCompositeLayer<'_>) -> bool {
    if layer.opacity.clamp(0.0, 1.0) == 0.0 {
        return true;
    }

    match layer.source {
        GpuCompositeLayerSource::CpuFrame(_)
        | GpuCompositeLayerSource::CpuDataTexture(_)
        | GpuCompositeLayerSource::GpuFrame(_)
        | GpuCompositeLayerSource::SolidColor(_) => affine_has_zero_area(layer.transform),
        GpuCompositeLayerSource::Adjustment => false,
    }
}

fn affine_has_zero_area(transform: [f32; 6]) -> bool {
    let determinant = transform[0] * transform[4] - transform[3] * transform[1];
    transform.iter().all(|value| value.is_finite()) && determinant == 0.0
}

fn gpu_transform_supported(layer: &GpuCompositeLayer<'_>) -> bool {
    match layer.source {
        GpuCompositeLayerSource::CpuFrame(_)
        | GpuCompositeLayerSource::CpuDataTexture(_)
        | GpuCompositeLayerSource::GpuFrame(_)
        | GpuCompositeLayerSource::SolidColor(_) => invert_affine(layer.transform).is_some(),
        GpuCompositeLayerSource::Adjustment => is_identity_transform(layer.transform),
    }
}

fn layer_source_descriptor(source: GpuCompositeLayerSource<'_>) -> Option<ColorFrameDescriptor> {
    match source {
        GpuCompositeLayerSource::CpuFrame(frame) => Some(frame.descriptor()),
        GpuCompositeLayerSource::CpuDataTexture(frame) => {
            let descriptor = frame.descriptor();
            Some(ColorFrameDescriptor {
                width: descriptor.width,
                height: descriptor.height,
                color_space: crate::ColorFrameSpace::NonColorData,
                domain: ColorFrameDomain::DataTexture,
                encoding: ColorFrameEncoding::LinearFloat,
                residency: ColorFrameResidency::Gpu,
                alpha: descriptor.alpha,
            })
        }
        GpuCompositeLayerSource::GpuFrame(handle) => Some(handle.descriptor()),
        GpuCompositeLayerSource::SolidColor(_) | GpuCompositeLayerSource::Adjustment => None,
    }
}

fn is_identity_transform(transform: [f32; 6]) -> bool {
    const EPSILON: f32 = 1.0e-6;
    (transform[0] - 1.0).abs() <= EPSILON
        && transform[1].abs() <= EPSILON
        && transform[2].abs() <= EPSILON
        && transform[3].abs() <= EPSILON
        && (transform[4] - 1.0).abs() <= EPSILON
        && transform[5].abs() <= EPSILON
}

fn invert_affine(transform: [f32; 6]) -> Option<[f32; 6]> {
    let a = transform[0];
    let c = transform[1];
    let tx = transform[2];
    let b = transform[3];
    let d = transform[4];
    let ty = transform[5];
    let det = a * d - b * c;
    if det.abs() <= 1.0e-8 {
        return None;
    }
    let inv_det = 1.0 / det;
    let ia = d * inv_det;
    let ic = -c * inv_det;
    let ib = -b * inv_det;
    let id = a * inv_det;
    let itx = -(ia * tx + ic * ty);
    let ity = -(ib * tx + id * ty);
    Some([ia, ic, itx, ib, id, ity])
}

fn texture_binding(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn create_working_resource(
    device: &wgpu::Device,
    ids: &mut GpuColorFrameIdAllocator,
    descriptor: ColorFrameDescriptor,
    label: &'static str,
    resource_pool: Option<&GpuColorFrameWgpuResourcePool>,
) -> Result<GpuColorFrameResource<GpuColorFrameWgpuResource>, GpuCompositeError> {
    let handle = GpuColorFrameHandle::new(
        ids.allocate()?,
        descriptor,
        GpuColorFrameTextureFormat::Rgba32Float,
        label,
    )?;
    let allocation = GpuColorFrameAllocationPlan::for_handle(handle);
    Ok(resource_pool.map_or_else(
        || GpuColorFrameUploader::allocate(device, &allocation),
        |pool| pool.acquire(device, &allocation),
    ))
}

fn clear_working_texture(
    encoder: &mut wgpu::CommandEncoder,
    view: &wgpu::TextureView,
    color: wgpu::Color,
) {
    let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("mondrian_gpu_working_compositor_clear"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(color),
                store: wgpu::StoreOp::Store,
            },
            depth_slice: None,
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::{
        NormalizedCurve, NormalizedCurvePoint, WorkingColorSpace, WorkingRgbaF32Frame,
    };

    #[test]
    fn gpu_compositor_and_matte_mix_shaders_parse_as_wgsl() {
        naga::front::wgsl::parse_str(GPU_COMPOSITOR_SHADER)
            .expect("GPU compositor WGSL should parse");
        naga::front::wgsl::parse_str(GPU_MATTE_MIX_SHADER).expect("GPU MatteMix WGSL should parse");
    }

    #[test]
    fn gpu_compositing_capability_classifies_single_layer() {
        let cap = evaluate_gpu_compositing_capability(false, true);
        assert_eq!(cap, GpuCompositingCapability::GpuNative);
    }

    #[test]
    fn gpu_compositing_capability_classifies_upload_needed() {
        let cap = evaluate_gpu_compositing_capability(false, false);
        assert_eq!(cap, GpuCompositingCapability::GpuWithUpload);
    }

    #[test]
    fn gpu_compositing_capability_rejects_unsupported_transform() {
        let cap = evaluate_gpu_compositing_capability(true, true);
        assert!(matches!(
            cap,
            GpuCompositingCapability::CpuFallback {
                reason: GpuCompositingBlockerReason::UnsupportedTransform
            }
        ));
    }

    #[test]
    fn gpu_compositing_diagnostics_accumulate() {
        let mut a = GpuCompositingDiagnostics {
            gpu_passthrough_frames: 2,
            gpu_native_composites: 3,
            gpu_composited_pixels: 1000,
            ..GpuCompositingDiagnostics::default()
        };
        let b = GpuCompositingDiagnostics {
            gpu_passthrough_frames: 1,
            cpu_fallback_composites: 1,
            cpu_composited_pixels: 500,
            first_blocker: Some(GpuCompositingBlockerReason::EffectRequiresCpu),
            ..GpuCompositingDiagnostics::default()
        };
        a.accumulate(b);
        assert_eq!(a.gpu_passthrough_frames, 3);
        assert_eq!(a.gpu_native_composites, 3);
        assert_eq!(a.cpu_fallback_composites, 1);
        assert_eq!(a.gpu_composited_pixels, 1000);
        assert_eq!(a.cpu_composited_pixels, 500);
        assert_eq!(
            a.first_blocker,
            Some(GpuCompositingBlockerReason::EffectRequiresCpu)
        );
    }

    #[test]
    fn single_gpu_working_layer_passthrough_requires_pixel_identity() {
        let handle = GpuColorFrameHandle::new(
            crate::GpuColorFrameId::from_raw(99),
            ColorFrameDescriptor {
                width: 8,
                height: 8,
                color_space: WorkingColorSpace::LinearRec709.into(),
                domain: ColorFrameDomain::Working,
                encoding: ColorFrameEncoding::LinearFloat,
                residency: ColorFrameResidency::Gpu,
                alpha: crate::ColorFrameAlpha::StraightCoverage,
            },
            GpuColorFrameTextureFormat::Rgba32Float,
            "passthrough-test",
        )
        .expect("valid passthrough handle");
        let mut layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::GpuFrame(&handle),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: None,
            frame_seed: 0,
        };

        let layers = [layer];
        let request = GpuCompositeRequest {
            width: 8,
            height: 8,
            working_color_space: WorkingColorSpace::LinearRec709,
            layers: &layers,
        };
        assert_eq!(single_layer_gpu_passthrough(&request), Some(&handle));

        layer.opacity = 0.5;
        let layers = [layer];
        let request = GpuCompositeRequest { layers: &layers, ..request };
        assert_eq!(single_layer_gpu_passthrough(&request), None);

        layer.opacity = 1.0;
        layer.transform[2] = 1.0;
        let layers = [layer];
        let request = GpuCompositeRequest { layers: &layers, ..request };
        assert_eq!(single_layer_gpu_passthrough(&request), None);
    }

    #[test]
    fn gpu_compositing_blocker_codes_are_stable() {
        let reasons = [
            GpuCompositingBlockerReason::EffectRequiresCpu,
            GpuCompositingBlockerReason::UnsupportedTransform,
            GpuCompositingBlockerReason::FrameNotGpuResident,
            GpuCompositingBlockerReason::GpuUnavailable,
        ];
        let mut codes: Vec<_> = reasons.iter().map(|r| r.code()).collect();
        codes.sort();
        codes.dedup();
        assert_eq!(codes.len(), reasons.len(), "blocker codes must be unique");
        for reason in &reasons {
            assert!(!reason.code().is_empty());
            assert!(!reason.description().is_empty());
        }
    }

    #[test]
    fn gpu_effect_uniforms_preserve_plan_order_and_parameters() {
        use mondrian_effects::{
            compile_reference_render_graph, lower_effect_graph_to_gpu_plan,
            EffectGraphBuilderState, EffectRenderOp,
        };

        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(EffectRenderOp::ColorAdjust {
            exposure: 0.25,
            contrast: 1.1,
            saturation: 0.8,
            working_color_space: WorkingColorSpace::LinearRec2020,
        });
        builder.append_unary(EffectRenderOp::Vignette { intensity: 0.7, feather: 0.4 });
        builder.append_unary(EffectRenderOp::Crop {
            left: 0.25,
            top: 0.0,
            right: 0.0,
            bottom: 0.25,
        });
        let graph = compile_reference_render_graph(builder.finish()).expect("valid graph");
        let plan = lower_effect_graph_to_gpu_plan(&graph).expect("supported point chain");

        let uniforms = effect_uniforms(Some(&plan), None).expect("uniforms without LUT resources");

        assert_eq!(uniforms[0].header[0], 1);
        assert_eq!(uniforms[0].params, [0.25, 1.1, 0.8, 0.0]);
        assert_eq!(uniforms[0].color, [0.2627, 0.6780, 0.0593, 0.0]);
        assert_eq!(uniforms[1].header[0], 3);
        assert_eq!(uniforms[1].params, [0.7, 0.4, 0.0, 0.0]);
        assert_eq!(uniforms[2].header[0], 5);
        assert_eq!(uniforms[2].params, [0.25, 0.0, 0.0, 0.25]);
        assert!(uniforms[3..].iter().all(|uniform| uniform.header[0] == 0));
    }

    #[test]
    fn gpu_effect_uniforms_preserve_complete_primary_grade_contracts() {
        use mondrian_effects::{
            compile_reference_render_graph, lower_effect_graph_to_gpu_plan, AscCdlGrade,
            EffectGraphBuilderState, EffectRenderOp, GamutCompressionGrade, HighlightRecoveryGrade,
            PrimariesGrade, WhiteBalanceGrade,
        };

        let white_balance = WhiteBalanceGrade::new(0.4, -0.2, WorkingColorSpace::LinearP3D65)
            .expect("white balance");
        let primaries = PrimariesGrade::new(
            [0.01, 0.02, 0.03],
            [1.1, 0.9, 1.0],
            [1.2, 0.8, 1.4],
            [0.7, 1.1, 0.95],
        )
        .expect("Primaries");
        let cdl = AscCdlGrade::new([1.1, 1.2, 1.3], [-0.1, 0.0, 0.1], [0.8, 1.0, 1.2], 0.75)
            .expect("ASC CDL");
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(EffectRenderOp::WhiteBalance { grade: white_balance });
        builder.append_unary(EffectRenderOp::Primaries { grade: primaries });
        builder.append_unary(EffectRenderOp::AscCdl { grade: cdl });
        let gamut = GamutCompressionGrade::new(0.6, WorkingColorSpace::LinearP3D65)
            .expect("gamut compression");
        let highlight = HighlightRecoveryGrade::new(1.0, 0.75, 0.8, WorkingColorSpace::LinearP3D65)
            .expect("highlight recovery");
        builder.append_unary(EffectRenderOp::GamutCompression { grade: gamut });
        builder.append_unary(EffectRenderOp::HighlightRecovery { grade: highlight });
        let graph = compile_reference_render_graph(builder.finish()).expect("grade graph");
        let plan = lower_effect_graph_to_gpu_plan(&graph).expect("GPU grade plan");

        let uniforms = effect_uniforms(Some(&plan), None).expect("primary uniforms");
        let matrix = white_balance.matrix();
        assert_eq!(uniforms[0].header[0], 6);
        assert_eq!(uniforms[0].params[..3], matrix[0]);
        assert_eq!(uniforms[0].color[..3], matrix[1]);
        assert_eq!(uniforms[0].extra0[..3], matrix[2]);
        assert_eq!(uniforms[1].header[0], 7);
        assert_eq!(uniforms[1].params[..3], primaries.offset());
        assert_eq!(uniforms[1].color[..3], primaries.lift_delta());
        assert_eq!(uniforms[1].extra0[..3], primaries.inverse_gamma());
        assert_eq!(uniforms[1].extra1[..3], primaries.gain());
        assert_eq!(uniforms[2].header[0], 8);
        assert_eq!(uniforms[2].params[..3], cdl.slope());
        assert_eq!(uniforms[2].color[..3], cdl.offset());
        assert_eq!(uniforms[2].extra0[..3], cdl.power());
        assert_eq!(uniforms[2].extra1[0], cdl.saturation());
        assert_eq!(uniforms[3].header, [10, 2, 0, 0]);
        assert_eq!(uniforms[3].params[0], gamut.amount());
        assert_eq!(uniforms[4].header[0], 11);
        assert_eq!(uniforms[4].params[..3], [1.0, 0.75, 0.8]);
        assert_eq!(uniforms[4].color[..3], highlight.luminance_coefficients());
    }

    #[test]
    fn gpu_composite_request_rejects_adjustment_without_effect_plan() {
        let layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::Adjustment,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: None,
            frame_seed: 0,
        };
        let request = GpuCompositeRequest {
            width: 4,
            height: 4,
            working_color_space: WorkingColorSpace::LinearRec709,
            layers: &[layer],
        };

        assert_eq!(
            validate_request(&request),
            Err(GpuCompositeError::AdjustmentMissingEffectPlan)
        );
    }

    #[test]
    fn working_compositor_rejects_effect_plan_declared_in_external_color_domain() {
        use mondrian_effects::{
            compile_reference_effect_graph_in_domain, lower_effect_graph_to_gpu_plan,
            EffectColorDomain, EffectColorDomainContract, EffectRenderOp, EffectRenderPlan,
        };

        let domain =
            EffectColorDomain::DisplayEncodedRgb { color_space: mondrian_core::ColorSpace::Rec709 };
        let graph = compile_reference_effect_graph_in_domain(
            &EffectRenderPlan {
                ops: vec![EffectRenderOp::ColorAdjust {
                    exposure: 0.25,
                    contrast: 1.0,
                    saturation: 1.0,
                    working_color_space: WorkingColorSpace::LinearRec709,
                }],
            },
            EffectColorDomainContract::preserving(domain),
        )
        .expect("valid domain graph");
        let plan = lower_effect_graph_to_gpu_plan(&graph).expect("GPU point plan");
        let layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::Adjustment,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: Some(&plan),
            frame_seed: 0,
        };
        let request = GpuCompositeRequest {
            width: 4,
            height: 4,
            working_color_space: WorkingColorSpace::LinearRec709,
            layers: &[layer],
        };

        assert_eq!(
            validate_request(&request),
            Err(GpuCompositeError::EffectDomainRequiresExternalPass { domain })
        );
    }

    #[test]
    fn point_effect_pass_accepts_matching_display_encoded_intermediate() {
        use mondrian_effects::{
            compile_reference_effect_graph_in_domain, lower_effect_graph_to_gpu_plan,
            EffectColorDomain, EffectColorDomainContract, EffectRenderOp, EffectRenderPlan,
        };

        let domain =
            EffectColorDomain::DisplayEncodedRgb { color_space: mondrian_core::ColorSpace::Rec709 };
        let graph = compile_reference_effect_graph_in_domain(
            &EffectRenderPlan {
                ops: vec![EffectRenderOp::ColorAdjust {
                    exposure: 0.25,
                    contrast: 1.0,
                    saturation: 1.0,
                    working_color_space: WorkingColorSpace::LinearRec709,
                }],
            },
            EffectColorDomainContract::preserving(domain),
        )
        .expect("valid domain graph");
        let plan = lower_effect_graph_to_gpu_plan(&graph).expect("GPU point plan");
        let input = GpuColorFrameHandle::new(
            crate::GpuColorFrameId::from_raw(120),
            ColorFrameDescriptor {
                width: 1920,
                height: 1080,
                color_space: mondrian_core::ColorSpace::Rec709.into(),
                domain: ColorFrameDomain::Effect,
                encoding: ColorFrameEncoding::EncodedFloat,
                residency: ColorFrameResidency::Gpu,
                alpha: crate::ColorFrameAlpha::StraightCoverage,
            },
            GpuColorFrameTextureFormat::Rgba32Float,
            "display-effect-input",
        )
        .expect("effect input");

        validate_point_effect_input(&input, &plan).expect("matching point effect input");

        let opaque = GpuColorFrameHandle::new(
            crate::GpuColorFrameId::from_raw(121),
            ColorFrameDescriptor {
                alpha: crate::ColorFrameAlpha::Opaque,
                ..input.descriptor()
            },
            GpuColorFrameTextureFormat::Rgba32Float,
            "opaque-display-effect-input",
        )
        .expect("opaque effect input");
        validate_point_effect_input(&opaque, &plan)
            .expect("opaque effect input is straight-compatible");

        let premultiplied = GpuColorFrameHandle::new(
            crate::GpuColorFrameId::from_raw(122),
            ColorFrameDescriptor {
                alpha: crate::ColorFrameAlpha::PremultipliedCoverage,
                ..input.descriptor()
            },
            GpuColorFrameTextureFormat::Rgba32Float,
            "premultiplied-display-effect-input",
        )
        .expect("premultiplied effect input handle");
        assert_eq!(
            validate_point_effect_input(&premultiplied, &plan)
                .expect_err("premultiplied RGB must not cross the point-effect seam"),
            GpuCompositeError::InputNotStraightCompatibleAlpha {
                actual: crate::ColorFrameAlpha::PremultipliedCoverage,
            }
        );
    }

    #[tokio::test]
    async fn gpu_point_effects_match_cpu_float_reference_on_real_wgpu_device() {
        use crate::color_accuracy::{
            compare_linear_rgba, LinearAccuracyBudget, LinearRgbaAccuracyBudget,
        };
        use mondrian_effects::{
            apply_compiled_effect_graph_pass_rgba_f32, apply_compiled_effect_graph_rgba_f32,
            compile_reference_render_graph, lower_effect_graph_to_gpu_plan, AscCdlGrade,
            ColorCurvesAuthoring, ColorCurvesMode, EffectGraphBuilderState, EffectRenderOp,
            GamutCompressionGrade, HdrGradingAuthoring, HdrGradingZone, HighlightRecoveryGrade,
            Lut3D, PreparedColorCurves, PreparedHdrGrading, PreparedLut3D, PrimariesGrade,
            WhiteBalanceGrade,
        };

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping GPU point-effect parity test: no GPU adapter available");
            return;
        };
        let data = (0..16)
            .map(|index| {
                let value = index as f32 / 15.0;
                [
                    -0.15 + value * 1.6,
                    1.3 - value * 1.4,
                    0.05 + value * 1.2,
                    1.0,
                ]
            })
            .collect::<Vec<_>>();
        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            width: 4,
            height: 4,
            color_space: WorkingColorSpace::LinearRec709,
            data,
        });
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(EffectRenderOp::ColorAdjust {
            exposure: 0.35,
            contrast: 1.15,
            saturation: 0.8,
            working_color_space: WorkingColorSpace::LinearRec709,
        });
        builder.append_unary(EffectRenderOp::WhiteBalance {
            grade: WhiteBalanceGrade::new(0.42, -0.18, WorkingColorSpace::LinearRec709)
                .expect("valid white balance"),
        });
        builder.append_unary(EffectRenderOp::Primaries {
            grade: PrimariesGrade::new(
                [0.01, -0.015, 0.02],
                [1.08, 0.94, 1.02],
                [1.15, 0.92, 1.3],
                [1.1, 0.85, 1.05],
            )
            .expect("valid Primaries"),
        });
        builder.append_unary(EffectRenderOp::AscCdl {
            grade: AscCdlGrade::new(
                [1.05, 0.95, 1.1],
                [-0.02, 0.01, 0.0],
                [0.9, 1.1, 1.05],
                0.88,
            )
            .expect("valid ASC CDL"),
        });
        builder.append_unary(EffectRenderOp::GamutCompression {
            grade: GamutCompressionGrade::new(0.82, WorkingColorSpace::LinearRec709)
                .expect("valid gamut compression"),
        });
        builder.append_unary(EffectRenderOp::HighlightRecovery {
            grade: HighlightRecoveryGrade::new(0.72, 0.9, 0.65, WorkingColorSpace::LinearRec709)
                .expect("valid highlight recovery"),
        });
        let mut hdr_authoring = HdrGradingAuthoring {
            global_exposure_stops: 0.18,
            global_saturation: 0.94,
            ..HdrGradingAuthoring::default()
        };
        hdr_authoring.zones[HdrGradingZone::Highlights as usize].exposure_stops = 0.42;
        hdr_authoring.zones[HdrGradingZone::Highlights as usize].saturation = 0.82;
        hdr_authoring.zones[HdrGradingZone::Highlights as usize].balance = [0.08, -0.03, -0.05];
        builder.append_unary(EffectRenderOp::HdrGrading {
            grade: std::sync::Arc::new(
                PreparedHdrGrading::new(hdr_authoring, WorkingColorSpace::LinearRec709)
                    .expect("valid HDR grade"),
            ),
        });
        let identity_curve = NormalizedCurve::identity();
        let neutral_secondary = NormalizedCurve::flat(0.5).expect("neutral secondary curve");
        let master_curve = NormalizedCurve::new(vec![
            NormalizedCurvePoint::new(0.0, 0.03),
            NormalizedCurvePoint::new(0.32, 0.24),
            NormalizedCurvePoint::new(0.72, 0.81),
            NormalizedCurvePoint::new(1.0, 0.97),
        ])
        .expect("master curve");
        let red_curve = NormalizedCurve::new(vec![
            NormalizedCurvePoint::new(0.0, 0.0),
            NormalizedCurvePoint::new(0.45, 0.51),
            NormalizedCurvePoint::new(1.0, 1.0),
        ])
        .expect("red curve");
        let hue_vs_hue = NormalizedCurve::new(vec![
            NormalizedCurvePoint::new(0.0, 0.5),
            NormalizedCurvePoint::new(0.5, 0.56),
            NormalizedCurvePoint::new(1.0, 0.5),
        ])
        .expect("hue-vs-hue curve");
        let hue_vs_saturation = NormalizedCurve::new(vec![
            NormalizedCurvePoint::new(0.0, 0.48),
            NormalizedCurvePoint::new(0.65, 0.55),
            NormalizedCurvePoint::new(1.0, 0.48),
        ])
        .expect("hue-vs-saturation curve");
        let luma_vs_saturation = NormalizedCurve::new(vec![
            NormalizedCurvePoint::new(0.0, 0.46),
            NormalizedCurvePoint::new(0.5, 0.54),
            NormalizedCurvePoint::new(1.0, 0.5),
        ])
        .expect("luma-vs-saturation curve");
        let saturation_vs_luma = NormalizedCurve::new(vec![
            NormalizedCurvePoint::new(0.0, 0.5),
            NormalizedCurvePoint::new(0.4, 0.47),
            NormalizedCurvePoint::new(1.0, 0.53),
        ])
        .expect("saturation-vs-luma curve");
        builder.append_unary(EffectRenderOp::ColorCurves {
            curves: std::sync::Arc::new(PreparedColorCurves::new(
                ColorCurvesAuthoring {
                    mode: ColorCurvesMode::YRgb,
                    master: &master_curve,
                    red: &red_curve,
                    green: &identity_curve,
                    blue: &identity_curve,
                    hue_vs_hue: &hue_vs_hue,
                    hue_vs_saturation: &hue_vs_saturation,
                    hue_vs_luma: &neutral_secondary,
                    luma_vs_saturation: &luma_vs_saturation,
                    saturation_vs_saturation: &neutral_secondary,
                    saturation_vs_luma: &saturation_vs_luma,
                },
                WorkingColorSpace::LinearRec709,
            )),
        });
        let mut lut_data = Vec::new();
        for blue in 0..3 {
            for green in 0..3 {
                for red in 0..3 {
                    let r = red as f32 * 0.5;
                    let g = green as f32 * 0.5;
                    let b = blue as f32 * 0.5;
                    lut_data.push([
                        0.03 + 0.82 * r + 0.06 * g,
                        0.01 + 0.88 * g + 0.04 * b,
                        0.02 + 0.84 * b + 0.05 * r,
                    ]);
                }
            }
        }
        builder.append_unary(EffectRenderOp::Lut3D {
            lut: std::sync::Arc::new(PreparedLut3D::new(Lut3D {
                name: "gpu-parity-domain-lut".to_owned(),
                size: 3,
                domain_min: [-0.2, -0.1, 0.0],
                domain_max: [1.2, 1.1, 1.0],
                data: lut_data,
            })),
            intensity: 0.63,
        });
        builder.append_unary(EffectRenderOp::Vignette { intensity: 0.45, feather: 0.7 });
        let mut second_lut_data = Vec::new();
        for blue in 0..2 {
            for green in 0..2 {
                for red in 0..2 {
                    second_lut_data.push([
                        1.0 - red as f32,
                        green as f32 * 0.9,
                        blue as f32 * 0.85,
                    ]);
                }
            }
        }
        builder.append_unary(EffectRenderOp::Lut3D {
            lut: std::sync::Arc::new(PreparedLut3D::new(Lut3D {
                name: "gpu-parity-second-lut".to_owned(),
                size: 2,
                domain_min: [0.0; 3],
                domain_max: [1.0; 3],
                data: second_lut_data,
            })),
            intensity: 0.27,
        });
        builder.append_unary(EffectRenderOp::Grain { amount: 0.1 });
        builder.append_unary(EffectRenderOp::Crop {
            left: 0.25,
            top: 0.0,
            right: 0.0,
            bottom: 0.25,
        });
        let graph = compile_reference_render_graph(builder.finish()).expect("valid graph");
        let plan = lower_effect_graph_to_gpu_plan(&graph).expect("supported point effects");
        let expected =
            apply_compiled_effect_graph_rgba_f32(&frame.rgba_f32().data, 4, 4, &graph, 23)
                .expect("CPU float reference");

        let media_layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::CpuFrame(&frame),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: Some(&plan),
            frame_seed: 23,
        };
        let actual = readback_test_composite(&context, &[media_layer]);
        assert_test_pixels_accurate(&expected, &actual);

        let adjustment_opacity = 0.55;
        let expected_adjustment = apply_compiled_effect_graph_pass_rgba_f32(
            &frame.rgba_f32().data,
            4,
            4,
            &graph,
            adjustment_opacity,
            None,
            23,
        )
        .expect("CPU float adjustment reference");
        let base_layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::CpuFrame(&frame),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: None,
            frame_seed: 0,
        };
        let adjustment_layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::Adjustment,
            opacity: adjustment_opacity,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: Some(&plan),
            frame_seed: 23,
        };
        let actual_adjustment = readback_test_composite(&context, &[base_layer, adjustment_layer]);
        assert_test_pixels_accurate(&expected_adjustment, &actual_adjustment);

        let aces_frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            width: 2,
            height: 2,
            color_space: WorkingColorSpace::AcesCg,
            data: vec![
                [0.966_634_1, 0.048_190_45, 0.007_193, 0.25],
                [0.001_423_957, 1.312_399_1, -0.223_322_99, 0.5],
                [-0.081_868_97, -0.279_064_9, 1.386_940_2, 0.75],
                [3.0, 1.0, 0.2, 1.0],
            ],
        });
        let mut aces_builder = EffectGraphBuilderState::new();
        aces_builder.append_unary(EffectRenderOp::GamutCompression {
            grade: GamutCompressionGrade::new(1.0, WorkingColorSpace::AcesCg)
                .expect("ACES gamut compression"),
        });
        aces_builder.append_unary(EffectRenderOp::HighlightRecovery {
            grade: HighlightRecoveryGrade::new(1.0, 1.0, 0.7, WorkingColorSpace::AcesCg)
                .expect("ACES highlight recovery"),
        });
        let aces_graph =
            compile_reference_render_graph(aces_builder.finish()).expect("ACES grade graph");
        let aces_plan = lower_effect_graph_to_gpu_plan(&aces_graph).expect("ACES GPU point plan");
        let aces_expected =
            apply_compiled_effect_graph_rgba_f32(&aces_frame.rgba_f32().data, 2, 2, &aces_graph, 0)
                .expect("ACES CPU reference");
        let aces_layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::CpuFrame(&aces_frame),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: Some(&aces_plan),
            frame_seed: 0,
        };
        let aces_actual = readback_test_composite_in_space(
            &context,
            &[aces_layer],
            2,
            2,
            WorkingColorSpace::AcesCg,
        );
        assert_test_pixels_accurate(&aces_expected, &aces_actual);

        fn assert_test_pixels_accurate(expected: &[[f32; 4]], actual: &[[f32; 4]]) {
            let report = compare_linear_rgba(
                expected,
                actual,
                LinearRgbaAccuracyBudget {
                    rgb: LinearAccuracyBudget::finite(3.0e-5, 1.0e-5, 2.0e-5),
                    alpha: LinearAccuracyBudget::finite(1.0e-6, 1.0e-7, 1.0e-6),
                },
            )
            .expect("matching GPU and CPU frame shapes");
            assert!(
                report.within_budget,
                "GPU point-effect accuracy budget exceeded: {report:#?}"
            );
        }
    }

    #[tokio::test]
    async fn gpu_lut_grade_chain_keeps_resident_source_native_and_reuses_lut_upload() {
        use mondrian_effects::{
            compile_reference_render_graph, lower_effect_graph_to_gpu_plan,
            EffectGraphBuilderState, EffectRenderOp, Lut3D, PreparedLut3D,
        };

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping resident GPU LUT test: no GPU adapter available");
            return;
        };
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(EffectRenderOp::ColorAdjust {
            exposure: 0.2,
            contrast: 1.05,
            saturation: 0.95,
            working_color_space: WorkingColorSpace::LinearRec709,
        });
        builder.append_unary(EffectRenderOp::Lut3D {
            lut: std::sync::Arc::new(PreparedLut3D::new(
                Lut3D::identity(3).expect("identity LUT"),
            )),
            intensity: 0.8,
        });
        let graph = compile_reference_render_graph(builder.finish()).expect("valid graph");
        let plan = lower_effect_graph_to_gpu_plan(&graph).expect("GPU LUT grade plan");
        let descriptor = ColorFrameDescriptor {
            width: 4,
            height: 4,
            color_space: WorkingColorSpace::LinearRec709.into(),
            domain: ColorFrameDomain::Working,
            encoding: ColorFrameEncoding::LinearFloat,
            residency: ColorFrameResidency::Gpu,
            alpha: crate::ColorFrameAlpha::StraightCoverage,
        };
        let input = GpuColorFrameHandle::new(
            crate::GpuColorFrameId::from_raw(1_200),
            descriptor,
            GpuColorFrameTextureFormat::Rgba32Float,
            "resident-lut-grade-input",
        )
        .expect("input handle");
        let input_resource = GpuColorFrameUploader::allocate(
            &context.device,
            &GpuColorFrameAllocationPlan::for_handle(input.clone()),
        );
        let input_pixels = [[0.18_f32, 0.35, 0.72, 1.0]; 16];
        context.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &input_resource.resource().texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&input_pixels),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(64),
                rows_per_image: Some(4),
            },
            wgpu::Extent3d { width: 4, height: 4, depth_or_array_layers: 1 },
        );
        let compositor = GpuFrameCompositor::new(&context.device).expect("GPU compositor");
        let mut ids = GpuColorFrameIdAllocator::new(1_201).expect("frame IDs");
        let mut table = GpuColorFrameResourceTable::new();
        table.insert(input_resource).expect("resident source");
        let layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::GpuFrame(&input),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: Some(&plan),
            frame_seed: 0,
        };

        for pass_index in 0..2 {
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("resident-gpu-lut-grade"),
                });
            let record = compositor
                .record(
                    &context.device,
                    &context.queue,
                    &mut encoder,
                    &mut ids,
                    &mut table,
                    None,
                    GpuCompositeRequest {
                        width: 4,
                        height: 4,
                        working_color_space: WorkingColorSpace::LinearRec709,
                        layers: &[layer],
                    },
                )
                .expect("resident GPU LUT composite");
            assert_eq!(record.diagnostics.gpu_native_composites, 1);
            assert_eq!(record.diagnostics.gpu_with_upload_composites, 0);
            context.queue.submit(std::iter::once(encoder.finish()));
            compositor.clear_frame_resources();
            let diagnostics = compositor.creative_lut_diagnostics();
            assert_eq!(diagnostics.texture_uploads, 1);
            if pass_index == 0 {
                assert_eq!(diagnostics.cache_misses, 1);
                assert_eq!(diagnostics.cache_hits, 0);
            } else {
                assert_eq!(diagnostics.cache_hits, 1);
            }
        }
    }

    #[tokio::test]
    async fn all_gpu_blend_modes_match_cpu_float_reference_with_full_frame_seed() {
        use mondrian_effects::blend_rgba_f32_pixel_seeded;

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping GPU BlendMode parity test: no GPU adapter available");
            return;
        };
        const FRAME_SEED: i64 = 0x1234_5678_9abc_def0_u64 as i64;
        const OPACITY: f32 = 0.63;
        let base_data = vec![
            [0.0, 0.0, 0.0, 0.0],
            [1.0, 1.0, 1.0, 1.0],
            [0.5, 0.5, 0.5, 0.5],
            [0.0, 1.0, 1.0, 1.0],
            [1.0, 0.0, 1.0, 0.25],
            [1.0, 1.0, 0.0, 0.75],
            [0.001, 0.999, 0.5, 1.0],
            [0.999, 0.001, 0.5, 0.001],
            [0.08, 0.91, 0.21, 0.2],
            [0.81, 0.39, 0.58, 0.95],
            [0.25, 0.25, 0.75, 0.4],
            [0.75, 0.25, 0.25, 0.6],
            [0.25, 0.75, 0.25, 0.8],
            [0.2, 0.4, 0.6, 1.0],
            [0.6, 0.4, 0.2, 0.35],
            [0.13, 0.87, 0.43, 0.67],
        ];
        let overlay_data = vec![
            [1.0, 1.0, 1.0, 1.0],
            [0.0, 0.0, 0.0, 0.0],
            [0.5, 0.5, 0.5, 1.0],
            [1.0, 0.0, 0.0, 0.9],
            [0.0, 1.0, 0.0, 0.7],
            [0.0, 0.0, 1.0, 0.5],
            [0.999, 0.001, 0.5, 0.3],
            [0.001, 0.999, 0.5, 1.0],
            [0.83, 0.17, 0.74, 0.9],
            [0.22, 0.86, 0.31, 0.35],
            [0.75, 0.75, 0.25, 0.6],
            [0.25, 0.75, 0.75, 0.8],
            [0.75, 0.25, 0.75, 0.4],
            [0.9, 0.3, 0.1, 0.55],
            [0.1, 0.3, 0.9, 0.85],
            [0.87, 0.13, 0.57, 0.73],
        ];
        let base = CpuColorFrame::working(WorkingRgbaF32Frame {
            width: 4,
            height: 4,
            color_space: WorkingColorSpace::LinearRec709,
            data: base_data.clone(),
        });
        let overlay = CpuColorFrame::working(WorkingRgbaF32Frame {
            width: 4,
            height: 4,
            color_space: WorkingColorSpace::LinearRec709,
            data: overlay_data.clone(),
        });
        let modes = [
            BlendMode::Normal,
            BlendMode::Dissolve,
            BlendMode::Multiply,
            BlendMode::Screen,
            BlendMode::Overlay,
            BlendMode::Darken,
            BlendMode::Lighten,
            BlendMode::ColorDodge,
            BlendMode::ColorBurn,
            BlendMode::HardLight,
            BlendMode::SoftLight,
            BlendMode::Difference,
            BlendMode::Exclusion,
            BlendMode::Subtract,
            BlendMode::DarkerColor,
            BlendMode::LighterColor,
            BlendMode::LinearBurn,
            BlendMode::LinearDodge,
            BlendMode::VividLight,
            BlendMode::LinearLight,
            BlendMode::PinLight,
            BlendMode::HardMix,
            BlendMode::Divide,
            BlendMode::Hue,
            BlendMode::Saturation,
            BlendMode::Color,
            BlendMode::Luminosity,
        ];

        for mode in modes {
            let base_layer = GpuCompositeLayer {
                source: GpuCompositeLayerSource::CpuFrame(&base),
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_plan: None,
                frame_seed: 0,
            };
            let overlay_layer = GpuCompositeLayer {
                source: GpuCompositeLayerSource::CpuFrame(&overlay),
                opacity: OPACITY,
                blend_mode: mode,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_plan: None,
                frame_seed: FRAME_SEED,
            };
            let actual = readback_test_composite(&context, &[base_layer, overlay_layer]);
            let expected = base_data
                .iter()
                .zip(&overlay_data)
                .enumerate()
                .map(|(index, (base, overlay))| {
                    let base =
                        blend_rgba_f32_pixel_seeded([0.0; 4], *base, 1.0, BlendMode::Normal, 0);
                    let dither_seed = (index as u32)
                        ^ (FRAME_SEED as u32).rotate_left(13)
                        ^ ((FRAME_SEED >> 32) as u32).rotate_right(7);
                    blend_rgba_f32_pixel_seeded(base, *overlay, OPACITY, mode, dither_seed)
                })
                .collect::<Vec<_>>();
            for (index, (expected, actual)) in expected.iter().zip(&actual).enumerate() {
                for channel in 0..4 {
                    assert!(
                        (expected[channel] - actual[channel]).abs() <= 8.0e-5,
                        "{mode:?} mismatch at pixel {index}, channel {channel}: expected {}, actual {}",
                        expected[channel],
                        actual[channel]
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn gpu_compositor_preserves_positive_sixteen_bit_alpha_and_opacity() {
        use mondrian_effects::blend_rgba_f32_pixel;

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping low-coverage GPU parity test: no GPU adapter available");
            return;
        };
        let edge = 1.0 / 65_535.0;
        let low_coverage = CpuColorFrame::working(WorkingRgbaF32Frame {
            width: 4,
            height: 4,
            color_space: WorkingColorSpace::LinearRec709,
            data: vec![[1.25, -0.25, 0.5, edge]; 16],
        });
        let low_coverage_layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::CpuFrame(&low_coverage),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: None,
            frame_seed: 0,
        };
        let low_coverage_actual = readback_test_composite(&context, &[low_coverage_layer]);
        for pixel in low_coverage_actual {
            assert_eq!(pixel, [1.25, -0.25, 0.5, edge]);
        }

        let base_pixel = [0.1, 0.3, 0.7, 1.0];
        let source_pixel = [1.5, -0.5, 0.125, 1.0];
        let base = CpuColorFrame::working(WorkingRgbaF32Frame {
            width: 4,
            height: 4,
            color_space: WorkingColorSpace::LinearRec709,
            data: vec![base_pixel; 16],
        });
        let source = CpuColorFrame::working(WorkingRgbaF32Frame {
            width: 4,
            height: 4,
            color_space: WorkingColorSpace::LinearRec709,
            data: vec![source_pixel; 16],
        });
        let layers = [
            GpuCompositeLayer {
                source: GpuCompositeLayerSource::CpuFrame(&base),
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_plan: None,
                frame_seed: 0,
            },
            GpuCompositeLayer {
                source: GpuCompositeLayerSource::CpuFrame(&source),
                opacity: edge,
                blend_mode: BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_plan: None,
                frame_seed: 0,
            },
        ];
        let expected = blend_rgba_f32_pixel(base_pixel, source_pixel, edge, BlendMode::Normal);
        let low_opacity_actual = readback_test_composite(&context, &layers);
        for (pixel_index, pixel) in low_opacity_actual.iter().enumerate() {
            for channel in 0..4 {
                assert!(
                    (pixel[channel] - expected[channel]).abs() <= 2.0e-7,
                    "pixel {pixel_index}, channel {channel}: expected {}, got {}",
                    expected[channel],
                    pixel[channel]
                );
            }
        }
    }

    #[tokio::test]
    async fn affine_procedural_solid_matches_cpu_float_effect_reference() {
        use mondrian_effects::{
            apply_compiled_effect_graph_rgba_f32, compile_reference_render_graph,
            lower_effect_graph_to_gpu_plan, EffectGraphBuilderState, EffectRenderOp,
        };

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping affine procedural-solid parity test: no GPU adapter available");
            return;
        };
        let color = Color { r: 1.25, g: -0.125, b: 0.375, a: 0.625 };
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(EffectRenderOp::Vignette { intensity: 0.55, feather: 0.65 });
        let graph =
            compile_reference_render_graph(builder.finish()).expect("valid procedural-solid graph");
        let plan = lower_effect_graph_to_gpu_plan(&graph).expect("GPU vignette plan");
        let effected_source = apply_compiled_effect_graph_rgba_f32(
            &vec![[color.r, color.g, color.b, color.a]; 16],
            4,
            4,
            &graph,
            17,
        )
        .expect("CPU float procedural-solid reference");
        let mut expected = vec![[0.0, 0.0, 0.0, 0.0]; 16];
        for y in 0..4 {
            for x in 1..4 {
                let source = effected_source[y * 4 + (x - 1)];
                expected[y * 4 + x] = source;
            }
        }
        let layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::SolidColor(color),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 1.0, 0.0, 1.0, 0.0],
            effect_plan: Some(&plan),
            frame_seed: 17,
        };

        let actual = readback_test_composite(&context, &[layer]);
        for (index, (expected, actual)) in expected.iter().zip(&actual).enumerate() {
            for channel in 0..4 {
                assert!(
                    (expected[channel] - actual[channel]).abs() <= 3.0e-5,
                    "affine solid mismatch at pixel {index}, channel {channel}: expected {}, actual {}",
                    expected[channel],
                    actual[channel]
                );
            }
        }
    }

    #[tokio::test]
    async fn solid_source_materialization_preserves_linear_rgba() {
        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping solid source materialization test: no GPU adapter available");
            return;
        };
        let color = Color { r: 1.25, g: -0.125, b: 0.375, a: 0.625 };
        let compositor = GpuFrameCompositor::new(&context.device).expect("GPU compositor");
        let mut ids = GpuColorFrameIdAllocator::new(950).expect("frame id allocator");
        let mut table = GpuColorFrameResourceTable::new();
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("solid-source-materialization"),
        });
        let record = compositor
            .record_solid_source_pass(
                &context.device,
                &context.queue,
                &mut encoder,
                &mut ids,
                &mut table,
                None,
                4,
                4,
                WorkingColorSpace::LinearRec709,
                color,
            )
            .expect("record solid source materialization");
        assert_eq!(record.materialized_pixels, 16);
        assert_eq!(
            record.output.descriptor(),
            ColorFrameDescriptor {
                width: 4,
                height: 4,
                color_space: WorkingColorSpace::LinearRec709.into(),
                domain: ColorFrameDomain::Working,
                encoding: ColorFrameEncoding::LinearFloat,
                residency: ColorFrameResidency::Gpu,
                alpha: crate::ColorFrameAlpha::StraightCoverage,
            }
        );
        let readback = context.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("solid-source-materialization-readback"),
            size: 256 * 4,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let output = table.get(&record.output).expect("solid source output");
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &output.resource().texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(4),
                },
            },
            wgpu::Extent3d { width: 4, height: 4, depth_or_array_layers: 1 },
        );
        context.queue.submit(std::iter::once(encoder.finish()));
        let mapped = map_test_readback(&context.device, &readback);
        for row in mapped.chunks_exact(256).take(4) {
            for pixel in bytemuck::cast_slice::<u8, f32>(&row[..64]).chunks_exact(4) {
                assert_eq!(pixel, &[color.r, color.g, color.b, color.a]);
            }
        }
        readback.unmap();
    }

    #[tokio::test]
    async fn cross_dissolve_matches_shared_cpu_straight_alpha_reference() {
        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping Cross Dissolve parity test: no GPU adapter available");
            return;
        };
        let left_color = Color { r: 1.2, g: -0.1, b: 0.35, a: 0.25 };
        let right_color = Color { r: 0.05, g: 0.8, b: 1.4, a: 0.75 };
        let progress = 0.4;
        let compositor = GpuFrameCompositor::new(&context.device).expect("GPU compositor");
        let mut ids = GpuColorFrameIdAllocator::new(960).expect("frame id allocator");
        let mut table = GpuColorFrameResourceTable::new();
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("cross-dissolve-parity"),
        });
        let left = compositor
            .record_solid_source_pass(
                &context.device,
                &context.queue,
                &mut encoder,
                &mut ids,
                &mut table,
                None,
                4,
                4,
                WorkingColorSpace::LinearRec709,
                left_color,
            )
            .expect("materialize left Transition source")
            .output;
        let right = compositor
            .record_solid_source_pass(
                &context.device,
                &context.queue,
                &mut encoder,
                &mut ids,
                &mut table,
                None,
                4,
                4,
                WorkingColorSpace::LinearRec709,
                right_color,
            )
            .expect("materialize right Transition source")
            .output;
        let record = compositor
            .record_cross_dissolve_pass(
                &context.device,
                &context.queue,
                &mut encoder,
                &mut ids,
                &mut table,
                None,
                &left,
                &right,
                progress,
                WorkingColorSpace::LinearRec709,
            )
            .expect("record typed Cross Dissolve");

        assert_eq!(record.diagnostics.gpu_cross_dissolve_passes, 1);
        assert_eq!(record.diagnostics.gpu_composited_pixels, 16);
        assert_eq!(
            record.output.descriptor().alpha,
            crate::ColorFrameAlpha::StraightCoverage
        );

        let readback = context.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("cross-dissolve-parity-readback"),
            size: 256 * 4,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let output = table.get(&record.output).expect("Cross Dissolve output");
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &output.resource().texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(4),
                },
            },
            wgpu::Extent3d { width: 4, height: 4, depth_or_array_layers: 1 },
        );
        context.queue.submit(std::iter::once(encoder.finish()));
        let mapped = map_test_readback(&context.device, &readback);
        let mut expected = [[0.0; 4]];
        crate::timeline_composite::cross_dissolve_straight_rgba_f32(
            &mut expected,
            &[[left_color.r, left_color.g, left_color.b, left_color.a]],
            &[[right_color.r, right_color.g, right_color.b, right_color.a]],
            progress,
        );
        for row in mapped.chunks_exact(256).take(4) {
            for actual in bytemuck::cast_slice::<u8, f32>(&row[..64]).chunks_exact(4) {
                for channel in 0..4 {
                    assert!(
                        (actual[channel] - expected[0][channel]).abs() <= 1.0e-6,
                        "Cross Dissolve mismatch in channel {channel}: expected {}, actual {}",
                        expected[0][channel],
                        actual[channel]
                    );
                }
            }
        }
        readback.unmap();
    }

    #[tokio::test]
    async fn compositor_uniform_arena_reuses_pages_across_submitted_frames() {
        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping compositor uniform arena test: no GPU adapter available");
            return;
        };
        let compositor = GpuFrameCompositor::new(&context.device).expect("GPU compositor");
        let mut ids = GpuColorFrameIdAllocator::new(975).expect("frame id allocator");
        let mut table = GpuColorFrameResourceTable::new();

        for frame in 0..2 {
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("compositor-uniform-arena-reuse"),
                });
            compositor
                .record_solid_source_pass(
                    &context.device,
                    &context.queue,
                    &mut encoder,
                    &mut ids,
                    &mut table,
                    None,
                    4,
                    4,
                    WorkingColorSpace::LinearRec709,
                    Color { r: frame as f32, g: 0.25, b: 0.5, a: 1.0 },
                )
                .expect("record solid through uniform arena");
            context.queue.submit(std::iter::once(encoder.finish()));
            compositor.clear_frame_resources();
        }

        let diagnostics = compositor.uniform_arena_diagnostics();
        assert_eq!(diagnostics.buffer_creations, 1);
        assert_eq!(diagnostics.uniform_writes, 2);
        assert_eq!(diagnostics.high_watermark_slots, 1);
        assert_eq!(diagnostics.high_watermark_pages, 1);
        assert_eq!(diagnostics.frame_resets, 2);
        assert_eq!(
            compositor.texture_binding_diagnostics().bind_group_creations,
            2
        );
        assert_eq!(compositor.texture_binding_diagnostics().cache_hits, 4);
    }

    #[tokio::test]
    async fn compositor_uniform_arena_pages_without_a_semantic_pass_limit() {
        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping compositor uniform paging test: no GPU adapter available");
            return;
        };
        let compositor = GpuFrameCompositor::new(&context.device).expect("GPU compositor");
        let mut ids = GpuColorFrameIdAllocator::new(1_200).expect("frame id allocator");
        let mut table = GpuColorFrameResourceTable::new();
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("compositor-uniform-arena-paging"),
        });

        for frame_seed in 0..=GPU_COMPOSITOR_UNIFORM_PAGE_SLOTS {
            compositor
                .record_solid_source_pass(
                    &context.device,
                    &context.queue,
                    &mut encoder,
                    &mut ids,
                    &mut table,
                    None,
                    1,
                    1,
                    WorkingColorSpace::LinearRec709,
                    Color {
                        r: frame_seed as f32 / GPU_COMPOSITOR_UNIFORM_PAGE_SLOTS as f32,
                        g: 0.25,
                        b: 0.5,
                        a: 1.0,
                    },
                )
                .expect("record pass across a uniform page boundary");
        }
        context.queue.submit(std::iter::once(encoder.finish()));

        let diagnostics = compositor.uniform_arena_diagnostics();
        assert_eq!(diagnostics.buffer_creations, 2);
        assert_eq!(diagnostics.uniform_writes, 129);
        assert_eq!(diagnostics.high_watermark_slots, 129);
        assert_eq!(diagnostics.high_watermark_pages, 2);
    }

    #[tokio::test]
    async fn compositor_reuses_bind_groups_with_pooled_input_textures() {
        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping compositor texture-binding cache test: no GPU adapter available");
            return;
        };
        fn insert_inputs(
            device: &wgpu::Device,
            ids: &mut GpuColorFrameIdAllocator,
            table: &mut GpuColorFrameResourceTable<GpuColorFrameWgpuResource>,
            pool: &GpuColorFrameWgpuResourcePool,
            descriptor: ColorFrameDescriptor,
        ) -> [GpuColorFrameHandle; 2] {
            ["binding-cache-input-a", "binding-cache-input-b"].map(|label| {
                let handle = GpuColorFrameHandle::new(
                    ids.allocate().expect("input frame id"),
                    descriptor,
                    GpuColorFrameTextureFormat::Rgba32Float,
                    label,
                )
                .expect("working input handle");
                let allocation = GpuColorFrameAllocationPlan::for_handle(handle.clone());
                table.insert(pool.acquire(device, &allocation)).expect("insert working input");
                handle
            })
        }

        let compositor = GpuFrameCompositor::new(&context.device).expect("GPU compositor");
        let mut ids = GpuColorFrameIdAllocator::new(1_025).expect("frame id allocator");
        let mut table = GpuColorFrameResourceTable::new();
        let pool = GpuColorFrameWgpuResourcePool::default();
        let descriptor = ColorFrameDescriptor {
            width: 4,
            height: 4,
            color_space: WorkingColorSpace::LinearRec709.into(),
            domain: ColorFrameDomain::Working,
            encoding: ColorFrameEncoding::LinearFloat,
            residency: ColorFrameResidency::Gpu,
            alpha: crate::ColorFrameAlpha::StraightCoverage,
        };
        let mut inputs = insert_inputs(&context.device, &mut ids, &mut table, &pool, descriptor);
        let mut after_first = None;

        for frame in 0..2 {
            let layer = |handle| GpuCompositeLayer {
                source: GpuCompositeLayerSource::GpuFrame(handle),
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_plan: None,
                frame_seed: 0,
            };
            let layers = [layer(&inputs[0]), layer(&inputs[1])];
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("compositor-texture-binding-cache"),
                });
            compositor
                .record(
                    &context.device,
                    &context.queue,
                    &mut encoder,
                    &mut ids,
                    &mut table,
                    None,
                    GpuCompositeRequest {
                        width: 4,
                        height: 4,
                        working_color_space: WorkingColorSpace::LinearRec709,
                        layers: &layers,
                    },
                )
                .expect("record pooled-input composite");
            context.queue.submit(std::iter::once(encoder.finish()));
            compositor.clear_frame_resources();
            if frame == 0 {
                after_first = Some(compositor.texture_binding_diagnostics());
                for handle in &inputs {
                    let resource =
                        table.remove(handle.id()).expect("remove submitted input resource");
                    pool.release(resource);
                }
                inputs = insert_inputs(&context.device, &mut ids, &mut table, &pool, descriptor);
            }
        }

        let after_first = after_first.expect("first frame diagnostics");
        let after_second = compositor.texture_binding_diagnostics();
        assert_eq!(
            after_second.bind_group_creations - after_first.bind_group_creations,
            2,
            "only the two new accumulator textures should need bindings"
        );
        assert_eq!(
            after_second.cache_hits - after_first.cache_hits,
            2,
            "both pooled input textures should reuse their layer bindings"
        );
        assert_eq!(pool.diagnostics().hits, 2);
    }

    #[tokio::test]
    async fn standalone_effect_domain_point_pass_matches_numeric_reference() {
        use mondrian_effects::{
            compile_reference_effect_graph_in_domain, lower_effect_graph_to_gpu_plan,
            EffectColorDomain, EffectColorDomainContract, EffectRenderOp, EffectRenderPlan,
        };

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping standalone point-effect test: no GPU adapter available");
            return;
        };
        let domain =
            EffectColorDomain::DisplayEncodedRgb { color_space: mondrian_core::ColorSpace::Rec709 };
        let graph = compile_reference_effect_graph_in_domain(
            &EffectRenderPlan {
                ops: vec![EffectRenderOp::ColorAdjust {
                    exposure: 1.0,
                    contrast: 1.0,
                    saturation: 1.0,
                    working_color_space: WorkingColorSpace::LinearRec709,
                }],
            },
            EffectColorDomainContract::preserving(domain),
        )
        .expect("valid display-domain graph");
        let plan = lower_effect_graph_to_gpu_plan(&graph).expect("GPU point plan");
        let data = (0..16)
            .map(|index| {
                let value = index as f32 / 32.0;
                [value, value * 0.75, value * 0.5, 1.0]
            })
            .collect::<Vec<_>>();
        let expected = data
            .iter()
            .map(|pixel| [pixel[0] * 2.0, pixel[1] * 2.0, pixel[2] * 2.0, pixel[3]])
            .collect::<Vec<_>>();
        let descriptor = ColorFrameDescriptor {
            width: 4,
            height: 4,
            color_space: mondrian_core::ColorSpace::Rec709.into(),
            domain: ColorFrameDomain::Effect,
            encoding: ColorFrameEncoding::EncodedFloat,
            residency: ColorFrameResidency::Gpu,
            alpha: crate::ColorFrameAlpha::StraightCoverage,
        };
        let input = GpuColorFrameHandle::new(
            crate::GpuColorFrameId::from_raw(900),
            descriptor,
            GpuColorFrameTextureFormat::Rgba32Float,
            "standalone-effect-input",
        )
        .expect("effect input");
        let input_allocation = GpuColorFrameAllocationPlan::for_handle(input.clone());
        let input_resource = GpuColorFrameUploader::allocate(&context.device, &input_allocation);
        context.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &input_resource.resource().texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&data),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(64),
                rows_per_image: Some(4),
            },
            wgpu::Extent3d { width: 4, height: 4, depth_or_array_layers: 1 },
        );
        let compositor = GpuFrameCompositor::new(&context.device).expect("GPU compositor");
        let mut ids = GpuColorFrameIdAllocator::new(901).expect("frame id allocator");
        let mut table = GpuColorFrameResourceTable::new();
        table.insert(input_resource).expect("insert effect input");
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("standalone-effect-domain-point-pass"),
        });
        let record = compositor
            .record_point_effect_pass(
                &context.device,
                &context.queue,
                &mut encoder,
                &mut ids,
                &mut table,
                None,
                &input,
                &plan,
                0,
            )
            .expect("record standalone point pass");
        assert_eq!(record.output.descriptor(), descriptor);
        assert_eq!(record.processed_pixels, 16);
        let readback = context.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("standalone-effect-domain-readback"),
            size: 256 * 4,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let output = table.get(&record.output).expect("point pass output");
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &output.resource().texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(4),
                },
            },
            wgpu::Extent3d { width: 4, height: 4, depth_or_array_layers: 1 },
        );
        context.queue.submit(std::iter::once(encoder.finish()));
        let mapped = map_test_readback(&context.device, &readback);
        let actual = mapped
            .chunks_exact(256)
            .take(4)
            .flat_map(|row| {
                bytemuck::cast_slice::<u8, f32>(&row[..64])
                    .chunks_exact(4)
                    .map(|pixel| [pixel[0], pixel[1], pixel[2], pixel[3]])
            })
            .collect::<Vec<_>>();
        readback.unmap();

        for (expected, actual) in expected.iter().zip(actual.iter()) {
            for channel in 0..4 {
                assert!((expected[channel] - actual[channel]).abs() <= 1.0e-6);
            }
        }
    }

    fn readback_test_composite(
        context: &crate::GpuContext,
        layers: &[GpuCompositeLayer<'_>],
    ) -> Vec<[f32; 4]> {
        readback_test_composite_in_space(context, layers, 4, 4, WorkingColorSpace::LinearRec709)
    }

    fn readback_test_composite_in_space(
        context: &crate::GpuContext,
        layers: &[GpuCompositeLayer<'_>],
        width: u32,
        height: u32,
        working_color_space: WorkingColorSpace,
    ) -> Vec<[f32; 4]> {
        let compositor = GpuFrameCompositor::new(&context.device).expect("GPU compositor");
        let mut ids = GpuColorFrameIdAllocator::new(1).expect("frame id allocator");
        let mut table = GpuColorFrameResourceTable::new();
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian-test-gpu-point-effect-parity"),
        });
        let record = compositor
            .record(
                &context.device,
                &context.queue,
                &mut encoder,
                &mut ids,
                &mut table,
                None,
                GpuCompositeRequest { width, height, working_color_space, layers },
            )
            .expect("record GPU effect composite");
        let readback = context.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mondrian-test-gpu-point-effect-readback"),
            size: 256 * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let output = table.get(&record.output).expect("composite output resource");
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &output.resource().texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        context.queue.submit(std::iter::once(encoder.finish()));
        let mapped = map_test_readback(&context.device, &readback);
        let mut actual = Vec::with_capacity((width * height) as usize);
        for row in mapped.chunks_exact(256).take(height as usize) {
            actual.extend(
                bytemuck::cast_slice::<u8, f32>(&row[..width as usize * 16])
                    .chunks_exact(4)
                    .map(|pixel| [pixel[0], pixel[1], pixel[2], pixel[3]]),
            );
        }
        readback.unmap();
        actual
    }

    fn map_test_readback(device: &wgpu::Device, buffer: &wgpu::Buffer) -> Vec<u8> {
        let (tx, rx) = std::sync::mpsc::channel();
        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        let _ = device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None });
        rx.recv().expect("readback callback").expect("readback mapping");
        slice.get_mapped_range().expect("mapped readback range").to_vec()
    }

    #[test]
    fn gpu_composite_request_accepts_canonical_blend_mode() {
        let layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::SolidColor(Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 }),
            opacity: 1.0,
            blend_mode: BlendMode::Multiply,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: None,
            frame_seed: 0,
        };
        let request = GpuCompositeRequest {
            width: 16,
            height: 16,
            working_color_space: WorkingColorSpace::LinearRec709,
            layers: &[layer],
        };

        validate_request(&request).expect("Multiply is part of the canonical GPU Blend algebra");
    }

    #[test]
    fn gpu_composite_request_accepts_media_extent_mismatch_with_affine_transform() {
        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            width: 8,
            height: 8,
            color_space: WorkingColorSpace::LinearRec709,
            data: vec![[0.0, 0.0, 0.0, 1.0]; 64],
        });
        let layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::CpuFrame(&frame),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [2.0, 0.0, 0.0, 0.0, 2.0, 0.0],
            effect_plan: None,
            frame_seed: 0,
        };
        let request = GpuCompositeRequest {
            width: 16,
            height: 16,
            working_color_space: WorkingColorSpace::LinearRec709,
            layers: &[layer],
        };

        validate_request(&request).expect("GPU compositor should support affine media sampling");
    }

    #[test]
    fn gpu_composite_request_accepts_affine_solid_transform() {
        let layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::SolidColor(Color {
                r: 1.5,
                g: -0.25,
                b: 0.5,
                a: 0.75,
            }),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [0.75, 0.0, 1.0, 0.0, 0.75, 1.0],
            effect_plan: None,
            frame_seed: 0,
        };
        let request = GpuCompositeRequest {
            width: 16,
            height: 16,
            working_color_space: WorkingColorSpace::LinearRec709,
            layers: &[layer],
        };

        validate_request(&request)
            .expect("GPU compositor should sample an invertible procedural-solid transform");

        let mut singular = layer;
        singular.transform = [0.0; 6];
        let layers = [singular];
        let request = GpuCompositeRequest { layers: &layers, ..request };
        validate_request(&request)
            .expect("a zero-area solid contributes no pixels and needs no fallback");
    }

    #[test]
    fn gpu_composite_request_accepts_gpu_resident_media_frame() {
        let handle = gpu_working_handle(10, WorkingColorSpace::LinearRec709);
        let layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::GpuFrame(&handle),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [2.0, 0.0, 0.0, 0.0, 2.0, 0.0],
            effect_plan: None,
            frame_seed: 0,
        };
        let request = GpuCompositeRequest {
            width: 16,
            height: 16,
            working_color_space: WorkingColorSpace::LinearRec709,
            layers: &[layer],
        };

        validate_request(&request).expect("GPU compositor should accept GPU-resident media layer");
    }

    #[test]
    fn gpu_composite_request_accepts_opaque_and_rejects_premultiplied_input() {
        let opaque = gpu_working_handle_with_alpha(
            12,
            WorkingColorSpace::LinearRec709,
            crate::ColorFrameAlpha::Opaque,
        );
        let opaque_layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::GpuFrame(&opaque),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: None,
            frame_seed: 0,
        };
        let opaque_layers = [opaque_layer];
        let opaque_request = GpuCompositeRequest {
            width: 8,
            height: 8,
            working_color_space: WorkingColorSpace::LinearRec709,
            layers: &opaque_layers,
        };
        validate_request(&opaque_request).expect("opaque RGB is straight-compatible");

        let premultiplied = gpu_working_handle_with_alpha(
            13,
            WorkingColorSpace::LinearRec709,
            crate::ColorFrameAlpha::PremultipliedCoverage,
        );
        let premultiplied_layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::GpuFrame(&premultiplied),
            ..opaque_layer
        };
        let premultiplied_layers = [premultiplied_layer];
        let premultiplied_request =
            GpuCompositeRequest { layers: &premultiplied_layers, ..opaque_request };

        assert_eq!(
            validate_request(&premultiplied_request)
                .expect_err("premultiplied RGB must not cross the compositor seam"),
            GpuCompositeError::InputNotStraightCompatibleAlpha {
                actual: crate::ColorFrameAlpha::PremultipliedCoverage,
            }
        );
    }

    #[test]
    fn gpu_composite_request_rejects_gpu_frame_color_space_mismatch() {
        let handle = gpu_working_handle(11, WorkingColorSpace::LinearP3D65);
        let layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::GpuFrame(&handle),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: None,
            frame_seed: 0,
        };
        let request = GpuCompositeRequest {
            width: 8,
            height: 8,
            working_color_space: WorkingColorSpace::LinearRec709,
            layers: &[layer],
        };

        let err =
            validate_request(&request).expect_err("GPU frame color-space mismatch should fail");

        assert!(matches!(
            err,
            GpuCompositeError::SourceDescriptorMismatch { .. }
        ));
    }

    #[test]
    fn gpu_composite_request_skips_zero_area_media_transform() {
        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            width: 8,
            height: 8,
            color_space: WorkingColorSpace::LinearRec709,
            data: vec![[0.0, 0.0, 0.0, 1.0]; 64],
        });
        let layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::CpuFrame(&frame),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            effect_plan: None,
            frame_seed: 0,
        };
        let request = GpuCompositeRequest {
            width: 16,
            height: 16,
            working_color_space: WorkingColorSpace::LinearRec709,
            layers: &[layer],
        };

        validate_request(&request)
            .expect("a zero-area authored layer contributes no pixels and needs no fallback");
    }

    #[test]
    fn gpu_composite_request_rejects_source_color_space_mismatch() {
        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            width: 8,
            height: 8,
            color_space: WorkingColorSpace::LinearP3D65,
            data: vec![[0.0, 0.0, 0.0, 1.0]; 64],
        });
        let layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::CpuFrame(&frame),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: None,
            frame_seed: 0,
        };
        let request = GpuCompositeRequest {
            width: 8,
            height: 8,
            working_color_space: WorkingColorSpace::LinearRec709,
            layers: &[layer],
        };

        let err = validate_request(&request).expect_err("color space mismatch should be blocked");

        assert!(matches!(
            err,
            GpuCompositeError::SourceDescriptorMismatch { .. }
        ));
    }

    fn gpu_working_handle(id: u64, color_space: WorkingColorSpace) -> GpuColorFrameHandle {
        gpu_working_handle_with_alpha(id, color_space, crate::ColorFrameAlpha::StraightCoverage)
    }

    fn gpu_working_handle_with_alpha(
        id: u64,
        color_space: WorkingColorSpace,
        alpha: crate::ColorFrameAlpha,
    ) -> GpuColorFrameHandle {
        GpuColorFrameHandle::new(
            crate::GpuColorFrameId::from_raw(id),
            ColorFrameDescriptor {
                width: 8,
                height: 8,
                color_space: color_space.into(),
                domain: ColorFrameDomain::Working,
                encoding: ColorFrameEncoding::LinearFloat,
                residency: ColorFrameResidency::Gpu,
                alpha,
            },
            GpuColorFrameTextureFormat::Rgba16Float,
            "test-gpu-working-layer",
        )
        .expect("test GPU handle")
    }
}
