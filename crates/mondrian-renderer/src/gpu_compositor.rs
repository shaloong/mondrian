//! GPU-resident working-space compositing.
//!
//! This module owns the native wgpu path for preview/playback compositing when
//! a layer stack is simple enough to stay on GPU: affine transforms, fused
//! pointwise effect graphs, the canonical BlendMode algebra, and a bounded layer count.
//! Unsupported layer shapes are rejected with typed blockers so callers can
//! fall back to the CPU reference compositor without losing diagnostic evidence.

use crate::color_frame::GpuColorFrameBindGroupCacheKey;
use crate::creative_lut_gpu::{GpuCreativeLutPreparedBinding, GpuCreativeLutRuntime};
use crate::gpu_composite_execution::{
    GpuCompositeExecutionDiagnostics, GpuCompositeExecutionPlanError, GpuCompositeExecutionPlanner,
    GpuCompositeExecutionPolicy, GpuCompositeLayerExecution, GpuCompositeLayerFootprint,
    GpuCompositeRect, GpuCompositeSourceCrop,
};
use crate::spatial_driver::{
    gpu_spatial_chain_plan_with_auxiliary, GpuSpatialChainPlan, GpuSpatialScratchPool,
    GpuSpatialStagePlan,
};
use crate::spatial_pass_gpu::EffectSpatialGpu;
use crate::{
    product_gpu_working_texture_format, ColorFrameAlpha, ColorFrameDescriptor, ColorFrameDomain,
    ColorFrameEncoding, ColorFrameResidency, CpuColorFrame, GpuColorFrameAllocationPlan,
    GpuColorFrameBindGroupCacheKeyAllocationError, GpuColorFrameHandle,
    GpuColorFrameIdAllocationError, GpuColorFrameIdAllocator, GpuColorFrameResource,
    GpuColorFrameResourceTable, GpuColorFrameTextureFormat, GpuColorFrameUploadPlan,
    GpuColorFrameUploader, GpuColorFrameWgpuResource, GpuColorFrameWgpuResourcePool,
};
use bytemuck::{Pod, Zeroable};
use mondrian_core::{
    automation::QualifierSampleOperation,
    types::{BlendMode, Color},
};
use mondrian_effects::{
    AuxiliaryPolicy, AuxiliaryResolution, CompiledEffectGpuPipeline, CompiledEffectGpuPlan,
    EffectColorDomain, EffectGpuPointOp, EffectGpuSpatialPass, EffectGpuStage, MaskOp,
    PreparedQualifier, QualifierMode, MAX_FUSED_GPU_EFFECT_OPS,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

const GPU_COMPOSITOR_UNIFORM_PAGE_SLOTS: usize = 128;
const GPU_COMPOSITOR_SHADER: &str = concat!(
    r#"
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
    extra2: vec4<f32>,
    extra3: vec4<f32>,
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
    let source_position = source_coordinate(in.position.xy);
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

@fragment
fn fs_opaque_normal(in: VsOut) -> @location(0) vec4<f32> {
    let source_position = source_coordinate(in.position.xy);
    var layer_px: vec4<f32>;
    if (uniforms.source_kind == 1u) {
        layer_px = select(vec4<f32>(0.0), uniforms.solid_color, source_inside(source_position));
    } else {
        layer_px = sample_layer(source_position);
    }
    let effect_position = source_position - vec2<f32>(0.5);
    layer_px = apply_effects(layer_px, effect_position);
    let coverage = clamp(layer_px.a * clamp(uniforms.opacity, 0.0, 1.0), 0.0, 1.0);
    return vec4<f32>(layer_px.rgb, coverage);
}

fn source_coordinate(dst_center: vec2<f32>) -> vec2<f32> {
    // Fragment position is the exact destination pixel center. Interpolated UV
    // reconstruction can fall below 0.5 at an identity edge on Vulkan, causing
    // a false outside-source result or selecting the previous grain coordinate.
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
    return src_center.x >= 0.0 &&
        src_center.y >= 0.0 &&
        src_center.x < src_size.x &&
        src_center.y < src_size.y;
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

fn invert_extremes(rgb: vec3<f32>) -> vec2<f32> {
    return vec2<f32>(max(rgb.r, max(rgb.g, rgb.b)), min(rgb.r, min(rgb.g, rgb.b)));
}

fn invert_rgb_from_hue_chroma_lightness(
    hue: f32,
    chroma: f32,
    lightness: f32,
) -> vec3<f32> {
    let sextant = fract(hue) * 6.0;
    let second = chroma * (1.0 - abs(fract(sextant * 0.5) * 2.0 - 1.0));
    var color = vec3<f32>(chroma, second, 0.0);
    if (sextant < 1.0) {
        color = vec3<f32>(chroma, second, 0.0);
    } else if (sextant < 2.0) {
        color = vec3<f32>(second, chroma, 0.0);
    } else if (sextant < 3.0) {
        color = vec3<f32>(0.0, chroma, second);
    } else if (sextant < 4.0) {
        color = vec3<f32>(0.0, second, chroma);
    } else if (sextant < 5.0) {
        color = vec3<f32>(second, 0.0, chroma);
    } else {
        color = vec3<f32>(chroma, 0.0, second);
    }
    return color + vec3<f32>(lightness - chroma * 0.5);
}

fn invert_hue(rgb: vec3<f32>) -> vec3<f32> {
    let extremes = invert_extremes(rgb);
    let chroma = extremes.x - extremes.y;
    if (chroma <= 1.0e-6) { return rgb; }
    let hue = invert_hue_of(rgb, extremes.x, chroma);
    let lightness = (extremes.x + extremes.y) * 0.5;
    return invert_rgb_from_hue_chroma_lightness(hue + 0.5, chroma, lightness);
}

fn invert_lightness(rgb: vec3<f32>) -> vec3<f32> {
    let extremes = invert_extremes(rgb);
    let lightness = (extremes.x + extremes.y) * 0.5;
    let shift = 1.0 - 2.0 * lightness;
    return rgb + vec3<f32>(shift);
}

fn invert_saturation(rgb: vec3<f32>) -> vec3<f32> {
    let extremes = invert_extremes(rgb);
    let chroma = extremes.x - extremes.y;
    if (chroma <= 1.0e-6) { return rgb; }
    let lightness = (extremes.x + extremes.y) * 0.5;
    let range = 1.0 - abs(2.0 * lightness - 1.0);
    if (range <= 1.0e-6 || chroma > range) { return rgb; }
    let scale = (range - chroma) / chroma;
    return vec3<f32>(lightness) + (rgb - vec3<f32>(lightness)) * scale;
}

fn hue_saturation_band_weight(rgb: vec3<f32>, center: f32) -> f32 {
    if (center < 0.0) { return 1.0; }
    let extremes = invert_extremes(rgb);
    let chroma = extremes.x - extremes.y;
    if (!(chroma > 1.0e-6)) { return 0.0; }
    let hue = invert_hue_of(rgb, extremes.x, chroma);
    let difference = fract(hue - center);
    var distance = difference;
    if (difference > 0.5) { distance = 1.0 - difference; }
    return 1.0 - smoothstep(1.0 / 12.0, 1.0 / 6.0, distance);
}

fn hue_saturation_shifted(rgb: vec3<f32>, controls: vec4<f32>) -> vec3<f32> {
    // controls = [hue turns, saturation scale, lightness offset, colorize amount]
    let extremes = invert_extremes(rgb);
    let lightness = (extremes.x + extremes.y) * 0.5 + controls.z;
    let chroma = extremes.x - extremes.y;
    if (!(chroma > 1.0e-6)) { return rgb + vec3<f32>(controls.z); }
    let hue = invert_hue_of(rgb, extremes.x, chroma) + controls.x;
    return invert_rgb_from_hue_chroma_lightness(hue, chroma * controls.y, lightness);
}

fn hue_saturation_tinted(rgb: vec3<f32>, tint: vec4<f32>) -> vec3<f32> {
    // tint = [tint hue turns, tint saturation fraction, tint lightness offset, band center]
    let extremes = invert_extremes(rgb);
    let lightness = (extremes.x + extremes.y) * 0.5 + tint.z;
    let range = max(1.0 - abs(2.0 * lightness - 1.0), 0.0);
    return invert_rgb_from_hue_chroma_lightness(tint.x, range * tint.y, lightness);
}

fn black_and_white_weight_at(index: u32, head: vec4<f32>, tail: vec2<f32>) -> f32 {
    // The six authored stops in hue order: red, yellow, green, cyan, blue, magenta.
    if (index == 0u) { return head.x; }
    if (index == 1u) { return head.y; }
    if (index == 2u) { return head.z; }
    if (index == 3u) { return head.w; }
    if (index == 4u) { return tail.x; }
    return tail.y;
}

fn black_and_white(rgb: vec3<f32>, head: vec4<f32>, tail: vec2<f32>, tint_amount: f32, tint: vec3<f32>) -> vec3<f32> {
    // `BlackAndWhiteGrade::apply`: the per-hue weight scales working-space
    // lightness, then the tint ramps the gray toward the authored color. Pixels
    // without a hue keep their own lightness, and no channel is clamped.
    let extremes = invert_extremes(rgb);
    let chroma = extremes.x - extremes.y;
    let lightness = (extremes.x + extremes.y) * 0.5;
    var gain = 1.0;
    if (chroma > 1.0e-6) {
        let hue = invert_hue_of(rgb, extremes.x, chroma);
        let scaled = fract(hue) * 6.0;
        let base = floor(scaled);
        let index = u32(base) % 6u;
        let next = (index + 1u) % 6u;
        let amount = scaled - base;
        let low = black_and_white_weight_at(index, head, tail);
        let high = black_and_white_weight_at(next, head, tail);
        gain = (low + (high - low) * amount) / 100.0;
    }
    let gray = lightness * gain;
    let tinted = gray + (gray * tint - vec3<f32>(gray)) * tint_amount;
    return tinted;
}

// FLM-02 grain: the hash constants are shared verbatim with the CPU kernel so
// both paths produce bit-identical noise for the same pixel.
const NOISE_SALT_RED: u32 = 0x9E3779B9u;
const NOISE_SALT_GREEN: u32 = 0x85EBCA6Bu;
const NOISE_SALT_BLUE: u32 = 0x27D4EB2Fu;
const NOISE_SALT_LUMINANCE: u32 = 0xC2B2AE35u;

fn noise_signed_grain(x: u32, y: u32, salt: u32) -> f32 {
    var hash = x * 0x9E3779B9u ^ y * 0x85EBCA6Bu ^ salt;
    hash = hash ^ (hash >> 16u);
    hash = hash * 0x7FEB352Du;
    hash = hash ^ (hash >> 15u);
    hash = hash * 0x846CA68Bu;
    hash = hash ^ (hash >> 16u);
    let bits = (hash >> 8u) & 65535u;
    return f32(bits) / 65535.0 * 2.0 - 1.0;
}

// FLM-06 JPEG damage: the quantiser and the per-cell mosaic offset mirror the
// CPU kernel term for term, including the tie-up rounding of `floor(v/d + 0.5)`.
fn jpeg_damage_quantize(value: f32, step: f32) -> f32 {
    return floor(value / step + 0.5) * step;
}

fn jpeg_damage_mosaic(x: u32, y: u32) -> f32 {
    var hash = x * 0x9E3779B9u ^ y * 0x85EBCA6Bu ^ 0x1F83D9ABu;
    hash ^= hash >> 16u;
    hash *= 0x7FEB352Du;
    hash ^= hash >> 15u;
    hash *= 0x846CA68Bu;
    hash ^= hash >> 16u;
    let bits = (hash >> 8u) & 65535u;
    return f32(bits) / 65535.0 * 2.0 - 1.0;
}

// GEN-01 ramp: the scatter jitter mirrors `RampGrade::ramp_unit_noise` and
// `noise_hash` bit for bit so a re-render is identical to the CPU kernel.
const RAMP_DITHER_SALT: u32 = 0x9E3779B9u;

fn ramp_hash(x: u32, y: u32, salt: u32) -> u32 {
    var hash = x * 0x9E3779B9u ^ y * 0x85EBCA6Bu ^ salt;
    hash = hash ^ (hash >> 16u);
    hash = hash * 0x7FEB352Du;
    hash = hash ^ (hash >> 15u);
    hash = hash * 0x846CA68Bu;
    hash = hash ^ (hash >> 16u);
    return hash;
}

fn ramp_unit_noise(x: u32, y: u32) -> f32 {
    return f32(ramp_hash(x, y, RAMP_DITHER_SALT) >> 8u) / 16777216.0;
}

// GEN-02 four-color gradient: the dither hash mirrors
// `four_color_gradient::dither_offset` / `noise::noise_hash` bit for bit, so a
// re-render is identical to the CPU kernel.
const FOUR_COLOR_GRADIENT_DITHER_SALT: u32 = 0x2545F491u;

fn four_color_gradient_hash(x: u32, y: u32, salt: u32) -> u32 {
    var hash = x * 0x9E3779B9u ^ y * 0x85EBCA6Bu ^ salt;
    hash = hash ^ (hash >> 16u);
    hash = hash * 0x7FEB352Du;
    hash = hash ^ (hash >> 15u);
    hash = hash * 0x846CA68Bu;
    hash = hash ^ (hash >> 16u);
    return hash;
}

fn four_color_gradient_noise(x: u32, y: u32) -> f32 {
    let bits = (four_color_gradient_hash(x, y, FOUR_COLOR_GRADIENT_DITHER_SALT) >> 8u) & 65535u;
    return f32(bits) / 65535.0 * 2.0 - 1.0;
}

// GEN-06 cell pattern: the field, the jitter hash and the overflow remap mirror
// `cell_pattern.rs` bit for bit, including the base-cell-local site offsets that
// keep the tiling bit-identical across a period shift. `header.y` is the cell
// type, `header.z` the overflow mode and `header.w` the invert/tiling/cycle
// flags; `params` packs size/offset/disperse, `color` the cell counts, evolution
// and contrast, and `extra0` the seed and the cycle length.
const CELL_PATTERN_EVOLUTION_BASE: u32 = 1048576u;
const CELL_PATTERN_SALT_JITTER: u32 = 0x6A09E667u;
const CELL_PATTERN_SALT_ID: u32 = 0xBB67AE85u;
const CELL_PATTERN_SALT_MIXED: u32 = 0x3C6EF372u;
const CELL_PATTERN_SALT_STRIDE: u32 = 0x9E3779B9u;

struct CellPatternField {
    f1: f32,
    f2: f32,
    winner_x: i32,
    winner_y: i32,
};

fn cell_pattern_hash(x: i32, y: i32, salt: u32) -> u32 {
    var hash = bitcast<u32>(x) * 0x9E3779B9u ^ bitcast<u32>(y) * 0x85EBCA6Bu ^ salt;
    hash = hash ^ (hash >> 16u);
    hash = hash * 0x7FEB352Du;
    hash = hash ^ (hash >> 15u);
    hash = hash * 0x846CA68Bu;
    hash = hash ^ (hash >> 16u);
    return hash;
}

fn cell_pattern_unit16(hash: u32) -> f32 {
    return f32(hash & 65535u) / 65535.0;
}

fn cell_pattern_seed_mix(seed: f32) -> u32 {
    return u32(floor(clamp(seed, 0.0, 100000.0))) * 0x9E3779B9u;
}

fn cell_pattern_jitter_at(x: i32, y: i32, salt: u32, step: u32) -> vec2<f32> {
    let hash = cell_pattern_hash(x, y, salt ^ (step * CELL_PATTERN_SALT_STRIDE));
    return vec2<f32>(
        cell_pattern_unit16(hash) * 2.0 - 1.0,
        cell_pattern_unit16(hash >> 16u) * 2.0 - 1.0,
    );
}

fn cell_pattern_jitter(x: i32, y: i32, salt: u32, step: u32, weight: f32) -> vec2<f32> {
    let first = cell_pattern_jitter_at(x, y, salt, step);
    if (weight <= 0.0) { return first; }
    let next = cell_pattern_jitter_at(x, y, salt, step + 1u);
    return first + (next - first) * weight;
}

fn cell_pattern_wrap(index: i32, period: i32) -> i32 {
    if (period <= 0) { return index; }
    var wrapped = index % period;
    if (wrapped < 0) { wrapped = wrapped + period; }
    return wrapped;
}

fn cell_pattern_field(
    q: vec2<f32>,
    frequency: f32,
    metric: u32,
    step: u32,
    weight: f32,
    jitter_salt: u32,
    cells_h: f32,
    cells_v: f32,
    tiling: bool,
    disperse: f32,
) -> CellPatternField {
    let base_x = i32(clamp(floor(q.x), -8388608.0, 8388607.0));
    let base_y = i32(clamp(floor(q.y), -8388608.0, 8388607.0));
    let local = q - vec2<f32>(f32(base_x), f32(base_y));
    var period_x = 0;
    var period_y = 0;
    if (tiling) {
        period_x = i32(max(round(cells_h * frequency), 1.0));
        period_y = i32(max(round(cells_v * frequency), 1.0));
    }
    var f1 = 1.0e9;
    var f2 = 1.0e9;
    var winner_x = base_x;
    var winner_y = base_y;
    for (var oy = -1; oy <= 1; oy = oy + 1) {
        for (var ox = -1; ox <= 1; ox = ox + 1) {
            let cell_x = base_x + ox;
            let cell_y = base_y + oy;
            var hash_x = cell_x;
            var hash_y = cell_y;
            if (tiling) {
                hash_x = cell_pattern_wrap(cell_x, period_x);
                hash_y = cell_pattern_wrap(cell_y, period_y);
            }
            let jitter = cell_pattern_jitter(hash_x, hash_y, jitter_salt, step, weight);
            let dx = local.x - (f32(ox) + 0.5 + disperse * jitter.x);
            let dy = local.y - (f32(oy) + 0.5 + disperse * jitter.y);
            var distance = sqrt(dx * dx + dy * dy);
            if (metric == 1u) { distance = abs(dx) + abs(dy); }
            if (distance < f1) {
                f2 = f1;
                f1 = distance;
                winner_x = hash_x;
                winner_y = hash_y;
            } else if (distance < f2) {
                f2 = distance;
            }
        }
    }
    return CellPatternField(f1, f2, winner_x, winner_y);
}

fn cell_pattern_luminance(x: i32, y: i32, seed_salt: u32, step: u32) -> f32 {
    return cell_pattern_unit16(
        cell_pattern_hash(
            x,
            y,
            CELL_PATTERN_SALT_ID ^ seed_salt ^ (step * CELL_PATTERN_SALT_STRIDE),
        ),
    );
}

fn cell_pattern_overflow(value: f32, mode: u32) -> f32 {
    if (mode == 1u) {
        if (value < 0.0) {
            let distance = -value;
            return distance / (1.0 + distance);
        }
        if (value > 1.0) {
            let distance = value - 1.0;
            return 1.0 - distance / (1.0 + distance);
        }
        return value;
    }
    if (mode == 2u) {
        let folded = value - 2.0 * floor(value * 0.5);
        return clamp(select(2.0 - folded, folded, folded <= 1.0), 0.0, 1.0);
    }
    return clamp(value, 0.0, 1.0);
}

fn cell_pattern_bubbles(
    q: vec2<f32>,
    frequency: f32,
    metric: u32,
    step: u32,
    weight: f32,
    seed_salt: u32,
    cells_h: f32,
    cells_v: f32,
    tiling: bool,
    disperse: f32,
    scale: f32,
    gain: f32,
    mode: u32,
) -> f32 {
    let field = cell_pattern_field(
        q,
        frequency,
        metric,
        step,
        weight,
        CELL_PATTERN_SALT_JITTER ^ seed_salt,
        cells_h,
        cells_v,
        tiling,
        disperse,
    );
    let luminance = cell_pattern_luminance(field.winner_x, field.winner_y, seed_salt, step);
    let raw = 0.5 + (0.55 + 0.30 * luminance - field.f1) * scale * gain;
    return cell_pattern_overflow(raw, mode);
}

fn cell_pattern_plates(
    q: vec2<f32>,
    frequency: f32,
    step: u32,
    weight: f32,
    seed_salt: u32,
    cells_h: f32,
    cells_v: f32,
    tiling: bool,
    disperse: f32,
    ramp: f32,
    frozen: bool,
) -> f32 {
    let field = cell_pattern_field(
        q,
        frequency,
        2u,
        step,
        weight,
        CELL_PATTERN_SALT_JITTER ^ seed_salt,
        cells_h,
        cells_v,
        tiling,
        disperse,
    );
    var luminance = cell_pattern_luminance(field.winner_x, field.winner_y, seed_salt, step);
    if (frozen) {
        luminance = cell_pattern_unit16(cell_pattern_hash(
            field.winner_x,
            field.winner_y,
            CELL_PATTERN_SALT_ID ^ seed_salt,
        ));
    }
    return luminance * clamp((field.f2 - field.f1) / ramp, 0.0, 1.0);
}

fn cell_pattern_crystallize(
    q: vec2<f32>,
    frequency: f32,
    step: u32,
    weight: f32,
    seed_salt: u32,
    cells_h: f32,
    cells_v: f32,
    tiling: bool,
    disperse: f32,
    edge_width: f32,
    frozen: bool,
) -> f32 {
    let field = cell_pattern_field(
        q,
        frequency,
        2u,
        step,
        weight,
        CELL_PATTERN_SALT_JITTER ^ seed_salt,
        cells_h,
        cells_v,
        tiling,
        disperse,
    );
    if (field.f2 - field.f1 > edge_width) {
        if (frozen) {
            return cell_pattern_unit16(cell_pattern_hash(
                field.winner_x,
                field.winner_y,
                CELL_PATTERN_SALT_ID ^ seed_salt,
            ));
        }
        return cell_pattern_luminance(field.winner_x, field.winner_y, seed_salt, step);
    }
    return 0.0;
}

fn cell_pattern_mixed(
    q: vec2<f32>,
    step: u32,
    weight: f32,
    seed_salt: u32,
    cells_h: f32,
    cells_v: f32,
    tiling: bool,
    disperse: f32,
    ramp: f32,
    gain: f32,
    mode: u32,
) -> f32 {
    let plates = cell_pattern_field(
        q,
        1.0,
        2u,
        step,
        weight,
        CELL_PATTERN_SALT_JITTER ^ seed_salt,
        cells_h,
        cells_v,
        tiling,
        disperse,
    );
    let crystals = cell_pattern_field(
        q,
        1.0,
        1u,
        step,
        weight,
        CELL_PATTERN_SALT_JITTER ^ seed_salt,
        cells_h,
        cells_v,
        tiling,
        disperse,
    );
    let pick = (cell_pattern_hash(
        plates.winner_x,
        plates.winner_y,
        CELL_PATTERN_SALT_MIXED ^ seed_salt,
    ) >> 16u) & 1u;
    if (pick == 1u) {
        let luminance = cell_pattern_luminance(crystals.winner_x, crystals.winner_y, seed_salt, step);
        return cell_pattern_overflow(
            0.5 + (0.55 + 0.30 * luminance - crystals.f1) * 6.0 * gain,
            mode,
        );
    }
    let luminance = cell_pattern_luminance(plates.winner_x, plates.winner_y, seed_salt, step);
    return cell_pattern_overflow(
        luminance * clamp((plates.f2 - plates.f1) / ramp, 0.0, 1.0),
        mode,
    );
}

fn cell_pattern_value(effect: EffectUniform, centre: vec2<f32>) -> f32 {
    let kind = effect.header.y;
    let mode = effect.header.z;
    let flags = effect.header.w;
    let invert = (flags & 1u) != 0u;
    let tiling = (flags & 2u) != 0u;
    let cycle = (flags & 4u) != 0u;
    let size = effect.params.x;
    let offset = effect.params.yz;
    let disperse = effect.params.w;
    let cells_h = effect.color.x;
    let cells_v = effect.color.y;
    let evolution = effect.color.z;
    let contrast = effect.color.w;
    let seed = effect.extra0.x;
    let revolutions = effect.extra0.y;
    let gain = contrast * 0.01;
    let edge_width = 8.0 / max(contrast, 1.0);
    let ramp = min(edge_width, 0.5);
    var step = CELL_PATTERN_EVOLUTION_BASE;
    var weight = 0.0;
    // Indices 3 (static plates) and 8 (static plates HQ) freeze the whole field.
    if (kind != 3u && kind != 8u) {
        var phase = evolution;
        if (cycle) { phase = phase - revolutions * floor(phase / revolutions); }
        phase = clamp(phase, -1048576.0, 1048576.0);
        let index = floor(phase);
        let fraction = phase - index;
        weight = fraction * fraction * (3.0 - 2.0 * fraction);
        step = u32(index + 1048576.0);
    }
    let seed_salt = cell_pattern_seed_mix(seed);
    let p = (centre - offset) / size;
    let second = p * 2.0 + vec2<f32>(0.5);
    var shaped = 0.0;
    if (kind == 0u) {
        shaped = cell_pattern_bubbles(
            p, 1.0, 2u, step, weight, seed_salt, cells_h, cells_v, tiling, disperse, 4.0, gain, mode,
        );
    } else if (kind == 1u) {
        shaped = cell_pattern_bubbles(
            p, 1.0, 1u, step, weight, seed_salt, cells_h, cells_v, tiling, disperse, 6.0, gain, mode,
        );
    } else if (kind == 2u) {
        shaped = clamp(cell_pattern_plates(
            p, 1.0, step, weight, seed_salt, cells_h, cells_v, tiling, disperse, ramp, false,
        ), 0.0, 1.0);
    } else if (kind == 3u) {
        shaped = clamp(cell_pattern_plates(
            p, 1.0, step, weight, seed_salt, cells_h, cells_v, tiling, disperse, ramp, true,
        ), 0.0, 1.0);
    } else if (kind == 4u) {
        shaped = clamp(cell_pattern_crystallize(
            p, 1.0, step, weight, seed_salt, cells_h, cells_v, tiling, disperse, edge_width, false,
        ), 0.0, 1.0);
    } else if (kind == 5u) {
        let field = cell_pattern_field(
            p,
            1.0,
            2u,
            step,
            weight,
            CELL_PATTERN_SALT_JITTER ^ seed_salt,
            cells_h,
            cells_v,
            tiling,
            disperse,
        );
        shaped = cell_pattern_overflow(0.5 + (0.62 - field.f1 * field.f1) * 2.5 * gain, mode);
    } else if (kind == 6u) {
        let first = cell_pattern_bubbles(
            p, 1.0, 1u, step, weight, seed_salt, cells_h, cells_v, tiling, disperse, 6.0, gain, mode,
        );
        let next = cell_pattern_bubbles(
            second, 2.0, 1u, step, weight, seed_salt, cells_h, cells_v, tiling, disperse, 6.0, gain, mode,
        );
        shaped = 0.65 * first + 0.35 * next;
    } else if (kind == 7u) {
        let first = cell_pattern_plates(
            p, 1.0, step, weight, seed_salt, cells_h, cells_v, tiling, disperse, ramp, false,
        );
        let next = cell_pattern_plates(
            second, 2.0, step, weight, seed_salt, cells_h, cells_v, tiling, disperse, ramp, false,
        );
        shaped = clamp(0.65 * first + 0.35 * next, 0.0, 1.0);
    } else if (kind == 8u) {
        let first = cell_pattern_plates(
            p, 1.0, step, weight, seed_salt, cells_h, cells_v, tiling, disperse, ramp, true,
        );
        let next = cell_pattern_plates(
            second, 2.0, step, weight, seed_salt, cells_h, cells_v, tiling, disperse, ramp, true,
        );
        shaped = clamp(0.65 * first + 0.35 * next, 0.0, 1.0);
    } else if (kind == 9u) {
        let first = cell_pattern_crystallize(
            p, 1.0, step, weight, seed_salt, cells_h, cells_v, tiling, disperse, edge_width, false,
        );
        let next = cell_pattern_crystallize(
            second, 2.0, step, weight, seed_salt, cells_h, cells_v, tiling, disperse, edge_width, false,
        );
        shaped = clamp(0.65 * first + 0.35 * next, 0.0, 1.0);
    } else if (kind == 10u) {
        shaped = cell_pattern_mixed(
            p, step, weight, seed_salt, cells_h, cells_v, tiling, disperse, ramp, gain, mode,
        );
    } else {
        let field = cell_pattern_field(
            p,
            1.0,
            2u,
            step,
            weight,
            CELL_PATTERN_SALT_JITTER ^ seed_salt,
            cells_h,
            cells_v,
            tiling,
            disperse,
        );
        let gap = field.f2 - field.f1;
        shaped = cell_pattern_overflow(0.5 + (0.40 - abs(gap - 0.18) * 5.0) * gain, mode);
    }
    if (invert) { shaped = 1.0 - shaped; }
    return clamp(shaped, 0.0, 1.0);
}

struct FractalEscape {
    escaped: bool,
    iterations: u32,
    fraction: f32,
    last_z: vec2<f32>,
};

fn fractal_complex_mul(a: vec2<f32>, b: vec2<f32>) -> vec2<f32> {
    // Keep the two products in their own locals: DXC may otherwise contract
    // `a * b - c * d` into an fma, which would move the escape fraction by
    // one ulp relative to the Rust kernel's fixed multiply/subtract order.
    let real_left = a.x * b.x;
    let real_right = a.y * b.y;
    let imag_left = a.x * b.y;
    let imag_right = a.y * b.x;
    return vec2<f32>(real_left - real_right, imag_left + imag_right);
}

fn fractal_complex_add(a: vec2<f32>, b: vec2<f32>) -> vec2<f32> {
    return a + b;
}

// The one reciprocal in the fractal shader, mirroring the Rust
// `apply_inversion` exactly. Inverse modes funnel through this function.
fn fractal_inversion(value: vec2<f32>, post: vec2<f32>) -> vec2<f32> {
    let r2_real = value.x * value.x;
    let r2_imag = value.y * value.y;
    let r2 = r2_real + r2_imag;
    let r2_safe = max(r2, 1.0e-30);
    return vec2<f32>(value.x / r2_safe + post.x, -(value.y / r2_safe) + post.y);
}

fn fractal_hue_rgb(hue: f32) -> vec3<f32> {
    let t = (hue - floor(hue)) * 6.0;
    return vec3<f32>(
        clamp(abs(t - 3.0) - 1.0, 0.0, 1.0),
        clamp(2.0 - abs(t - 2.0), 0.0, 1.0),
        clamp(2.0 - abs(t - 4.0), 0.0, 1.0),
    );
}

fn fractal_atan_turns(y: f32, x: f32) -> f32 {
    let ax = abs(x);
    let ay = abs(y);
    let steep = ay > ax;
    let t = select(
        ay / max(ax, 1.0e-30),
        ax / max(ay, 1.0e-30),
        steep,
    );
    let a = t * (0.125 + 0.04345 * (1.0 - t));
    let quarter = select(a, 0.25 - a, steep);
    let half = select(quarter, 1.0 - quarter, y < 0.0);
    let turn = select(half, 0.5 - half, x < 0.0);
    return turn - floor(turn);
}

fn fractal_escape(
    constant: vec2<f32>,
    start: vec2<f32>,
    equation: u32,
    max_iterations: u32,
) -> FractalEscape {
    // The bailout is derived from the constant itself, so `|z| == bailout` is a
    // common exact tie: the first iterate is the constant. A few ulp of
    // headroom keeps that tie from being decided by whether DXC fuses one of
    // the two squared-norm sums. Must match `fractal.rs` exactly.
    let bailout_real = constant.x * constant.x;
    let bailout_imag = constant.y * constant.y;
    let bailout_r2 = max(bailout_real + bailout_imag, 4.0) * 1.000001;
    var z = start;
    var last_r2 = 0.0;
    var iterations = 0u;
    var fraction = 0.0;
    var escaped = false;
    for (var step = 0u; step < max_iterations; step = step + 1u) {
        let r2_real = z.x * z.x;
        let r2_imag = z.y * z.y;
        let r2 = r2_real + r2_imag;
        if (r2 > bailout_r2) {
            fraction = clamp(
                (r2 - bailout_r2) / max(r2 - last_r2, 1.0e-30),
                0.0,
                1.0,
            );
            escaped = true;
            break;
        }
        last_r2 = r2;
        let squared = fractal_complex_mul(z, z);
        var next = squared;
        if (equation == 2u) {
            next = fractal_complex_mul(squared, z);
        } else if (equation == 3u) {
            let fourth = fractal_complex_mul(squared, squared);
            next = fractal_complex_mul(fourth, z);
        } else if (equation == 4u) {
            let fourth = fractal_complex_mul(squared, squared);
            let sixth = fractal_complex_mul(fourth, squared);
            next = fractal_complex_mul(sixth, z);
        }
        z = fractal_complex_add(next, constant);
        iterations = iterations + 1u;
    }
    if (!escaped) {
        iterations = max_iterations;
    }
    return FractalEscape(escaped, iterations, fraction, z);
}

fn fractal_palette_rgb(
    palette: u32,
    w: f32,
    band: f32,
    hue_turns: f32,
    last_z: vec2<f32>,
    cycle_steps: f32,
    edge_highlight: f32,
) -> vec3<f32> {
    if (palette == 4u) {
        return fractal_hue_rgb(hue_turns);
    }
    let k = hue_turns + band * (1.0 / cycle_steps);
    var rgb = vec3<f32>(0.0);
    if (palette == 1u) {
        rgb = vec3<f32>(w) * (vec3<f32>(0.5) + 0.5 * fractal_hue_rgb(k));
    } else if (palette == 2u) {
        rgb = fractal_hue_rgb(k);
    } else if (palette == 3u) {
        let value = band - 2.0 * floor(band * 0.5);
        rgb = vec3<f32>(value);
    } else if (palette == 5u) {
        rgb = fractal_hue_rgb(fractal_atan_turns(last_z.y, last_z.x) + hue_turns);
    } else if (palette == 6u) {
        rgb = vec3<f32>(1.0 - abs(2.0 * w - 1.0));
    } else {
        let q = min(floor(w * 4.0), 3.0);
        let c0 = vec3<f32>(0.15, 0.85, 0.25);
        let c1 = vec3<f32>(0.85, 0.30, 0.85);
        let c2 = vec3<f32>(0.95, 0.95, 0.95);
        let c3 = vec3<f32>(0.95, 0.55, 0.10);
        rgb = c0;
        if (q > 0.5) { rgb = c1; }
        if (q > 1.5) { rgb = c2; }
        if (q > 2.5) { rgb = c3; }
    }
    if (edge_highlight > 0.0) {
        let distance = min(w, 1.0 - w);
        let width = edge_highlight * 0.05;
        let edge = clamp((width - distance) / max(width, 0.02), 0.0, 1.0);
        rgb = rgb + (vec3<f32>(1.0) - rgb) * edge;
    }
    return rgb;
}

fn fractal_point(effect: EffectUniform, x: f32, y: f32) -> vec4<f32> {
    let set_choice = effect.header.y;
    let equation = effect.header.z;
    let iterations = effect.header.w;
    let inverse_iterations = 1.0 / f32(iterations);
    let width = max(uniforms.geometry.z, 1.0);
    let height = max(uniforms.geometry.w, 1.0);
    let half = max(min(width, height) * 0.5, 1.0);
    let mandelbrot_family = set_choice <= 4u;
    let julia = vec2<f32>(effect.params.w, effect.color.x);
    let centre = select(julia, effect.params.xy, mandelbrot_family);
    let inverse = (set_choice == 2u) || (set_choice == 4u) || (set_choice == 6u);
    let starts_at_julia = (set_choice == 3u) || (set_choice == 4u);
    let post = effect.color.zw;
    let view = vec2<f32>(
        centre.x + ((x - width * 0.5) / half) * effect.extra2.y,
        centre.y + ((y - height * 0.5) / half) * effect.extra2.y,
    );

    var constant = view;
    var start = vec2<f32>(0.0);
    if (mandelbrot_family) {
        if (inverse) { constant = fractal_inversion(view, post); }
        if (starts_at_julia) { start = julia; }
    } else {
        constant = julia;
        start = select(view, fractal_inversion(view, post), inverse);
    }

    let field = fractal_escape(constant, start, equation, iterations);
    let inside = !field.escaped;
    let u = select(1.0, (f32(field.iterations) + field.fraction) * inverse_iterations, field.escaped);
    let hue_turns = effect.extra0.w * (1.0 / 360.0);
    let palette = u32(effect.extra0.z);
    var rgb = vec3<f32>(0.0);
    var alpha = 1.0;
    if (palette == 4u) {
        let visible = select(inside, !inside, effect.extra0.y > 0.5);
        rgb = fractal_hue_rgb(hue_turns);
        alpha = select(0.0, 1.0, visible);
    } else {
        if (!inside) {
            let phase = u * effect.extra1.x + effect.extra1.y * (1.0 / 360.0);
            let band = floor(phase);
            let w = phase - band;
            rgb = fractal_palette_rgb(
                palette,
                w,
                band,
                hue_turns,
                field.last_z,
                effect.extra1.x,
                effect.extra1.z,
            );
        }
        if (effect.extra0.y > 0.5 && inside) {
            alpha = 0.0;
        }
    }

    if (effect.extra0.x > 0.0 && !inside) {
        let ghost_iterations = max(16u, iterations / 4u);
        var ghost_constant = view;
        var ghost_start = vec2<f32>(0.0);
        if (mandelbrot_family) {
            ghost_constant = julia;
            ghost_start = view;
        }
        let ghost = fractal_escape(ghost_constant, ghost_start, equation, ghost_iterations);
        let ghost_inside = !ghost.escaped;
        let ghost_u = select(
            1.0,
            (f32(ghost.iterations) + ghost.fraction) * (1.0 / f32(ghost_iterations)),
            ghost.escaped,
        );
        var ghost_rgb = vec3<f32>(0.0);
        if (!ghost_inside) {
            let phase = ghost_u * effect.extra1.x + effect.extra1.y * (1.0 / 360.0);
            let band = floor(phase);
            let w = phase - band;
            ghost_rgb = fractal_palette_rgb(
                palette,
                w,
                band,
                hue_turns,
                ghost.last_z,
                effect.extra1.x,
                0.0,
            );
        }
        rgb = max(rgb, ghost_rgb * effect.extra0.x);
    }
    return vec4<f32>(rgb, alpha);
}

fn fractal_value(effect: EffectUniform, origin: vec2<f32>) -> vec4<f32> {
    let side = u32(effect.extra2.x);
    if (side <= 1u) {
        return fractal_point(effect, origin.x + 0.5, origin.y + 0.5);
    }
    let step = 1.0 / f32(side);
    var sum = vec4<f32>(0.0);
    for (var sub_y = 0u; sub_y < side; sub_y = sub_y + 1u) {
        for (var sub_x = 0u; sub_x < side; sub_x = sub_x + 1u) {
            let x = origin.x + (f32(sub_x) + 0.5) * step;
            let y = origin.y + (f32(sub_y) + 0.5) * step;
            sum = sum + fractal_point(effect, x, y);
        }
    }
    return sum * (1.0 / f32(side * side));
}

// `ExtractGrade`: params.x is the channel, params.y the black point,
// params.z the white point and params.w the invert toggle; extra0.xy are the
// softness around each point.
fn extract_smoothstep(ratio: f32) -> f32 {
    let clamped = clamp(ratio, 0.0, 1.0);
    return clamped * clamped * (3.0 - 2.0 * clamped);
}

fn extract_matte(value: f32, effect: EffectUniform) -> f32 {
    let black = effect.params.y;
    let white = effect.params.z;
    let black_softness = effect.extra0.x;
    let white_softness = effect.extra0.y;
    let lower = select(
        select(0.0, 1.0, value >= black),
        extract_smoothstep((value - (black - black_softness)) / (2.0 * black_softness)),
        black_softness > 1.0e-6
    );
    let upper = select(
        select(1.0, 0.0, value > white),
        1.0 - extract_smoothstep((value - (white - white_softness)) / (2.0 * white_softness)),
        white_softness > 1.0e-6
    );
    let matte = clamp(lower * upper, 0.0, 1.0);
    return select(matte, 1.0 - matte, effect.params.w > 0.5);
}

fn levels_map(value: f32, effect: EffectUniform) -> f32 {
    let input_black = effect.params.x;
    let input_white = effect.params.y;
    let gamma = effect.params.z;
    let output_black = effect.params.w;
    let output_white = effect.extra0.x;
    let span = input_white - input_black;
    // A degenerate input range is a hard step at the black point, exactly as the
    // CPU kernel documents it. Both arms of the select are evaluated, so the
    // divisor is guarded instead of the division.
    let degenerate = abs(span) <= 0.0;
    let normalized = (value - input_black) / select(span, 1.0, degenerate);
    let stepped = select(0.0, 1.0, value > input_black);
    let base = select(normalized, stepped, degenerate);
    // Gamma above one brightens, so the exponent is its reciprocal.
    let shaped = pow(max(base, 0.0), 1.0 / gamma);
    let mapped = output_black + shaped * (output_white - output_black);
    // The two clipping menus: 1 is off, 2 clamps into the output range and 3
    // clamps into the display range. Each end of the output range is authored
    // by its own menu.
    let black_clip = effect.extra0.z;
    let low = select(
        select(min(output_black, output_white), 0.0, black_clip > 2.5),
        -3.4028235e38,
        black_clip < 1.5
    );
    let white_clip = effect.extra0.w;
    let high = select(
        select(max(output_black, output_white), 1.0, white_clip > 2.5),
        3.4028235e38,
        white_clip < 1.5
    );
    return clamp(mapped, low, high);
}

fn arithmetic_quantise(sample: f32) -> i32 {
    // The CPU kernel reads the sample as an eight-bit value, so the sample is
    // first clamped into the display range and then rounded half away from zero,
    // matching `f32::round` on the CPU side.
    return i32(floor(clamp(sample, 0.0, 1.0) * 255.0 + 0.5));
}

fn arithmetic_combine(channel: f32, value: f32, operation: i32, clip_result: bool) -> f32 {
    // The thirteen operators are numbered like the menu: 0 and, 1 or, 2 xor,
    // 3 add, 4 subtract, 5 multiply, 6 difference, 7 min, 8 max, 9 block above,
    // 10 block below, 11 slice, 12 screen. The bitwise operators combine the
    // eight-bit readings, the rest combine the float samples, exactly like the
    // CPU grade.
    let k = arithmetic_quantise(channel);
    let v = arithmetic_quantise(value);
    var result = channel;
    switch operation {
        case 0: {
            result = f32(k & v) / 255.0;
        }
        case 1: {
            result = f32(k | v) / 255.0;
        }
        case 2: {
            result = f32(k ^ v) / 255.0;
        }
        case 3: {
            result = channel + value;
        }
        case 4: {
            result = channel - value;
        }
        case 5: {
            result = channel * value;
        }
        case 6: {
            result = abs(channel - value);
        }
        case 7: {
            result = min(channel, value);
        }
        case 8: {
            result = max(channel, value);
        }
        case 9: {
            result = select(channel, 0.0, channel > value);
        }
        case 10: {
            result = select(channel, 0.0, channel < value);
        }
        case 11: {
            result = select(0.0, 1.0, channel > value);
        }
        default: {
            result = 1.0 - (1.0 - channel) * (1.0 - value);
        }
    }
    if clip_result {
        result = clamp(result, 0.0, 1.0);
    }
    return result;
}

fn leave_color_hue(colour: vec3<f32>) -> f32 {
    // `LeaveColorMatch::Hue`: the maximum/minimum difference expression the CPU
    // kernel uses, written so both sides evaluate the same sectors. A grey
    // sample has no hue; the red arm wraps a negative sector into the top of the
    // circle instead of taking a remainder.
    let maximum = max(colour.r, max(colour.g, colour.b));
    let minimum = min(colour.r, min(colour.g, colour.b));
    let delta = maximum - minimum;
    if (delta == 0.0) {
        return 0.0;
    }
    if (maximum == colour.r) {
        let sector = (colour.g - colour.b) / delta;
        if (sector < 0.0) {
            return (sector + 6.0) / 6.0;
        }
        return sector / 6.0;
    }
    if (maximum == colour.g) {
        return ((colour.b - colour.r) / delta + 2.0) / 6.0;
    }
    return ((colour.r - colour.g) / delta + 4.0) / 6.0;
}

fn change_to_color_chroma_limit(colour: vec3<f32>) -> f32 {
    // The same `max(r, g, b, 1e-4)` normaliser the CPU kernel divides by, so a
    // black sample cannot produce a division by zero.
    return max(max(colour.r, colour.g), max(colour.b, 0.0001));
}

fn change_to_color_normalised_chroma(colour: vec3<f32>) -> vec3<f32> {
    return colour / vec3<f32>(change_to_color_chroma_limit(colour));
}

fn change_to_color_hue(colour: vec3<f32>) -> f32 {
    // Copied from `leave_color_hue` on purpose: the two point effects must agree
    // at the red boundary, and sharing the helper would tie two independently
    // authored readings together.
    let maximum = max(colour.r, max(colour.g, colour.b));
    let minimum = min(colour.r, min(colour.g, colour.b));
    let delta = maximum - minimum;
    if (delta == 0.0) {
        return 0.0;
    }
    if (maximum == colour.r) {
        let sector = (colour.g - colour.b) / delta;
        if (sector < 0.0) {
            return (sector + 6.0) / 6.0;
        }
        return sector / 6.0;
    }
    if (maximum == colour.g) {
        return ((colour.b - colour.r) / delta + 2.0) / 6.0;
    }
    return ((colour.r - colour.g) / delta + 4.0) / 6.0;
}

fn change_to_color_saturation(colour: vec3<f32>) -> f32 {
    let maximum = max(colour.r, max(colour.g, colour.b));
    let minimum = min(colour.r, min(colour.g, colour.b));
    return (maximum - minimum) / max(maximum, 0.0001);
}

fn change_to_color_lightness(colour: vec3<f32>) -> f32 {
    return max(colour.r, max(colour.g, colour.b));
}

fn change_to_color_wrap(value: f32) -> f32 {
    // Shortest signed arc: `x - floor(x + 0.5)` lands in `-0.5..=0.5`.
    return value - floor(value + 0.5);
}

fn change_to_color_fract(value: f32) -> f32 {
    return value - floor(value);
}

fn change_to_color_hsv_to_rgb(hue: f32, saturation: f32, lightness: f32) -> vec3<f32> {
    // The same six sectors `hsv_channel` spells as a match, unrolled into the
    // five constant comparisons the shader is allowed to use.
    if (saturation <= 0.0) {
        return vec3<f32>(lightness);
    }
    let sextant = change_to_color_fract(hue) * 6.0;
    let chroma = lightness * saturation;
    let x = chroma * (1.0 - abs(sextant - 2.0 * floor(sextant * 0.5) - 1.0));
    // Sectors 0 and 1 rise from red to yellow and yellow to green; sectors 2
    // and 3 fall from green to cyan and cyan to blue. Each adjacent pair is one
    // select over the same two candidates. `select(f, t, cond)` takes `t` when
    // the condition holds, so the operands are ordered "later sector first".
    let rising = select(
        vec3<f32>(x, chroma, 0.0),
        vec3<f32>(chroma, x, 0.0),
        sextant < 1.0,
    );
    let falling = select(
        vec3<f32>(0.0, x, chroma),
        vec3<f32>(0.0, chroma, x),
        sextant < 3.0,
    );
    // The low half spans sectors 0..3, the high half 4..5.
    let low = select(falling, rising, sextant < 2.0);
    let high = select(
        vec3<f32>(chroma, 0.0, x),
        vec3<f32>(x, 0.0, chroma),
        sextant < 5.0,
    );
    let base = select(high, low, sextant < 4.0);
    return base + vec3<f32>(lightness - chroma);
}

fn change_to_color_weight(colour: vec3<f32>, effect: EffectUniform) -> f32 {
    // `d = ||ĉ − k̂|| / √3` against the chroma-normalised source, then the
    // Hermite ramp from one at the source to zero past the authored softness.
    let sample = change_to_color_normalised_chroma(colour);
    let source = change_to_color_normalised_chroma(effect.extra0.xyz);
    let difference = sample - source;
    let distance = sqrt(dot(difference, difference)) * 0.5773503;
    let edge = max(effect.params.w, 0.0) + 0.0001;
    let t = clamp(distance / edge, 0.0, 1.0);
    return 1.0 - t * t * (3.0 - 2.0 * t);
}

// COL-20 colour map helpers. The five palette stops are packed across the
// colour and extra cells; `colorama_stop` is the one place that knows the lane
// layout and mirrors the Rust `ColoramaGpuControls` packing one for one.
fn colorama_stop(index: u32, effect: EffectUniform) -> vec3<f32> {
    if (index == 0u) {
        return vec3<f32>(effect.extra1.x, effect.extra1.y, effect.extra1.z);
    }
    if (index == 1u) {
        return vec3<f32>(effect.extra1.w, effect.extra2.x, effect.extra2.y);
    }
    if (index == 2u) {
        return vec3<f32>(effect.extra2.z, effect.extra2.w, effect.extra3.x);
    }
    if (index == 3u) {
        return vec3<f32>(effect.extra3.y, effect.extra3.z, effect.extra3.w);
    }
    return vec3<f32>(effect.color.w, effect.extra0.z, effect.extra0.w);
}

fn colorama_mix(left: f32, right: f32, weight: f32) -> f32 {
    return left * (1.0 - weight) + right * weight;
}

fn colorama_hue(rgb: vec3<f32>) -> f32 {
    let maximum = max(rgb.x, max(rgb.y, rgb.z));
    let minimum = min(rgb.x, min(rgb.y, rgb.z));
    let delta = maximum - minimum;
    if (delta == 0.0) {
        return 0.0;
    }
    if (maximum == rgb.x) {
        let sector = (rgb.y - rgb.z) / delta;
        if (sector < 0.0) {
            return (sector + 6.0) / 6.0;
        }
        return sector / 6.0;
    }
    if (maximum == rgb.y) {
        return ((rgb.z - rgb.x) / delta + 2.0) / 6.0;
    }
    return ((rgb.x - rgb.y) / delta + 4.0) / 6.0;
}

fn colorama_saturation(rgb: vec3<f32>) -> f32 {
    let maximum = max(rgb.x, max(rgb.y, rgb.z));
    let minimum = min(rgb.x, min(rgb.y, rgb.z));
    if (maximum > 0.0) {
        return 1.0 - minimum / maximum;
    }
    return 0.0;
}

fn colorama_lightness(rgb: vec3<f32>) -> f32 {
    let maximum = max(rgb.x, max(rgb.y, rgb.z));
    let minimum = min(rgb.x, min(rgb.y, rgb.z));
    return (maximum + minimum) * 0.5;
}

fn colorama_hsv_to_rgb(hue: f32, saturation: f32, value: f32) -> vec3<f32> {
    if (saturation <= 0.0) {
        return vec3<f32>(value, value, value);
    }
    let h6 = fract(hue) * 6.0;
    let sector = floor(h6);
    let f = h6 - sector;
    let p = value * (1.0 - saturation);
    let q = value * (1.0 - saturation * f);
    let t = value * (1.0 - saturation * (1.0 - f));
    if (sector < 1.0) {
        return vec3<f32>(value, t, p);
    }
    if (sector < 2.0) {
        return vec3<f32>(q, value, p);
    }
    if (sector < 3.0) {
        return vec3<f32>(p, value, t);
    }
    if (sector < 4.0) {
        return vec3<f32>(p, q, value);
    }
    if (sector < 5.0) {
        return vec3<f32>(t, p, value);
    }
    return vec3<f32>(value, p, q);
}

fn colorama_hsl_to_rgb(hue: f32, saturation: f32, lightness: f32) -> vec3<f32> {
    if (saturation <= 0.0) {
        return vec3<f32>(lightness, lightness, lightness);
    }
    let chroma = (1.0 - abs(2.0 * lightness - 1.0)) * saturation;
    let h6 = fract(hue) * 6.0;
    let sector = floor(h6);
    let second = chroma * (1.0 - abs(h6 - 2.0 * floor(h6 * 0.5) - 1.0));
    var rgb = vec3<f32>(0.0, 0.0, 0.0);
    if (sector < 1.0) {
        rgb = vec3<f32>(chroma, second, 0.0);
    } else if (sector < 2.0) {
        rgb = vec3<f32>(second, chroma, 0.0);
    } else if (sector < 3.0) {
        rgb = vec3<f32>(0.0, chroma, second);
    } else if (sector < 4.0) {
        rgb = vec3<f32>(0.0, second, chroma);
    } else if (sector < 5.0) {
        rgb = vec3<f32>(second, 0.0, chroma);
    } else {
        rgb = vec3<f32>(chroma, 0.0, second);
    }
    let base = lightness - chroma * 0.5;
    return rgb + vec3<f32>(base, base, base);
}

fn colorama_phase(pixel: vec4<f32>, source: u32) -> f32 {
    if (source == 0u) {
        return dot(pixel.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
    }
    if (source == 1u) {
        return pixel.r;
    }
    if (source == 2u) {
        return pixel.g;
    }
    if (source == 3u) {
        return pixel.b;
    }
    if (source == 4u) {
        return colorama_hue(pixel.rgb);
    }
    if (source == 5u) {
        return colorama_lightness(pixel.rgb);
    }
    if (source == 6u) {
        return colorama_saturation(pixel.rgb);
    }
    if (source == 7u) {
        return max(pixel.r, max(pixel.g, pixel.b));
    }
    if (source == 8u) {
        return clamp(pixel.a, 0.0, 1.0);
    }
    return 0.0;
}

fn colorama_add(first: f32, second: f32, mode: u32) -> f32 {
    if (mode == 0u) {
        return fract(first + second);
    }
    if (mode == 1u) {
        return min(first + second, 1.0);
    }
    if (mode == 2u) {
        return 0.5 * (first + second);
    }
    return 1.0 - (1.0 - first) * (1.0 - second);
}

fn colorama_palette(effect: EffectUniform, t: f32) -> vec4<f32> {
    let x = fract(t) * 5.0;
    let index = u32(floor(x));
    let f = x - floor(x);
    let first = colorama_stop(min(index, 4u), effect);
    let second = colorama_stop((index + 1u) % 5u, effect);
    let weight = f * f * (3.0 - 2.0 * f);
    let hue = fract(first.x + (fract(second.x - first.x + 1.5) - 0.5) * weight);
    let saturation = colorama_mix(first.y, second.y, weight);
    let value = colorama_mix(first.z, second.z, weight);
    let colour = colorama_hsv_to_rgb(hue, saturation, value);
    let code = (effect.header.z >> 16u) & 0xFFu;
    var alpha = 1.0;
    // Only `Alpha Ramp` (menu index 1) carries a per-sample alpha; every other
    // palette is fully opaque. `None` is index 34 and never reaches here.
    if (code == 1u) {
        alpha = t;
    }
    return vec4<f32>(colour, alpha);
}

fn colorama_match(effect: EffectUniform, pixel: vec4<f32>) -> f32 {
    let mode = effect.header.y >> 24u;
    if (mode == 0u) {
        return 1.0;
    }
    var distance = 0.0;
    if (mode == 1u) {
        let difference = pixel.rgb - effect.color.rgb;
        distance = min(sqrt(dot(difference, difference)) * 0.5773503, 1.0);
    } else if (mode == 2u) {
        let delta = abs(colorama_hue(pixel.rgb) - colorama_hue(effect.color.rgb));
        distance = 2.0 * min(delta, 1.0 - delta);
    } else {
        let delta = abs(colorama_hue(pixel.rgb) - colorama_hue(effect.color.rgb));
        let hue_distance = 2.0 * min(delta, 1.0 - delta);
        let saturation_distance =
            abs(colorama_saturation(pixel.rgb) - colorama_saturation(effect.color.rgb));
        distance = min(
            sqrt(hue_distance * hue_distance + saturation_distance * saturation_distance)
                * 0.70710678,
            1.0,
        );
    }
    let tolerance = effect.extra0.y;
    let softness = effect.extra0.x;
    if (softness <= 0.0) {
        if (distance <= tolerance) {
            return 1.0;
        }
        return 0.0;
    }
    let ramp = clamp((distance - tolerance) / softness, 0.0, 1.0);
    return 1.0 - ramp * ramp * (3.0 - 2.0 * ramp);
}

fn colorama_matte(effect: EffectUniform, pixel: vec4<f32>) -> f32 {
    let mode = (effect.header.z >> 8u) & 0xFFu;
    var weight = 1.0;
    if (mode == 1u) {
        weight = dot(pixel.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
    } else if (mode == 2u) {
        weight = pixel.a;
    } else if (mode == 3u) {
        weight = 1.0 - dot(pixel.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
    } else if (mode == 4u) {
        weight = 1.0 - pixel.a;
    }
    return clamp(weight, 0.0, 1.0);
}

fn colorama_modify(pixel: vec4<f32>, palette: vec4<f32>, code: u32) -> vec3<f32> {
    let source = pixel.rgb;
    let colour = palette.rgb;
    if (code == 0u) {
        return colour;
    }
    if (code == 1u) {
        return vec3<f32>(colour.x, source.y, source.z);
    }
    if (code == 2u) {
        return vec3<f32>(source.x, colour.y, source.z);
    }
    if (code == 3u) {
        return vec3<f32>(source.x, source.y, colour.z);
    }
    if (code == 4u) {
        return vec3<f32>(colour.x, colour.y, source.z);
    }
    if (code == 5u) {
        return vec3<f32>(source.x, colour.y, colour.z);
    }
    if (code == 6u) {
        return vec3<f32>(colour.x, source.y, colour.z);
    }
    if (code == 7u) {
        return source;
    }
    if (code == 8u) {
        return colorama_hsl_to_rgb(
            colorama_hue(colour),
            colorama_saturation(source),
            colorama_lightness(source),
        );
    }
    if (code == 9u) {
        return colorama_hsl_to_rgb(
            colorama_hue(source),
            colorama_saturation(source),
            colorama_lightness(colour),
        );
    }
    if (code == 10u) {
        return colorama_hsl_to_rgb(
            colorama_hue(source),
            colorama_saturation(colour),
            colorama_lightness(source),
        );
    }
    if (code == 11u) {
        return colorama_hsl_to_rgb(
            colorama_hue(colour),
            colorama_saturation(source),
            colorama_lightness(colour),
        );
    }
    if (code == 12u) {
        return colorama_hsl_to_rgb(
            colorama_hue(source),
            colorama_saturation(colour),
            colorama_lightness(colour),
        );
    }
    return colorama_hsl_to_rgb(
        colorama_hue(colour),
        colorama_saturation(colour),
        colorama_lightness(source),
    );
}

fn effects_author_coverage() -> bool {
    // A pixel without coverage is invisible, so every grade could skip it — and
    // that is exactly what this early return does. The exception is a chain that
    // contains a stage able to author coverage: Levels in alpha mode can lift an
    // empty pixel into view, and then whatever the earlier stages did to its
    // hidden colour becomes visible. The CPU reference applies every stage to
    // every pixel, so once the chain can reveal a pixel the GPU has to run the
    // whole chain as well, or the two sides would reveal different colours.
    for (var index = 0u; index < 16u; index = index + 1u) {
        if (index >= uniforms.effect_count) { break; }
        let effect = uniforms.effects[index];
        if (effect.header.x == 26u && effect.extra0.y > 4.5) { return true; }
        // A generated test signal replaces the pixel outright, coverage
        // included, so an empty input pixel must not skip it.
        if (effect.header.x == 31u) { return true; }
        // The four-color gradient is a generator as well: it authors its own
        // coverage from the authored opacity, so a transparent source pixel
        // must still reach the blend.
        if (effect.header.x == 35u) { return true; }
        // The checkerboard is a generator too, so a transparent source pixel
        // must still reach the blend.
        if (effect.header.x == 36u) { return true; }
        // The grid is the same kind of generator: it authors coverage from the
        // authored border, so a transparent source pixel must not skip it.
        if (effect.header.x == 37u) { return true; }
        // The circle is another generator: it authors coverage from the signed
        // distance to the radius, so a transparent source pixel must not skip it.
        if (effect.header.x == 38u) { return true; }
        // The fill is the last generator: its coverage comes from the inset
        // feather ramp, so a transparent source pixel must not skip it either.
        if (effect.header.x == 39u) { return true; }
        // The cell pattern is a generator as well: it always writes opaque
        // coverage, so a transparent source pixel must not skip it.
        if (effect.header.x == 41u) { return true; }
        // The fractal is a generator too: it authors coverage from the escape
        // field, so a transparent source pixel must not skip it.
        if (effect.header.x == 42u) { return true; }
        // The colour map can paint coverage into an empty pixel when the
        // authored switch is on, so its hidden colour must be computed too.
        if (effect.header.x == 46u && (effect.header.z & 0x04000000u) != 0u) { return true; }
        // The channel combiner can write the alpha slot from a hidden colour
        // channel or from the authored solid alpha, which turns an empty pixel
        // visible, so the hidden colour has to be computed as well.
        if (effect.header.x == 47u
            && (((effect.header.y >> 12u) & 15u) != 15u || effect.params.x > 0.0)) {
            return true;
        }
    }
    return false;
}

fn test_signals_column(column: u32, width: u32, count: u32) -> u32 {
    if (width == 0u) { return 0u; }
    return min((column * count) / width, count - 1u);
}

// The seven 75% SMPTE bars: white, yellow, cyan, green, magenta, red, blue.
fn test_signals_bar(index: u32) -> vec3<f32> {
    if (index == 0u) { return vec3<f32>(0.75); }
    if (index == 1u) { return vec3<f32>(0.75, 0.75, 0.0); }
    if (index == 2u) { return vec3<f32>(0.0, 0.75, 0.75); }
    if (index == 3u) { return vec3<f32>(0.0, 0.75, 0.0); }
    if (index == 4u) { return vec3<f32>(0.75, 0.0, 0.75); }
    if (index == 5u) { return vec3<f32>(0.75, 0.0, 0.0); }
    return vec3<f32>(0.0, 0.0, 0.75);
}

// The reverse bars of the lower third: blue, black, magenta, black, cyan,
// black, white.
fn test_signals_reverse_bar(index: u32) -> vec3<f32> {
    if (index == 0u) { return vec3<f32>(0.0, 0.0, 0.75); }
    if (index == 2u) { return vec3<f32>(0.75, 0.0, 0.75); }
    if (index == 4u) { return vec3<f32>(0.0, 0.75, 0.75); }
    if (index == 6u) { return vec3<f32>(0.75); }
    return vec3<f32>(0.0);
}

fn test_signals_radius_squared(column: u32, row: u32, width: u32, height: u32) -> f32 {
    let centre_x = (f32(width) - 1.0) * 0.5;
    let centre_y = (f32(height) - 1.0) * 0.5;
    let dx = f32(column) - centre_x;
    let dy = f32(row) - centre_y;
    return dx * dx + dy * dy;
}

fn test_signals_abs_diff(a: u32, b: u32) -> u32 {
    if (a >= b) { return a - b; }
    return b - a;
}

fn test_signals_pattern(pattern: u32, column: u32, row: u32, width: u32, height: u32) -> vec3<f32> {
    if (pattern == 0u) {
        let band = (row * 3u) / height;
        if (band < 2u) { return test_signals_bar(test_signals_column(column, width, 7u)); }
        if (column * 2u < width) {
            return test_signals_reverse_bar(test_signals_column(column * 2u, width, 7u));
        }
        let pluge_start = (width * 7u) / 8u;
        if (width > pluge_start && column >= pluge_start) {
            let step = test_signals_column(column - pluge_start, width - pluge_start, 3u);
            if (step == 0u) { return vec3<f32>(-0.04); }
            if (step == 1u) { return vec3<f32>(0.0); }
            return vec3<f32>(0.04);
        }
        return vec3<f32>(0.0);
    }
    if (pattern == 1u) {
        let level = f32(test_signals_column(column, width, 11u)) / 10.0;
        return vec3<f32>(level);
    }
    if (pattern == 2u) {
        let level = f32(test_signals_column(column, width, 16u)) / 15.0;
        return vec3<f32>(level);
    }
    if (pattern == 3u) {
        let cell = (column / 32u + row / 32u) % 2u;
        if (cell == 0u) { return vec3<f32>(1.0); }
        return vec3<f32>(0.0);
    }
    if (pattern == 4u) {
        let radius = sqrt(test_signals_radius_squared(column, row, width, height));
        let ring = u32(radius / 16.0);
        if (ring % 2u == 0u) { return vec3<f32>(1.0); }
        return vec3<f32>(0.0);
    }
    // The radial zone plate peaks at the centre, so its sine carries a
    // quarter-turn phase.
    let extent = min(f32(width), f32(height));
    let wave_number = 6.283185307179586 / (8.0 * extent);
    let value = 0.5 + 0.5 * sin(wave_number * test_signals_radius_squared(column, row, width, height) + 1.5707963267948966);
    return vec3<f32>(value);
}

// The calibration marks: a 10% grid, a centre cross, and four corner ticks.
fn test_signals_mark(column: u32, row: u32, width: u32, height: u32) -> bool {
    if (width == 0u || height == 0u) { return false; }
    let step_x = max((width + 9u) / 10u, 1u);
    let step_y = max((height + 9u) / 10u, 1u);
    if (column % step_x == 0u || row % step_y == 0u) { return true; }
    let centre_x = (width - 1u) / 2u;
    let centre_y = (height - 1u) / 2u;
    let arm = max(min(width, height) / 10u, 1u);
    if (column == centre_x && test_signals_abs_diff(row, centre_y) <= arm) { return true; }
    if (row == centre_y && test_signals_abs_diff(column, centre_x) <= arm) { return true; }
    let near_left = column < arm;
    let near_right = column + arm >= width;
    let near_top = row < arm;
    let near_bottom = row + arm >= height;
    return (near_top || near_bottom) && (near_left || near_right);
}

fn test_signals_safe_rect(column: u32, row: u32, width: u32, height: u32, scale: f32) -> bool {
    if (width < 2u || height < 2u) { return false; }
    let inset_x = f32(width) * (1.0 - scale) * 0.5;
    let inset_y = f32(height) * (1.0 - scale) * 0.5;
    let centre_x = f32(column) + 0.5;
    let centre_y = f32(row) + 0.5;
    let left = inset_x;
    let right = f32(width) - inset_x;
    let top = inset_y;
    let bottom = f32(height) - inset_y;
    let on_vertical = abs(centre_x - left) <= 0.75 || abs(centre_x - right) <= 0.75;
    let on_horizontal = abs(centre_y - top) <= 0.75 || abs(centre_y - bottom) <= 0.75;
    let inside_y = centre_y >= top - 0.75 && centre_y <= bottom + 0.75;
    let inside_x = centre_x >= left - 0.75 && centre_x <= right + 0.75;
    return (on_vertical && inside_y) || (on_horizontal && inside_x);
}

fn test_signals_safe_frame(column: u32, row: u32, width: u32, height: u32) -> bool {
    return test_signals_safe_rect(column, row, width, height, 0.93)
        || test_signals_safe_rect(column, row, width, height, 0.90);
}

fn apply_effects(input: vec4<f32>, position: vec2<f32>) -> vec4<f32> {
    if (input.a <= 0.0 && !effects_author_coverage()) { return input; }
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
        } else if (effect.header.x == 13u) {
            // Packed `InvertChannels::gpu_scope`: header.y selects the RGB
            // channels inverted with `1 - x`, header.z carries alpha/hue/
            // saturation/lightness in bits 0..3, params.x retains that
            // fraction of the source pixel.
            let color_mask = effect.header.y;
            let flags = effect.header.z;
            var rgb = pixel.rgb;
            if ((flags & 2u) != 0u) {
                rgb = invert_hue(rgb);
            }
            if ((flags & 4u) != 0u) {
                rgb = invert_saturation(rgb);
            }
            if ((flags & 8u) != 0u) {
                rgb = invert_lightness(rgb);
            }
            var color = rgb;
            if ((color_mask & 1u) != 0u) { color.r = 1.0 - pixel.r; }
            if ((color_mask & 2u) != 0u) { color.g = 1.0 - pixel.g; }
            if ((color_mask & 4u) != 0u) { color.b = 1.0 - pixel.b; }
            let alpha = select(pixel.a, 1.0 - pixel.a, (flags & 1u) != 0u);
            let retained = clamp(effect.params.x, 0.0, 1.0);
            let inverted = vec4<f32>(color, alpha);
            if (retained <= 0.0) {
                pixel = inverted;
            } else if (retained < 1.0) {
                pixel = mix(inverted, pixel, vec4<f32>(retained));
            }
        } else if (effect.header.x == 14u) {
            // `HueSaturationGrade::gpu_controls`: params carries the hue turns,
            // saturation scale, lightness offset, and colorize amount; color
            // carries the tint hue, tint saturation, tint lightness, and the band
            // center in turns (negative selects the master band).
            let weight = hue_saturation_band_weight(pixel.rgb, effect.color.w);
            var rgb = pixel.rgb;
            if (weight > 0.0) {
                rgb = mix(rgb, hue_saturation_shifted(rgb, effect.params), weight);
            }
            if (effect.params.w > 0.0) {
                rgb = mix(rgb, hue_saturation_tinted(rgb, effect.color), effect.params.w);
            }
            pixel = vec4<f32>(rgb, pixel.a);
        } else if (effect.header.x == 15u) {
            // `BlackAndWhiteGrade::gpu_controls`: params carries the first four
            // hue weights, color carries the last two weights plus the tint
            // amount, and extra0 carries the tint color.
            let gray = black_and_white(
                pixel.rgb,
                effect.params,
                vec2<f32>(effect.color.x, effect.color.y),
                effect.color.z,
                effect.extra0.xyz,
            );
            pixel = vec4<f32>(gray, pixel.a);
        } else if (effect.header.x == 17u) {
                // `ChannelMixerGrade`: params/color/extra0 hold the three output rows in
                // percent, extra1.x flags monochrome output.
                let red = vec3<f32>(
                    dot(pixel.rgb, vec3<f32>(effect.params.x, effect.params.y, effect.params.z))
                        + effect.params.w,
                    dot(pixel.rgb, vec3<f32>(effect.color.x, effect.color.y, effect.color.z))
                        + effect.color.w,
                    dot(pixel.rgb, vec3<f32>(effect.extra0.x, effect.extra0.y, effect.extra0.z))
                        + effect.extra0.w,
                ) / 100.0;
                if (effect.extra1.x > 0.5) {
                    let gray = red.r;
                    pixel = vec4<f32>(gray, gray, gray, pixel.a);
                } else {
                    pixel = vec4<f32>(red.r, red.g, red.b, pixel.a);
                }
        } else if (effect.header.x == 18u) {
            // `ThresholdGrade`: params.x is the level and color.xyz the
            // working-space luminance coefficients.
            let luminance = dot(pixel.rgb, effect.color.xyz);
            let value = select(0.0, 1.0, luminance >= effect.params.x);
            pixel = vec4<f32>(value, value, value, pixel.a);
        } else if (effect.header.x == 19u) {
            // `PosterizeGrade`: params.x is the level count per channel.
            let steps = max(effect.params.x - 1.0, 1.0);
            // Round half away from zero, matching f32::round on the CPU side.
            let scaled = pixel.rgb * steps;
            let magnitude = floor(abs(scaled) + 0.5) * sign(scaled);
            pixel = vec4<f32>(magnitude / steps, pixel.a);
        } else if (effect.header.x == 20u) {
            // `TintGrade`: params is the amount followed by the working-space
            // luminance coefficients, color.xyz the black target plus
            // extra0.xy carrying the white target's green and blue. The remap
            // never clamps, so HDR and negative scene-linear values survive.
            let luminance = dot(pixel.rgb, effect.params.yzw);
            let black = effect.color.xyz;
            let white = vec3<f32>(effect.color.w, effect.extra0.x, effect.extra0.y);
            let tint_target = black + (white - black) * luminance;
            pixel = vec4<f32>(mix(pixel.rgb, tint_target, effect.params.x), pixel.a);
        } else if (effect.header.x == 21u) {
            // `VibranceGrade`: params packs the two author controls followed by
            // the first two working-space luminance coefficients, extra0.x the
            // third. The scale is non-negative by construction, so the kernel
            // needs no clamp and extended-range colours keep their sign.
            let coefficients = vec3<f32>(effect.params.z, effect.params.w, effect.extra0.x);
            let luminance = dot(pixel.rgb, coefficients);
            let maximum = max(pixel.r, max(pixel.g, pixel.b));
            let minimum = min(pixel.r, min(pixel.g, pixel.b));
            let vivid = clamp((maximum - minimum) / max(maximum, 1.0e-6), 0.0, 1.0);
            let muted = 1.0 - vivid;
            let scale = (1.0 + effect.params.x * muted * muted) * (1.0 + effect.params.y);
            pixel = vec4<f32>(luminance + (pixel.rgb - luminance) * scale, pixel.a);
        } else if (effect.header.x == 22u) {
            // `TritoneGrade`: params is the blend amount followed by the
            // working-space luminance coefficients; color.xyz and extra0.xy hold
            // the shadow and mid ramp colours, extra0.zw and extra1.x the high
            // ramp colour. The ramp clamps the position, not the source, so
            // extended-range pixels keep their value.
            let coefficients = vec3<f32>(effect.params.y, effect.params.z, effect.params.w);
            let position = clamp(dot(pixel.rgb, coefficients), 0.0, 1.0);
            let shadows = effect.color.xyz;
            let midtones = vec3<f32>(effect.color.w, effect.extra0.x, effect.extra0.y);
            let highlights = vec3<f32>(effect.extra0.z, effect.extra0.w, effect.extra1.x);
            let ramp = select(
                mix(midtones, highlights, position * 2.0 - 1.0),
                mix(shadows, midtones, position * 2.0),
                position <= 0.5
            );
            pixel = vec4<f32>(mix(ramp, pixel.rgb, effect.params.x), pixel.a);
        } else if (effect.header.x == 24u) {
            // `ExtractGrade`: the authored channel becomes the matte and alpha
            // is left alone.
            let channel = effect.params.x;
            var value: f32;
            if (channel < 0.5) {
                value = dot(pixel.rgb, effect.color.xyz);
            } else if (channel < 1.5) {
                value = pixel.r;
            } else if (channel < 2.5) {
                value = pixel.g;
            } else if (channel < 3.5) {
                value = pixel.b;
            } else {
                value = pixel.a;
            }
            // The compositor short circuits zero coverage, so the matte only
            // replaces a pixel that is actually visible.
            if (pixel.a > 0.0) {
                let matte = extract_matte(value, effect);
                pixel = vec4<f32>(matte, matte, matte, pixel.a);
            }
        } else if (effect.header.x == 25u) {
            // `BrightnessContrastGrade`: params.x is the gain, params.y the
            // offset in the working domain and params.z the legacy clipping
            // toggle. Alpha is never authored, so a pixel without coverage is
            // left alone, exactly like the CPU kernel does.
            if (pixel.a > 0.0) {
                let shifted = pixel.rgb * effect.params.x + effect.params.y;
                let rgb = select(
                    shifted,
                    clamp(shifted, vec3<f32>(0.0), vec3<f32>(1.0)),
                    effect.params.z > 0.5
                );
                pixel = vec4<f32>(rgb, pixel.a);
            }
        } else if (effect.header.x == 26u) {
            // `LevelsGrade`: params is [input_black, input_white, gamma,
            // output_black] and extra0 is [output_white, channel menu number,
            // clip_output_black menu number, clip_output_white menu number].
            let channel = effect.extra0.y;
            if (channel > 4.5) {
                // Alpha mode authors coverage, so the value is mapped even when
                // it arrives at zero: creating coverage is the point of the
                // grade. The colour channels are never authored here.
                pixel = vec4<f32>(pixel.rgb, levels_map(pixel.a, effect));
            } else if (pixel.a > 0.0) {
                // Colour modes never author coverage, so a pixel without
                // coverage is left exactly as it arrived.
                let red = select(pixel.r, levels_map(pixel.r, effect), channel < 2.5);
                let green = select(
                    pixel.g,
                    levels_map(pixel.g, effect),
                    channel < 1.5 || (channel > 2.5 && channel < 3.5)
                );
                let blue = select(
                    pixel.b,
                    levels_map(pixel.b, effect),
                    channel < 1.5 || channel > 3.5
                );
                pixel = vec4<f32>(red, green, blue, pixel.a);
            }
        } else if (effect.header.x == 27u) {
            // `RampGrade`: params is [start.x, start.y, end.x, end.y], color is
            // the start colour, extra0 the end colour and extra1
            // [shape tag, scatter, blend_with_original, 0]. The ramp never
            // authors coverage, so a pixel without coverage is left alone. The
            // gradient is straight alpha here: the composite blends it with
            // `blend_straight_alpha`, so it must not be premultiplied.
            if (pixel.a > 0.0) {
                let coords = floor(max(position, vec2<f32>(0.0)) + vec2<f32>(0.5));
                let delta = effect.params.zw - effect.params.xy;
                let length_squared = dot(delta, delta);
                let offset = coords - effect.params.xy;
                var t = 0.0;
                if (length_squared > 1.0e-12) {
                    if (effect.extra1.x < 0.5) {
                        t = dot(offset, delta) / length_squared;
                    } else {
                        t = length(offset) / sqrt(length_squared);
                    }
                }
                if (effect.extra1.y > 0.0) {
                    let jitter = ramp_unit_noise(u32(coords.x), u32(coords.y)) - 0.5;
                    t = t + jitter * (effect.extra1.y / 255.0);
                }
                t = clamp(t, 0.0, 1.0);
                let gradient = effect.color.rgb + (effect.extra0.rgb - effect.color.rgb) * t;
                pixel = vec4<f32>(mix(gradient, pixel.rgb, effect.extra1.z), pixel.a);
            }
        } else if (effect.header.x == 28u) {
            // `ArithmeticGrade`: params is [operator index, red, green, blue] and
            // extra0.x the clipping toggle. Each colour channel is combined with
            // its own authored value; alpha is never authored, so a pixel without
            // coverage is left exactly as it arrived.
            if (pixel.a > 0.0) {
                let operation = i32(effect.params.x);
                let values = effect.params.yzw;
                let clip_result = effect.extra0.x > 0.5;
                let red = arithmetic_combine(pixel.r, values.x, operation, clip_result);
                let green = arithmetic_combine(pixel.g, values.y, operation, clip_result);
                let blue = arithmetic_combine(pixel.b, values.z, operation, clip_result);
                pixel = vec4<f32>(red, green, blue, pixel.a);
            }
        } else if (effect.header.x == 29u) {
            // `LeaveColorGrade`: params is [amount, tolerance, softness, match
            // mode] and color is the kept colour. The match distance follows the
            // CPU kernel exactly: the RGB cube distance normalised by sqrt(3), or
            // the circular hue distance from the shared hue expression. The
            // explicit epsilon keeps the soft edge wider than the tolerance even
            // when the authored softness is zero. Alpha is never authored, so a
            // pixel without coverage is left exactly as it arrived.
            if (pixel.a > 0.0) {
                let kept = effect.color.rgb;
                var match_distance: f32;
                if (effect.params.w < 0.5) {
                    let difference = pixel.rgb - kept;
                    match_distance = sqrt(dot(difference, difference)) / 1.7320508;
                } else {
                    let difference = abs(leave_color_hue(pixel.rgb) - leave_color_hue(kept));
                    match_distance = 2.0 * min(difference, 1.0 - difference);
                }
                let edge0 = effect.params.y;
                let edge1 = effect.params.y + effect.params.z + 1.0e-4;
                var keep = 0.0;
                if (edge1 <= edge0) {
                    keep = select(0.0, 1.0, match_distance <= edge0);
                } else {
                    let t = clamp((match_distance - edge0) / (edge1 - edge0), 0.0, 1.0);
                    keep = 1.0 - t * t * (3.0 - 2.0 * t);
                }
                let grey = dot(pixel.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
                let decolour = effect.params.x * (1.0 - keep);
                pixel = vec4<f32>(mix(pixel.rgb, vec3<f32>(grey), decolour), pixel.a);
            }
        } else if (effect.header.x == 30u) {
            // `BroadcastSafeGrade`: params is [safety mode, limit, 0, 0] and
            // extra0 is [locale, 0, 0, 0]. The limit was resolved on the CPU
            // from the authored IRE amplitude and the locale, so the shader
            // never re-derives the IRE. Mode 0 scales an over-limit luma toward
            // the limit, mode 1 mixes an over-limit sample toward its luma,
            // modes 2 and 3 key the unsafe or the safe samples out by clearing
            // alpha, and mode 4 clamps each channel. A pixel without coverage is
            // left exactly as it arrived, and the modes that do not key pass the
            // source alpha through.
            if (pixel.a > 0.0) {
                let limit = effect.params.y;
                let luma = dot(pixel.rgb, vec3<f32>(0.299, 0.587, 0.114));
                let maximum = max(pixel.r, max(pixel.g, pixel.b));
                let mode = i32(effect.params.x);
                if (mode == 0) {
                    if (luma > limit) {
                        pixel = vec4<f32>(pixel.rgb * (limit / luma), pixel.a);
                    }
                } else if (mode == 1) {
                    if (maximum > limit) {
                        let chroma = maximum - luma;
                        if (chroma > 1.0e-6) {
                            let amount = clamp((maximum - limit) / chroma, 0.0, 1.0);
                            pixel = vec4<f32>(mix(pixel.rgb, vec3<f32>(luma), amount), pixel.a);
                        }
                    }
                } else if (mode == 2) {
                    if (maximum > limit) {
                        pixel = vec4<f32>(pixel.rgb, 0.0);
                    }
                } else if (mode == 3) {
                    if (maximum <= limit) {
                        pixel = vec4<f32>(pixel.rgb, 0.0);
                    }
                } else {
                    pixel = vec4<f32>(
                        clamp(pixel.rgb, vec3<f32>(0.0), vec3<f32>(limit)),
                        pixel.a,
                    );
                }
            }
        } else if (effect.header.x == 40u) {
            // `HdrToneMappingGrade`: params is [mode code, gain, gamma,
            // 1 / hable(11.2)] and the extras are zero. The white-point inverse
            // was resolved on the CPU, so the shader never re-derives hable(11.2).
            // The mode code is one-based: 1 is Reinhard and 2 is Hable. The order
            // is fixed: exposure in, negative clamp, tone curve, then the inverse
            // gamma display encoding, all per channel. The filmic result is pinned
            // to one at the white point so the output stays display-referred,
            // exactly like the CPU kernel. Alpha passes through and a pixel
            // without coverage is left exactly as it arrived.
            if (pixel.a > 0.0) {
                let x = max(pixel.rgb * effect.params.y, vec3<f32>(0.0));
                let reinhard = x / (1.0 + x);
                let filmic = min(
                    (x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14)
                        * effect.params.w,
                    vec3<f32>(1.0),
                );
                let y = select(reinhard, filmic, effect.params.x > 1.5);
                pixel = vec4<f32>(pow(y, vec3<f32>(1.0 / effect.params.z)), pixel.a);
            }
        } else if (effect.header.x == 23u) {
            // `NoiseGrade`: params.x is the amount as a fraction of the full
            // signed range, params.y the grain type and params.z the clipping
            // toggle. The grain only depends on the integer pixel position.
            let coords = vec2<u32>(floor(max(position, vec2<f32>(0.0)) + vec2<f32>(0.5)));
            var grain: vec3<f32>;
            if (effect.params.y < 0.5) {
                grain = vec3<f32>(
                    noise_signed_grain(coords.x, coords.y, NOISE_SALT_RED),
                    noise_signed_grain(coords.x, coords.y, NOISE_SALT_GREEN),
                    noise_signed_grain(coords.x, coords.y, NOISE_SALT_BLUE)
                );
            } else {
                let delta = noise_signed_grain(coords.x, coords.y, NOISE_SALT_LUMINANCE);
                grain = vec3<f32>(delta);
            }
            var noisy = pixel.rgb + grain * effect.params.x;
            if (effect.params.z > 0.5) {
                noisy = clamp(noisy, vec3<f32>(0.0), vec3<f32>(1.0));
            }
            pixel = vec4<f32>(noisy, pixel.a);        } else if (effect.header.x == 16u) {
            // `ShiftChannelsGrade`: header.y packs the four 4-bit source tags
            // (red, green, blue, alpha) and params.xyz carries the working-space
            // CIE Y coefficients.
            let coefficients = effect.params.xyz;
            let packed = effect.header.y;
            let red = shift_channels_source(pixel.rgb, pixel.a, packed & 15u, coefficients);
            let green =
                shift_channels_source(pixel.rgb, pixel.a, (packed >> 4u) & 15u, coefficients);
            let blue =
                shift_channels_source(pixel.rgb, pixel.a, (packed >> 8u) & 15u, coefficients);
            let alpha =
                shift_channels_source(pixel.rgb, pixel.a, (packed >> 12u) & 15u, coefficients);
            pixel = vec4<f32>(red, green, blue, alpha);
        } else if (effect.header.x == 31u) {
            // `TestSignalsGrade`: params is [pattern, intensity, labels,
            // safety], extra0 is [width, height, 0, 0] and colour / extra1 are
            // unused. The generator reads no source pixel, so alpha is 1.
            let frame_width = u32(max(effect.extra0.x, 0.0));
            let frame_height = u32(max(effect.extra0.y, 0.0));
            if (frame_width >= 1u && frame_height >= 1u) {
                let column = u32(max(position.x, 0.0));
                let row = u32(max(position.y, 0.0));
                var rgb = test_signals_pattern(
                    u32(effect.params.x),
                    column,
                    row,
                    frame_width,
                    frame_height,
                ) * effect.params.y;
                // The marks and the safe frames are measurement overlays: they
                // stay full white whatever the intensity is.
                if (effect.params.z > 0.5
                    && test_signals_mark(column, row, frame_width, frame_height)) {
                    rgb = vec3<f32>(1.0);
                }
                if (effect.params.w > 0.5
                    && test_signals_safe_frame(column, row, frame_width, frame_height)) {
                    rgb = vec3<f32>(1.0);
                }
                pixel = vec4<f32>(rgb, 1.0);
            }
        } else if (effect.header.x == 32u) {
            // `ColorBalanceGrade`: params is [shadow r, shadow g, shadow b,
            // midtone r], colour is [midtone g, midtone b, highlight r,
            // highlight g] and extra0 is [highlight b, preserve, 0, 0]. The
            // region weights come from the source Rec.709 luma and are shared
            // by all three channels, so the three transfers cannot drift
            // apart. The luminosity rescale is deliberately not clamped again:
            // keeping the source luma has priority over the unit range.
            let source_luma = dot(pixel.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
            let shadows = clamp(1.0 - 2.0 * source_luma, 0.0, 1.0);
            let highlights = clamp(2.0 * source_luma - 1.0, 0.0, 1.0);
            let midtones = 1.0 - shadows - highlights;
            let amounts = (
                vec3<f32>(effect.params.x, effect.params.y, effect.params.z) * shadows
                + vec3<f32>(effect.params.w, effect.color.x, effect.color.y) * midtones
                + vec3<f32>(effect.color.z, effect.color.w, effect.extra0.x) * highlights
            ) / 100.0 * 0.5;
            var balanced = clamp(pixel.rgb + amounts, vec3<f32>(0.0), vec3<f32>(1.0));
            if (effect.extra0.y > 0.5) {
                let balanced_luma = dot(balanced, vec3<f32>(0.2126, 0.7152, 0.0722));
                if (balanced_luma > 0.000001) {
                    balanced = balanced * (source_luma / balanced_luma);
                }
            }
            pixel = vec4<f32>(balanced, pixel.a);
        } else if (effect.header.x == 33u) {
            // `SpillSuppressorGrade`: params is [suppression, tolerance,
            // desaturation, spill range] and colour is [key r, key g, key b,
            // colour correction], each over 100 except the key. extra0 is
            // [luminance correction, method, dominant channel, 0]; the host
            // already resolved the key's dominant channel (red before green
            // before blue on ties), so the shader never re-derives it. The
            // order is fixed: despill, then desaturate, then luminance
            // correction, and the 仅颜色 method stops after the despill.
            let source_luma = dot(pixel.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
            let dominant = i32(effect.extra0.z);
            let key_max = select(
                select(effect.color.z, effect.color.y, dominant == 1),
                effect.color.x,
                dominant == 0,
            );
            // The two non-dominant channels reduce to their larger member,
            // selected without dynamic vector indexing.
            let peer = select(
                select(
                    max(pixel.r, pixel.g),
                    max(pixel.r, pixel.b),
                    dominant == 1,
                ),
                max(pixel.g, pixel.b),
                dominant == 0,
            );
            let dominant_value = select(
                select(pixel.b, pixel.g, dominant == 1),
                pixel.r,
                dominant == 0,
            );
            let spill = clamp(
                (dominant_value - peer) / max(dominant_value, 0.0001),
                0.0,
                1.0,
            );
            let pixel_max = max(max(pixel.r, pixel.g), pixel.b);
            let pixel_chroma = pixel.rgb / max(pixel_max, 0.0001);
            let key_chroma = vec3<f32>(effect.color.x, effect.color.y, effect.color.z)
                / max(key_max, 0.0001);
            let distance = length(pixel_chroma - key_chroma) * 0.5773503;
            let softness = max(effect.params.w * 0.5, 0.0001);
            let edge = clamp((distance - effect.params.y) / softness, 0.0, 1.0);
            let gate = 1.0 - edge * edge * (3.0 - 2.0 * edge);
            let weight = clamp(spill * gate, 0.0, 1.0) * effect.params.x;
            var corrected = vec3<f32>(
                select(
                    pixel.r,
                    pixel.r + (peer - pixel.r) * weight * effect.color.w,
                    dominant == 0,
                ),
                select(
                    pixel.g,
                    pixel.g + (peer - pixel.g) * weight * effect.color.w,
                    dominant == 1,
                ),
                select(
                    pixel.b,
                    pixel.b + (peer - pixel.b) * weight * effect.color.w,
                    dominant == 2,
                ),
            );
            if (effect.extra0.y < 0.5) {
                let grey = dot(corrected, vec3<f32>(0.2126, 0.7152, 0.0722));
                corrected += (vec3<f32>(grey) - corrected) * weight * effect.params.z;
                let corrected_luma = dot(corrected, vec3<f32>(0.2126, 0.7152, 0.0722));
                if (corrected_luma > 0.000001) {
                    corrected *= 1.0
                        + (source_luma / corrected_luma - 1.0) * effect.extra0.x;
                }
            }
            pixel = vec4<f32>(clamp(corrected, vec3<f32>(0.0), vec3<f32>(1.0)), pixel.a);
        } else if (effect.header.x == 35u) {
            // `FourColorGradientGrade`: params is [p1.x, p1.y, p2.x, p2.y],
            // colour is [c1.rgb, blend], extra0 is [p3.x, p3.y, p4.x, p4.y],
            // extra1 is [c2.rgb, dither], extra2 is [c3.rgb, opacity] and
            // extra3 is [c4.rgb, mode index]. The generator reads no source
            // pixel. The four inverse-distance weights carry a soft radius
            // lambda, so the sum is a partition of unity and never divides by
            // zero, even when a point coincides with the sampled centre.
            let centre = floor(max(position, vec2<f32>(0.0))) + vec2<f32>(0.5);
            let points = array<vec2<f32>, 4>(
                effect.params.xy,
                effect.params.zw,
                effect.extra0.xy,
                effect.extra0.zw,
            );
            let colors = array<vec3<f32>, 4>(
                effect.color.xyz,
                effect.extra1.xyz,
                effect.extra2.xyz,
                effect.extra3.xyz,
            );
            let lambda_squared = effect.color.w * effect.color.w;
            let lambda4 = lambda_squared * lambda_squared;
            // A vector cannot take a runtime index assignment, so the weights
            // live in an array and are folded afterwards in a fixed order.
            var weights = array<f32, 4>(0.0, 0.0, 0.0, 0.0);
            for (var corner = 0u; corner < 4u; corner = corner + 1u) {
                let offset = centre - points[corner];
                let distance_squared = dot(offset, offset);
                let distance4 = distance_squared * distance_squared;
                weights[corner] = 1.0 / (distance4 + lambda4);
            }
            let total = weights[0] + weights[1] + weights[2] + weights[3];
            let w = vec4<f32>(weights[0], weights[1], weights[2], weights[3]) / total;
            var gradient =
                colors[0] * w.x + colors[1] * w.y + colors[2] * w.z + colors[3] * w.w;
            let dither = effect.extra1.w;
            if (dither > 0.0) {
                let column = u32(centre.x);
                let row = u32(centre.y);
                gradient = gradient
                    + vec3<f32>(four_color_gradient_noise(column, row)) * (dither / 255.0);
            }
            let gradient_saturated = clamp(gradient, vec3<f32>(0.0), vec3<f32>(1.0));
            let opacity = clamp(effect.extra2.w, 0.0, 1.0);
            pixel = blend_straight_alpha(
                pixel,
                vec4<f32>(gradient_saturated, opacity),
                1.0,
                u32(max(effect.extra3.w, 0.0) + 0.5),
                index,
            );
        } else if (effect.header.x == 36u) {
            // `CheckerboardGrade`: params is [anchor.x, anchor.y, corner.x,
            // corner.y], colour is [rgb, opacity] and extra0 is [width, height,
            // feather width, feather height]. header.y selects the cell size:
            // 0 uses the corner delta, 1 uses the width on both axes and 2 uses
            // the width and height sliders. The generator reads no source pixel:
            // the parity of the integer cell decides the phase.
            let centre = floor(max(position, vec2<f32>(0.0))) + vec2<f32>(0.5);
            var cell = vec2<f32>(effect.extra0.x, effect.extra0.x);
            if (effect.header.y == 0u) {
                cell = abs(effect.params.zw - effect.params.xy);
            } else if (effect.header.y == 2u) {
                cell = effect.extra0.xy;
            }
            var coverage = 0.0;
            if (cell.x > 0.001 && cell.y > 0.001) {
                let offset = (centre - effect.params.xy) / cell;
                let cell_index = floor(offset);
                let parity = cell_index - 2.0 * floor(cell_index * 0.5);
                let within = offset - cell_index;
                let distance = min(within, vec2<f32>(1.0, 1.0) - within) * cell;
                var ramp = vec2<f32>(1.0, 1.0);
                if (effect.extra0.z > 0.0) {
                    ramp.x = clamp(distance.x / (effect.extra0.z * 0.5), 0.0, 1.0);
                }
                if (effect.extra0.w > 0.0) {
                    ramp.y = clamp(distance.y / (effect.extra0.w * 0.5), 0.0, 1.0);
                }
                let axis = vec2<f32>(0.5, 0.5) + (parity - vec2<f32>(0.5, 0.5)) * ramp;
                coverage = axis.x * axis.y + (1.0 - axis.x) * (1.0 - axis.y);
            }
            let alpha = clamp(coverage * effect.color.w, 0.0, 1.0);
            pixel = blend_straight_alpha(
                pixel,
                vec4<f32>(effect.color.xyz, alpha),
                1.0,
                effect.header.z,
                index,
            );
        } else if (effect.header.x == 37u) {
            // `GridGrade`: params is [anchor.x, anchor.y, corner.x, corner.y],
            // colour is [rgb, opacity], extra0 is [width, height, border, 0] and
            // extra1 is [feather width, feather height, 0, 0]. header.y selects
            // the cell size: 0 uses the corner delta, 1 uses the width on both
            // axes and 2 uses the width and height sliders. header.w inverts the
            // coverage. The generator reads no source pixel: the distance from
            // the pixel centre to the nearest cell boundary decides the line.
            let centre = floor(max(position, vec2<f32>(0.0))) + vec2<f32>(0.5);
            var cell = vec2<f32>(effect.extra0.x, effect.extra0.x);
            if (effect.header.y == 0u) {
                cell = abs(effect.params.zw - effect.params.xy);
            } else if (effect.header.y == 2u) {
                cell = effect.extra0.xy;
            }
            var base = 0.0;
            // `0.001` mirrors `GRID_DEGENERATE_CELL` on the CPU side, and the
            // border test keeps a zero-width grid from painting a line: with
            // `half_border == 0` an exact-boundary pixel would otherwise match
            // `distance <= 0`, and a feathered ramp would paint a half line.
            if (cell.x > 0.001 && cell.y > 0.001 && effect.extra0.z > 0.0) {
                let offset = (centre - effect.params.xy) / cell;
                let within = offset - floor(offset);
                let distance = min(within, vec2<f32>(1.0, 1.0) - within) * cell;
                let half_border = effect.extra0.z * 0.5;
                var axis = vec2<f32>(
                    select(0.0, 1.0, distance.x <= half_border),
                    select(0.0, 1.0, distance.y <= half_border),
                );
                if (effect.extra1.x > 0.0) {
                    axis.x = clamp(
                        (half_border + effect.extra1.x * 0.5 - distance.x) / effect.extra1.x,
                        0.0,
                        1.0,
                    );
                }
                if (effect.extra1.y > 0.0) {
                    axis.y = clamp(
                        (half_border + effect.extra1.y * 0.5 - distance.y) / effect.extra1.y,
                        0.0,
                        1.0,
                    );
                }
                base = max(axis.x, axis.y);
            }
            var coverage = base;
            if (effect.header.w != 0u) {
                coverage = 1.0 - base;
            }
            let alpha = coverage * clamp(effect.color.w, 0.0, 1.0);
            pixel = blend_straight_alpha(
                pixel,
                vec4<f32>(effect.color.xyz, alpha),
                1.0,
                effect.header.z,
                index,
            );
        } else if (effect.header.x == 38u) {
            // `CircleEllipseGrade`: params is [centre.x, centre.y, radius, edge],
            // colour is [rgb, opacity] and extra0 is [feather outer, feather
            // inner, 0, 0]. header.y selects the blend mode and header.w inverts
            // the coverage. The generator reads no source pixel: the signed
            // distance from the pixel centre decides both ramps, and the
            // geometric radius sits at exactly half coverage.
            let centre = floor(max(position, vec2<f32>(0.0))) + vec2<f32>(0.5);
            let offset = centre - effect.params.xy;
            let distance = sqrt(offset.x * offset.x + offset.y * offset.y);
            let radius = abs(effect.params.z);
            let half_edge = max(effect.params.w, 0.0) * 0.5;
            let inside_width = max(effect.extra0.y, half_edge);
            let outside_width = max(effect.extra0.x, half_edge);
            let signed_distance = distance - radius;
            var coverage = 0.0;
            if (signed_distance <= 0.0) {
                coverage = 0.5 + 0.5 * clamp(-signed_distance / inside_width, 0.0, 1.0);
            } else {
                coverage = 0.5 * (1.0 - clamp(signed_distance / outside_width, 0.0, 1.0));
            }
            if (effect.header.w != 0u) {
                coverage = 1.0 - coverage;
            }
            let alpha = coverage * clamp(effect.color.w, 0.0, 1.0);
            pixel = blend_straight_alpha(
                pixel,
                vec4<f32>(effect.color.xyz, alpha),
                1.0,
                effect.header.y,
                index,
            );
        } else if (effect.header.x == 39u) {
            // `FillGrade`: params is [feather_x, feather_y, 0, 0], colour is
            // [rgb, opacity], and the extras are zero. header.y is free (the fill
            // has no blend-mode parameter and always composites Normal) and
            // header.w inverts the coverage. The generator reads no source pixel:
            // the inset from the frame border, measured from the pixel centre,
            // decides both axis ramps and their product is the coverage.
            let centre = floor(max(position, vec2<f32>(0.0))) + vec2<f32>(0.5);
            let extent = max(uniforms.geometry.zw, vec2<f32>(1.0));
            let inset = min(centre, extent - centre);
            let feather = vec2<f32>(effect.params.x, effect.params.y);
            // A zero feather is the constant branch, not a degenerate division:
            // it asks for full coverage on that axis.
            let axis_x = select(
                clamp(inset.x / max(feather.x, 1.0e-6), 0.0, 1.0),
                1.0,
                feather.x <= 0.0,
            );
            let axis_y = select(
                clamp(inset.y / max(feather.y, 1.0e-6), 0.0, 1.0),
                1.0,
                feather.y <= 0.0,
            );
            var coverage = axis_x * axis_y;
            if (effect.header.w != 0u) {
                coverage = 1.0 - coverage;
            }
            let alpha = clamp(coverage * clamp(effect.color.w, 0.0, 1.0), 0.0, 1.0);
            pixel = blend_straight_alpha(
                pixel,
                vec4<f32>(effect.color.xyz, alpha),
                1.0,
                0u,
                index,
            );
        } else if (effect.header.x == 41u) {
            // `CellPatternGrade`: params is [size, offset.x, offset.y, disperse],
            // colour is [cells_h, cells_v, evolution, contrast], extra0 is
            // [seed, cycle_revolutions, 0, 0]. header.y is the cell type,
            // header.z the overflow mode and header.w the invert/tiling/cycle
            // flags. The generator replaces the pixel outright at full coverage.
            let centre = floor(max(position, vec2<f32>(0.0))) + vec2<f32>(0.5);
            let value = cell_pattern_value(effect, centre);
            pixel = vec4<f32>(value, value, value, 1.0);
        } else if (effect.header.x == 42u) {
            // `FractalGrade`: params is [mb_x, mb_y, mb_magnification,
            // julia_x], colour is [julia_y, julia_magnification, post_x,
            // post_y], extra0 is [overlay, transparency, palette, hue],
            // extra1 is [cycle_steps, cycle_offset, edge_highlight,
            // oversample_method] and extra2 is [sample_side, view_scale, 0, 0].
            // header.y is the set choice, header.z the equation and header.w
            // the resolved iteration budget. The generator replaces the pixel
            // outright, matching the CPU kernel's coverage.
            let origin = floor(max(position, vec2<f32>(0.0)));
            pixel = fractal_value(effect, origin);
        } else if (effect.header.x == 43u) {
            // `RemoveColorMattingGrade`: params is [background r, background g,
            // background b, 0] and header.y is the clip switch. The background
            // is straight (unpremultiplied), so the unmix term (1 - a) / a is
            // computed first and subtracted from the colour; the clip is one
            // final clamp of the three colour channels, never a mid-way clamp.
            // The form matches the CPU kernel exactly (C - B * (1 - a) / a), and
            // a pixel without coverage is returned untouched.
            if (pixel.a > 0.0) {
                let rgb = pixel.rgb - effect.params.xyz * ((1.0 - pixel.a) / pixel.a);
                if (effect.header.y > 0u) {
                    pixel = vec4<f32>(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)), pixel.a);
                } else {
                    pixel = vec4<f32>(rgb, pixel.a);
                }
            }
        } else if (effect.header.x == 44u) {
            // `PhotoFilterGrade`: params is [gain r, gain g, gain b, density]
            // and colour is [gel r, gel g, gel b, 0]. header.y is the
            // preserve-luminosity switch. The tint is the per-channel gain
            // (1 - d + d * F), never a lerp towards F, so full density
            // reproduces the gel exactly. With the switch on the result is
            // rescaled to the source Rec.709 luma; the 1e-6 guard mirrors the
            // CPU kernel on both sides. A pixel without coverage is untouched.
            if (pixel.a > 0.0) {
                let filtered = pixel.rgb * effect.params.xyz;
                if (effect.header.y == 1u) {
                    let source_luma = dot(pixel.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
                    let filtered_luma = dot(filtered, vec3<f32>(0.2126, 0.7152, 0.0722));
                    if (source_luma > 0.000001 && filtered_luma > 0.000001) {
                        pixel = vec4<f32>(filtered * (source_luma / filtered_luma), pixel.a);
                    } else {
                        pixel = vec4<f32>(filtered, pixel.a);
                    }
                } else {
                    pixel = vec4<f32>(filtered, pixel.a);
                }
            }
        } else if (effect.header.x == 45u) {
            // `JpegDamageGrade`: header = [45, component, block_x, block_y];
            // params = [luma step, chroma step, attenuation strength (1 - frequency), edge amplitude];
            // color = [mosaic amplitude, 0, 0, 0]. The grid is addressed from the
            // absolute sample position, so splitting a layer into tiles cannot move
            // the block boundaries. Every term is a multiply-add; neither side uses
            // pow/exp/log.
            if (pixel.a > 0.0) {
                let block = effect.header.zw;
                let sample = vec2<u32>(floor(max(position, vec2<f32>(0.0))));
                let cell = sample / block;
                let local = sample - cell * block;

                let luma = 0.2126 * pixel.r + 0.7152 * pixel.g + 0.0722 * pixel.b;
                let cb = pixel.b - luma;
                let cr = pixel.r - luma;

                var y_quantized = luma;
                var cb_quantized = cb;
                var cr_quantized = cr;
                if (effect.params.x > 0.0) {
                    y_quantized = jpeg_damage_quantize(luma, effect.params.x);
                    cb_quantized = jpeg_damage_quantize(cb, effect.params.y);
                    cr_quantized = jpeg_damage_quantize(cr, effect.params.y);
                }

                let phase_x = select(0u, 1u, local.x * 2u >= block.x);
                let phase_y = select(0u, 1u, local.y * 2u >= block.y);
                let attenuation = 1.0 - effect.params.z * (0.5 + 0.5 * f32(phase_x ^ phase_y));

                var y_out = y_quantized;
                var cb_out = cb_quantized;
                var cr_out = cr_quantized;
                if (effect.header.y != 1u) {
                    y_out = y_quantized + (luma - y_quantized) * attenuation;
                }
                if (effect.header.y != 0u) {
                    cb_out = cb_quantized * attenuation;
                    cr_out = cr_quantized * attenuation;
                }

                let red = y_out + cr_out;
                let blue = y_out + cb_out;
                let green = y_out - (0.2972595 * cr_out + 0.1009519 * cb_out);

                var edge = 0.0;
                var ring = 0.0;
                if (local.x == 0u || local.y == 0u) { edge = 1.0; }
                if ((local.x == 1u && block.x > 1u) || (local.y == 1u && block.y > 1u)) {
                    ring = 1.0;
                }
                let gain = (1.0 - effect.params.w * edge) + (0.5 * effect.params.w * ring);
                let mosaic = jpeg_damage_mosaic(cell.x, cell.y) * effect.color.x;
                pixel = vec4<f32>(
                    vec3<f32>(red, green, blue) * gain + vec3<f32>(mosaic),
                    pixel.a,
                );
            }
        } else if (effect.header.x == 46u) {
            // `ColoramaGrade`: header = [46, phase source | add source << 8 |
            // add mode << 16 | match mode << 24, modify | mask mode << 8 |
            // palette << 16 | flags << 24, 0]; params = [repetitions, phase
            // shift (turns), add phase, blend]; colour = [match r, match g,
            // match b, last stop hue]; extra0 = [softness, tolerance, last
            // stop saturation, last stop value]. The other four stops live in
            // extra1..extra3. The chain mirrors the CPU kernel: phase, add,
            // wrap, palette, selection, modification, composite, blend.
            let flags = effect.header.z >> 24u;
            let palette_code = (effect.header.z >> 16u) & 0xFFu;
            let change_empty = (flags & 4u) != 0u;
            if (effect.params.x != 0.0 && palette_code != 34u && (pixel.a > 0.0 || change_empty)) {
                let source = pixel;
                let p1 = colorama_phase(source, effect.header.y & 0xFFu);
                let add_source = (effect.header.y >> 8u) & 0xFFu;
                var phase = p1;
                if (add_source != 9u) {
                    let p2 = max(effect.params.z, 0.0) * colorama_phase(source, add_source);
                    phase = colorama_add(p1, p2, (effect.header.y >> 16u) & 0xFFu);
                }
                var t = fract(phase * effect.params.x + effect.params.y);
                if ((flags & 8u) == 0u) {
                    t = floor(t * 256.0) / 256.0;
                }
                let palette = colorama_palette(effect, t);
                var selection = colorama_match(effect, source);
                if ((flags & 16u) != 0u) {
                    selection = selection * colorama_matte(effect, source);
                }
                selection = clamp(selection, 0.0, 1.0);
                let destination = colorama_modify(source, palette, effect.header.z & 0xFFu);
                let destination_alpha = select(source.a, palette.a, (flags & 2u) != 0u);
                var rgb = destination;
                var alpha = source.a * selection;
                if ((flags & 1u) != 0u) {
                    rgb = vec3<f32>(
                        colorama_mix(source.r, destination.x, selection),
                        colorama_mix(source.g, destination.y, selection),
                        colorama_mix(source.b, destination.z, selection),
                    );
                    alpha = colorama_mix(source.a, destination_alpha, selection);
                }
                let blend = effect.params.w;
                pixel = vec4<f32>(
                    colorama_mix(rgb.x, source.r, blend),
                    colorama_mix(rgb.y, source.g, blend),
                    colorama_mix(rgb.z, source.b, blend),
                    colorama_mix(alpha, source.a, blend),
                );
            }
        } else if (effect.header.x == 34u) {
            // `ChangeToColorGrade`: params is [hue amount, lightness amount,
            // saturation amount, softness], colour is [to r, to g, to b, 0], and
            // extra0 is [from r, from g, from b, 0]; extra1.x is the correction
            // matte view. header.y is the change bitfield (bit0 hue, bit1
            // lightness, bit2 saturation) and header.z selects the absolute
            // change-by. The order is fixed: match against the chroma-normalised
            // source, shift the selected HSV components, then mix the matte view.
            if (pixel.a > 0.0) {
                let w = change_to_color_weight(pixel.rgb, effect);
                // A zero weight still shows the matte: the viewer asked to see
                // the correction, and the correction of an unmatched sample is
                // black.
                if (w > 0.0 || effect.extra1.x > 0.0) {
                    let h0 = change_to_color_hue(pixel.rgb);
                    let s0 = change_to_color_saturation(pixel.rgb);
                    let v0 = change_to_color_lightness(pixel.rgb);
                    let kh = change_to_color_hue(effect.extra0.xyz);
                    let ks = change_to_color_saturation(effect.extra0.xyz);
                    let kv = change_to_color_lightness(effect.extra0.xyz);
                    let th = change_to_color_hue(effect.color.xyz);
                    let ts = change_to_color_saturation(effect.color.xyz);
                    let tv = change_to_color_lightness(effect.color.xyz);
                    let absolute = effect.header.z != 0u;
                    let dh = select(
                        change_to_color_wrap(th - kh),
                        change_to_color_wrap(th - h0),
                        absolute,
                    );
                    let ds = select(ts - ks, ts - s0, absolute);
                    let dv = select(tv - kv, tv - v0, absolute);
                    // channel_scale is ordered like params.xyz: hue, then
                    // lightness, then saturation; the bitfield is expanded with
                    // select because the shader has no dynamic vector indexing.
                    // (`enable` is a WGSL directive keyword, so it cannot name a
                    // value here.)
                    let channel_scale = vec3<f32>(
                        select(0.0, 1.0, (effect.header.y & 1u) != 0u),
                        select(0.0, 1.0, (effect.header.y & 2u) != 0u),
                        select(0.0, 1.0, (effect.header.y & 4u) != 0u),
                    );
                    let a = w * effect.params.xyz * channel_scale;
                    let h1 = change_to_color_fract(h0 + a.x * dh);
                    let s1 = s0 + a.z * ds;
                    let v1 = v0 + a.y * dv;
                    let shifted = change_to_color_hsv_to_rgb(h1, s1, v1);
                    let mixed = mix(shifted, vec3<f32>(w), effect.extra1.x);
                    pixel = vec4<f32>(clamp(mixed, vec3<f32>(0.0), vec3<f32>(1.0)), pixel.a);
                }
            }
        } else if (effect.header.x == 47u) {
            pixel = channel_combiner_apply(
                pixel, pixel, effect.header.y, effect.header.z, effect.params,
            );
        }
    }
    return pixel;
}
"#,
    include_str!("channel_combiner.wgsl")
);

// Exact Mix has its own module and a lazily owned pipeline. Ordinary Over
// does not parse, validate or compile the fixed-width integer arithmetic.
const GPU_ADJUSTMENT_MIX_SHADER: &str = r#"
struct MixUniforms {
    // The first Float32 member of the shared Rust GpuCompositeUniforms.
    // The existing group layout and binding range remain authoritative.
    opacity: f32,
};

@group(0) @binding(0) var layer_tex: texture_2d<f32>;
@group(1) @binding(0) var accum_tex: texture_2d<f32>;
@group(2) @binding(0) var<uniform> uniforms: MixUniforms;

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> @builtin(position) vec4<f32> {
    var positions = array<vec2<f32>, 4>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 1.0, -1.0),
        vec2<f32>(-1.0,  1.0),
        vec2<f32>( 1.0,  1.0),
    );
    return vec4<f32>(positions[vertex_index], 0.0, 1.0);
}

// Exact bounded Mix of admitted StraightCoverage/Opaque texels. Each term
// has at most three Float32 factors (72 significand bits). The minimum
// quantum is 2^-447; |associated| < 2^130. Rounded quotient alignment and
// its 24 trial bits fit 19 words (608 bits). The common <=128-bit case
// runs the same arithmetic with four active words. No Float32 product,
// rounded 1-amount, division, epsilon or coverage clamp defines this law.
struct MixTerm {
    words: vec3<u32>,
    exponent: i32,
    negative: bool,
};

struct MixUnsigned {
    words: array<u32, 19>,
    count: u32,
};

struct MixExact {
    magnitude: MixUnsigned,
    origin: i32,
    negative: bool,
};

fn mix_float_term(value: f32) -> MixTerm {
    let bits = bitcast<u32>(value);
    let encoded = (bits >> 23u) & 255u;
    var mantissa = bits & 0x007fffffu;
    var exponent = -149;
    if (encoded != 0u) {
        mantissa = mantissa | 0x00800000u;
        exponent = i32(encoded) - 150;
    }
    if (mantissa != 0u) {
        let trailing = countTrailingZeros(mantissa);
        mantissa = mantissa >> trailing;
        exponent = exponent + i32(trailing);
    }
    return MixTerm(vec3<u32>(mantissa, 0u, 0u), exponent, (bits >> 31u) != 0u);
}

// Exact 32x32->64 multiplication using four 16-bit products. Each inner
// sum is bounded by 0xfffeffff; only the intended low-word carry wraps.
fn mix_multiply_word(left: u32, right: u32) -> vec2<u32> {
    let left_low = left & 65535u;
    let left_high = left >> 16u;
    let right_low = right & 65535u;
    let right_high = right >> 16u;
    let bottom = left_low * right_low;
    let middle = left_high * right_low + (bottom >> 16u);
    let cross = left_low * right_high + (middle & 65535u);
    return vec2<u32>(
        (cross << 16u) | (bottom & 65535u),
        left_high * right_high + (middle >> 16u) + (cross >> 16u),
    );
}

fn mix_multiply_term(term: MixTerm, factor: MixTerm) -> MixTerm {
    var result: MixTerm;
    result.exponent = term.exponent + factor.exponent;
    result.negative = term.negative != factor.negative;
    var carry = 0u;
    for (var index = 0u; index < 3u; index = index + 1u) {
        let product = mix_multiply_word(term.words[index], factor.words.x);
        let low = product.x + carry;
        result.words[index] = low;
        carry = product.y + select(0u, 1u, low < product.x);
    }
    return result;
}

fn mix_negate_term(term: MixTerm) -> MixTerm {
    var result = term;
    result.negative = !result.negative;
    return result;
}

fn mix_term_bits(term: MixTerm) -> u32 {
    for (var index = 2; index >= 0; index = index - 1) {
        let word = term.words[u32(index)];
        if (word != 0u) { return u32(index) * 32u + 32u - countLeadingZeros(word); }
    }
    return 0u;
}

fn mix_unsigned_bits(value: MixUnsigned) -> u32 {
    var value_storage = value;
    for (var index = i32(value_storage.count) - 1; index >= 0; index = index - 1) {
        let word = value_storage.words[u32(index)];
        if (word != 0u) { return u32(index) * 32u + 32u - countLeadingZeros(word); }
    }
    return 0u;
}

fn mix_unsigned_compare(left: MixUnsigned, right: MixUnsigned) -> i32 {
    var left_storage = left;
    var right_storage = right;
    for (var index = i32(left_storage.count) - 1; index >= 0; index = index - 1) {
        let a = left_storage.words[u32(index)];
        let b = right_storage.words[u32(index)];
        if (a < b) { return -1; }
        if (a > b) { return 1; }
    }
    return 0;
}

fn mix_unsigned_add(left: MixUnsigned, right: MixUnsigned) -> MixUnsigned {
    var left_storage = left;
    var right_storage = right;
    var result: MixUnsigned;
    result.count = left_storage.count;
    var carry = 0u;
    for (var index = 0u; index < result.count; index = index + 1u) {
        let partial = left_storage.words[index] + right_storage.words[index];
        let complete = partial + carry;
        carry = select(0u, 1u, partial < left_storage.words[index] || complete < partial);
        result.words[index] = complete;
    }
    return result;
}

// The caller establishes left>=right before this exact magnitude subtraction.
fn mix_unsigned_subtract(left: MixUnsigned, right: MixUnsigned) -> MixUnsigned {
    var left_storage = left;
    var right_storage = right;
    var result: MixUnsigned;
    result.count = left_storage.count;
    var borrow = 0u;
    for (var index = 0u; index < result.count; index = index + 1u) {
        let partial = left_storage.words[index] - right_storage.words[index];
        let complete = partial - borrow;
        borrow = select(0u, 1u, left_storage.words[index] < right_storage.words[index] || partial < borrow);
        result.words[index] = complete;
    }
    return result;
}

fn mix_unsigned_shift(value: MixUnsigned, shift: u32, count: u32) -> MixUnsigned {
    var value_storage = value;
    var result: MixUnsigned;
    result.count = count;
    let words = shift / 32u;
    let offset = shift % 32u;
    for (var index = 0u; index < value_storage.count; index = index + 1u) {
        let word = value_storage.words[index];
        if (word == 0u) { continue; }
        result.words[index + words] = result.words[index + words] | (word << offset);
        if (offset != 0u) {
            let upper = word >> (32u - offset);
            if (upper != 0u) {
                result.words[index + words + 1u] = result.words[index + words + 1u] | upper;
            }
        }
    }
    return result;
}

fn mix_unsigned_term(term: MixTerm, origin: i32, count: u32) -> MixUnsigned {
    var magnitude: MixUnsigned;
    magnitude.count = 3u;
    for (var index = 0u; index < 3u; index = index + 1u) {
        magnitude.words[index] = term.words[index];
    }
    return mix_unsigned_shift(magnitude, u32(term.exponent - origin), count);
}

fn mix_exact_sum(first: MixTerm, second: MixTerm, third: MixTerm) -> MixExact {
    var terms = array<MixTerm, 3>(first, second, third);
    var origin = 1024;
    for (var index = 0u; index < 3u; index = index + 1u) {
        if (mix_term_bits(terms[index]) != 0u) { origin = min(origin, terms[index].exponent); }
    }
    var result: MixExact;
    result.magnitude.count = 4u;
    if (origin == 1024) { return result; }
    result.origin = origin;
    var required = 0u;
    for (var index = 0u; index < 3u; index = index + 1u) {
        let bits = mix_term_bits(terms[index]);
        if (bits != 0u) { required = max(required, bits + u32(terms[index].exponent - origin) + 2u); }
    }
    result.magnitude.count = select(19u, 4u, required <= 128u);
    for (var index = 0u; index < 3u; index = index + 1u) {
        let term = terms[index];
        if (mix_term_bits(term) == 0u) { continue; }
        let magnitude = mix_unsigned_term(term, origin, result.magnitude.count);
        if (result.negative == term.negative) {
            result.magnitude = mix_unsigned_add(result.magnitude, magnitude);
        } else if (mix_unsigned_compare(result.magnitude, magnitude) >= 0) {
            result.magnitude = mix_unsigned_subtract(result.magnitude, magnitude);
        } else {
            result.magnitude = mix_unsigned_subtract(magnitude, result.magnitude);
            result.negative = term.negative;
        }
    }
    return result;
}

fn mix_unsigned_bit(value: MixUnsigned, index: u32) -> bool {
    var value_storage = value;
    if (index >= value_storage.count * 32u) { return false; }
    return ((value_storage.words[index / 32u] >> (index % 32u)) & 1u) != 0u;
}

fn mix_unsigned_below(value: MixUnsigned, end: u32) -> bool {
    var value_storage = value;
    let full = min(end / 32u, value_storage.count);
    for (var index = 0u; index < full; index = index + 1u) {
        if (value_storage.words[index] != 0u) { return true; }
    }
    let offset = end % 32u;
    return offset != 0u && full < value_storage.count &&
        (value_storage.words[full] & ((1u << offset) - 1u)) != 0u;
}

fn mix_unsigned_low_after_shift(value: MixUnsigned, shift: u32) -> u32 {
    var value_storage = value;
    let word = shift / 32u;
    let offset = shift % 32u;
    if (word >= value_storage.count) { return 0u; }
    var low = value_storage.words[word] >> offset;
    if (offset != 0u && word + 1u < value_storage.count) {
        low = low | (value_storage.words[word + 1u] << (32u - offset));
    }
    return low;
}

fn mix_round_dyadic(value: MixExact) -> f32 {
    let bits = mix_unsigned_bits(value.magnitude);
    if (bits == 0u) { return 0.0; }
    let sign = select(0u, 0x80000000u, value.negative);
    var exponent = value.origin + i32(bits) - 1;
    if (exponent > 127) { return bitcast<f32>(sign | 0x7f800000u); }
    if (exponent < -150) { return bitcast<f32>(sign); }
    let subnormal = exponent < -126;
    let quantum = select(exponent - 23, -149, subnormal);
    let shift = quantum - value.origin;
    var significand = 0u;
    if (shift > 0) {
        significand = mix_unsigned_low_after_shift(value.magnitude, u32(shift));
        let guard = mix_unsigned_bit(value.magnitude, u32(shift - 1));
        let sticky = mix_unsigned_below(value.magnitude, u32(shift - 1));
        if (guard && (sticky || (significand & 1u) != 0u)) { significand = significand + 1u; }
    } else {
        significand = mix_unsigned_low_after_shift(value.magnitude, 0u) << u32(-shift);
    }
    if (subnormal) { return bitcast<f32>(sign | significand); }
    if (significand == 0x01000000u) {
        significand = significand >> 1u;
        exponent = exponent + 1;
    }
    if (exponent > 127) { return bitcast<f32>(sign | 0x7f800000u); }
    return bitcast<f32>(sign | (u32(exponent + 127) << 23u) | (significand & 0x007fffffu));
}

// Straight delivery rounds the exact quotient once. Associated numerator
// and coverage denominator stay integer until final bitcast; neither tiny
// alpha nor an intermediate association is rounded to zero first.
fn mix_round_quotient(numerator: MixExact, denominator: MixExact) -> f32 {
    let numerator_bits = mix_unsigned_bits(numerator.magnitude);
    if (numerator_bits == 0u) { return 0.0; }
    let denominator_bits = mix_unsigned_bits(denominator.magnitude);
    let sign = select(0u, 0x80000000u, numerator.negative != denominator.negative);
    var exponent = numerator.origin - denominator.origin + i32(numerator_bits) - i32(denominator_bits);
    let left_shift = u32(max(0, i32(denominator_bits) - i32(numerator_bits)));
    let right_shift = u32(max(0, i32(numerator_bits) - i32(denominator_bits)));
    let leading_count = select(19u, 4u, max(numerator_bits, denominator_bits) <= 128u);
    let leading_left = mix_unsigned_shift(numerator.magnitude, left_shift, leading_count);
    let leading_right = mix_unsigned_shift(denominator.magnitude, right_shift, leading_count);
    if (mix_unsigned_compare(leading_left, leading_right) < 0) { exponent = exponent - 1; }
    if (exponent > 127) { return bitcast<f32>(sign | 0x7f800000u); }
    if (exponent < -150) { return bitcast<f32>(sign); }
    let subnormal = exponent < -126;
    let quantum = select(exponent - 23, -149, subnormal);
    let shift = numerator.origin - denominator.origin - quantum;
    let numerator_shift = u32(max(0, shift));
    let denominator_shift = u32(max(0, -shift));
    let required = max(numerator_bits + numerator_shift, denominator_bits + denominator_shift + 23u);
    let count = select(19u, 4u, required <= 128u);
    var remainder = mix_unsigned_shift(numerator.magnitude, numerator_shift, count);
    let divisor = mix_unsigned_shift(denominator.magnitude, denominator_shift, count);
    var significand = 0u;
    for (var bit = 23; bit >= 0; bit = bit - 1) {
        let candidate = mix_unsigned_shift(divisor, u32(bit), count);
        if (mix_unsigned_compare(remainder, candidate) >= 0) {
            remainder = mix_unsigned_subtract(remainder, candidate);
            significand = significand | (1u << u32(bit));
        }
    }
    let complement = mix_unsigned_subtract(divisor, remainder);
    let order = mix_unsigned_compare(remainder, complement);
    if (order > 0 || (order == 0 && (significand & 1u) != 0u)) { significand = significand + 1u; }
    if (subnormal) { return bitcast<f32>(sign | significand); }
    if (significand == 0x01000000u) {
        significand = significand >> 1u;
        exponent = exponent + 1;
    }
    if (exponent > 127) { return bitcast<f32>(sign | 0x7f800000u); }
    return bitcast<f32>(sign | (u32(exponent + 127) << 23u) | (significand & 0x007fffffu));
}

fn mix_adjustment_straight_alpha(
    base_px: vec4<f32>,
    processed_px: vec4<f32>,
    opacity: f32,
) -> vec4<f32> {
    // Preserve an invalid intermediate signal for the sole completed-frame
    // admission. Integer decoding must never reinterpret Inf/NaN as finite,
    // or hide invalid coverage through cancellation. Upload validation still
    // independently admits mutable host inputs before any GPU execution.
    let base_bits = bitcast<vec4<u32>>(base_px);
    let processed_bits = bitcast<vec4<u32>>(processed_px);
    if (any((base_bits & vec4<u32>(0x7f800000u)) == vec4<u32>(0x7f800000u))) { return base_px; }
    if (any((processed_bits & vec4<u32>(0x7f800000u)) == vec4<u32>(0x7f800000u))) { return processed_px; }
    let base_alpha_bits = base_bits.a & 0x7fffffffu;
    let source_alpha_bits = processed_bits.a & 0x7fffffffu;
    if (base_alpha_bits > 0x3f800000u || (base_bits.a >> 31u != 0u && base_alpha_bits != 0u)) { return base_px; }
    if (source_alpha_bits > 0x3f800000u || (processed_bits.a >> 31u != 0u && source_alpha_bits != 0u)) { return processed_px; }
    let amount_bits = bitcast<u32>(opacity) & 0x7fffffffu;
    if (amount_bits == 0u) { return base_px; }
    if (amount_bits == 0x3f800000u) { return processed_px; }
    // Numerical identity includes +0==-0. Canonicalize only comparison
    // zero bits; do not use floating comparisons that can flush tiny texels.
    // Returning the original left sample preserves its exact hidden signal.
    let identity_base = select(base_bits, vec4<u32>(0u),
        (base_bits & vec4<u32>(0x7fffffffu)) == vec4<u32>(0u));
    let identity_processed = select(processed_bits, vec4<u32>(0u),
        (processed_bits & vec4<u32>(0x7fffffffu)) == vec4<u32>(0u));
    if (all(identity_base == identity_processed)) { return base_px; }
    let base_a = mix_float_term(base_px.a);
    let source_a = mix_float_term(processed_px.a);
    let amount = mix_float_term(opacity);
    let alpha = mix_exact_sum(
        base_a,
        mix_multiply_term(source_a, amount),
        mix_negate_term(mix_multiply_term(base_a, amount)),
    );
    if (mix_unsigned_bits(alpha.magnitude) == 0u) { return vec4<f32>(0.0); }
    var output: vec4<f32>;
    output.a = mix_round_dyadic(alpha);
    for (var channel = 0u; channel < 3u; channel = channel + 1u) {
        let base = mix_multiply_term(mix_float_term(base_px[channel]), base_a);
        let source = mix_multiply_term(mix_float_term(processed_px[channel]), source_a);
        let associated = mix_exact_sum(
            base,
            mix_multiply_term(source, amount),
            mix_negate_term(mix_multiply_term(base, amount)),
        );
        output[channel] = mix_round_quotient(associated, alpha);
    }
    return output;
}

@fragment
fn fs_mix(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    // These matching-extent two inputs use their actual texel coordinates.
    let coordinate = vec2<i32>(floor(position.xy));
    return mix_adjustment_straight_alpha(
        textureLoad(accum_tex, coordinate, 0),
        textureLoad(layer_tex, coordinate, 0),
        uniforms.opacity,
    );
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
    /// A lowered spatial effect chain cannot run on the GPU for this layer.
    SpatialChainUnsupported,
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
            Self::SpatialChainUnsupported => "spatial_chain_unsupported",
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
            Self::SpatialChainUnsupported => {
                "Spatial effect chain has no GPU execution for this layer"
            }
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
    /// Opaque Normal composites recorded into one attachment without ping-pong.
    #[serde(default)]
    pub opaque_normal_single_accumulator_composites: u64,
    /// Per-pixel accumulator texture samples avoided by that single-attachment path.
    #[serde(default)]
    pub avoided_accumulator_sample_pixels: u64,
    /// Number of compositing operations that fell back to CPU compositing.
    pub cpu_fallback_composites: u64,
    /// Total pixels processed through GPU compositing.
    pub gpu_composited_pixels: u64,
    /// Total pixels processed through CPU compositing.
    pub cpu_composited_pixels: u64,
    /// Layers whose lowered spatial effect chain ran on the GPU.
    #[serde(default)]
    pub spatial_effect_layers: u64,
    /// Spatial texture passes recorded by those chains.
    #[serde(default)]
    pub spatial_effect_passes: u64,
    /// Standalone fused point stages recorded for those chains.
    #[serde(default)]
    pub spatial_point_stages: u64,
    /// Peak scratch textures one composite asked the spatial pool to retain.
    #[serde(default)]
    pub spatial_scratch_textures_peak: u32,
    /// Peak scratch bytes one composite asked the spatial pool to retain.
    #[serde(default)]
    pub spatial_scratch_bytes_peak: u64,
    /// Layers this composite recorded through an admitted spatial chain, i.e.
    /// the layers whose cost estimate chose the GPU and which then reached the
    /// compositor.
    #[serde(default)]
    pub spatial_chain_gpu_choices: u64,
    /// Layers a caller kept on the CPU because its cost estimate chose the CPU.
    ///
    /// The compositor never observes those layers, so this counter is filled by
    /// the owner that made the choice.
    #[serde(default)]
    pub spatial_chain_cpu_choices: u64,
    /// Estimated microseconds saved by those choices, summed over both sides.
    #[serde(default)]
    pub spatial_chain_estimated_savings_us: u64,
    /// Exact spatial execution evidence for GPU layer passes.
    #[serde(default)]
    pub execution: GpuCompositeExecutionDiagnostics,
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
        self.opaque_normal_single_accumulator_composites = self
            .opaque_normal_single_accumulator_composites
            .saturating_add(other.opaque_normal_single_accumulator_composites);
        self.avoided_accumulator_sample_pixels = self
            .avoided_accumulator_sample_pixels
            .saturating_add(other.avoided_accumulator_sample_pixels);
        self.cpu_fallback_composites =
            self.cpu_fallback_composites.saturating_add(other.cpu_fallback_composites);
        self.gpu_composited_pixels =
            self.gpu_composited_pixels.saturating_add(other.gpu_composited_pixels);
        self.cpu_composited_pixels =
            self.cpu_composited_pixels.saturating_add(other.cpu_composited_pixels);
        self.spatial_effect_layers =
            self.spatial_effect_layers.saturating_add(other.spatial_effect_layers);
        self.spatial_effect_passes =
            self.spatial_effect_passes.saturating_add(other.spatial_effect_passes);
        self.spatial_point_stages =
            self.spatial_point_stages.saturating_add(other.spatial_point_stages);
        self.spatial_scratch_textures_peak =
            self.spatial_scratch_textures_peak.max(other.spatial_scratch_textures_peak);
        self.spatial_scratch_bytes_peak =
            self.spatial_scratch_bytes_peak.max(other.spatial_scratch_bytes_peak);
        self.spatial_chain_gpu_choices =
            self.spatial_chain_gpu_choices.saturating_add(other.spatial_chain_gpu_choices);
        self.spatial_chain_cpu_choices =
            self.spatial_chain_cpu_choices.saturating_add(other.spatial_chain_cpu_choices);
        self.spatial_chain_estimated_savings_us = self
            .spatial_chain_estimated_savings_us
            .saturating_add(other.spatial_chain_estimated_savings_us);
        self.execution.accumulate(other.execution);
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
    /// CPU working-space frame uploaded to the policy-selected working texture.
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
    /// Canonical scene-linear pass policy. Ordinary sources use `Blend`;
    /// `Mix` is admitted only for an Adjustment of its original accumulator.
    pub blend_policy: mondrian_effects::EffectPassBlendPolicy,
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

/// One layer of a composite request whose effects may include spatial kernels.
#[derive(Debug, Clone, Copy)]
pub struct GpuCompositeSpatialLayer<'a> {
    /// The ordinary layer contract: source, opacity, blend, transform, frame seed.
    pub layer: GpuCompositeLayer<'a>,
    /// Lowered per-layer spatial effect chain, executed before the layer draw.
    ///
    /// An attached pipeline must run: admission failure is a hard error, never a
    /// silent CPU fallback inside the compositor.
    pub spatial_plan: Option<&'a CompiledEffectGpuPipeline>,
}

/// One independently admitted auxiliary raster owned by the GPU resource table.
#[derive(Debug, Clone, Copy)]
pub struct GpuSpatialAuxiliaryRaster<'a> {
    /// External slot named by the lowered chain.
    pub slot: u32,
    /// Retained working Float32 resource.
    pub frame: &'a GpuColorFrameHandle,
    /// Normalized affine mapping in `[a, b, tx, c, d, ty]` order.
    pub transform: [f32; 6],
    /// Host registration geometry.
    pub policy: AuxiliaryPolicy,
}

/// Request for recording a GPU composite whose layers may carry spatial chains.
pub struct GpuCompositeSpatialRequest<'a> {
    /// Output width.
    pub width: u32,
    /// Output height.
    pub height: u32,
    /// Working color space of the composite output.
    pub working_color_space: mondrian_core::WorkingColorSpace,
    /// Layers in bottom-to-top order.
    pub layers: &'a [GpuCompositeSpatialLayer<'a>],
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
    /// Frame-lifetime resets after ordered submission or abandonment.
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
    /// Spatial execution planning rejected an invalid request.
    #[error(transparent)]
    ExecutionPlan(#[from] GpuCompositeExecutionPlanError),
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
    /// An adjustment layer did not provide its declared GPU effect plan.
    #[error("GPU adjustment layer requires an effect plan")]
    AdjustmentMissingEffectPlan,
    /// A supplied pass weight is nonfinite or outside the normalized range.
    #[error("GPU composite opacity is not finite and normalized (bits {bits:#010x})")]
    InvalidOpacity {
        /// Original Float32 representation, preserving NaN payload and sign.
        bits: u32,
    },
    /// The compiled inline Mix did not own its required processed target.
    #[error("GPU inline Adjustment Mix is missing its processed target")]
    AdjustmentProcessedTargetMissing,
    /// Mix can interpolate only a processed Adjustment and its original base.
    #[error("GPU pass Mix requires an Adjustment source")]
    MixRequiresAdjustment,

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
    /// Alpha-mask execution requires the selected working texture format.
    #[error(
        "GPU alpha-mask pass requires the selected working texture format, got {texture_format:?}"
    )]
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
    /// An attached spatial effect chain failed while recording.
    #[error("GPU spatial effect chain failed in `{pass}`: {detail}")]
    SpatialChainFailed {
        /// Spatial pass or chain stage that refused.
        pass: &'static str,
        /// Full executor failure text.
        detail: String,
    },
    /// A layer carried both a fused point plan and a spatial chain.
    #[error("GPU composite layer cannot carry both an effect plan and a spatial chain")]
    SpatialPlanWithPointPlan,
    /// A spatial chain was attached to a source the driver cannot process.
    #[error("GPU spatial effect chain does not support source {source_kind}")]
    SpatialPlanOnUnsupportedSource {
        /// Rejected layer source kind.
        source_kind: &'static str,
    },
}

/// Runtime for recording GPU working-space composites.
pub struct GpuFrameCompositor {
    pipeline: wgpu::RenderPipeline,
    pipeline_layout: wgpu::PipelineLayout,
    adjustment_mix_pipeline: OnceLock<wgpu::RenderPipeline>,
    opaque_normal_pipeline: wgpu::RenderPipeline,
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
    spatial: EffectSpatialGpu,
    spatial_scratch: Mutex<GpuSpatialScratchPool>,
}

#[derive(Clone, Copy)]
enum GpuCompositeTextureBinding<'a> {
    Resource(&'a GpuColorFrameWgpuResource),
    /// A texture view that is not owned by the resource table, such as an
    /// intermediate spatial chain raster. Its bind group is never cached.
    TextureView(&'a wgpu::TextureView),
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
    extra2: [f32; 4],
    extra3: [f32; 4],
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
                    format: product_gpu_working_texture_format().to_wgpu(),
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
        let opaque_normal_pipeline =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("mondrian_gpu_working_compositor_opaque_normal_pipeline"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_opaque_normal"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: product_gpu_working_texture_format().to_wgpu(),
                        blend: Some(wgpu::BlendState {
                            color: wgpu::BlendComponent {
                                src_factor: wgpu::BlendFactor::SrcAlpha,
                                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                                operation: wgpu::BlendOperation::Add,
                            },
                            alpha: wgpu::BlendComponent {
                                src_factor: wgpu::BlendFactor::One,
                                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                                operation: wgpu::BlendOperation::Add,
                            },
                        }),
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
                    format: product_gpu_working_texture_format().to_wgpu(),
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
            format: product_gpu_working_texture_format().to_wgpu(),
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
            pipeline_layout: layout,
            adjustment_mix_pipeline: OnceLock::new(),
            opaque_normal_pipeline,
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
            spatial: EffectSpatialGpu::new(device),
            spatial_scratch: Mutex::new(GpuSpatialScratchPool::new()),
        })
    }

    /// Reset frame-local uniform slots after ordered submission or abandonment.
    /// An abandoned encoder must be discarded before another frame is recorded.
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

    /// Current residency of the spatial effect scratch pool.
    pub fn spatial_scratch_diagnostics(
        &self,
    ) -> crate::spatial_driver::GpuSpatialScratchPoolDiagnostics {
        self.spatial_scratch.lock().diagnostics()
    }

    /// Record a GPU working-space composite into the supplied command encoder
    /// and resource table.
    pub fn record(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        ids: &mut GpuColorFrameIdAllocator,
        table: &mut GpuColorFrameResourceTable<GpuColorFrameWgpuResource>,
        resource_pool: Option<&GpuColorFrameWgpuResourcePool>,
        request: GpuCompositeRequest<'_>,
    ) -> Result<GpuCompositeRecord, GpuCompositeError> {
        validate_request(&request)?;
        self.record_layers(
            device,
            queue,
            encoder,
            ids,
            table,
            resource_pool,
            &request,
            &|_| None,
            &[],
        )
    }

    /// Record a GPU working-space composite whose layers may each carry a
    /// lowered spatial effect chain.
    ///
    /// A layer with an attached chain runs that chain on its own straight-alpha
    /// working raster before its layer draw; a layer without one is recorded
    /// exactly as [`Self::record`] records it.
    pub fn record_with_spatial_effect_chains(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        ids: &mut GpuColorFrameIdAllocator,
        table: &mut GpuColorFrameResourceTable<GpuColorFrameWgpuResource>,
        resource_pool: Option<&GpuColorFrameWgpuResourcePool>,
        request: GpuCompositeSpatialRequest<'_>,
    ) -> Result<GpuCompositeRecord, GpuCompositeError> {
        self.record_with_auxiliary_spatial_effect_chains(
            device,
            queue,
            encoder,
            ids,
            table,
            resource_pool,
            request,
            &[],
        )
    }

    /// Record spatial chains with auxiliary rasters indexed by layer and slot.
    /// Every required binding is validated before recording any layer.
    #[allow(clippy::too_many_arguments)]
    pub fn record_with_auxiliary_spatial_effect_chains(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        ids: &mut GpuColorFrameIdAllocator,
        table: &mut GpuColorFrameResourceTable<GpuColorFrameWgpuResource>,
        resource_pool: Option<&GpuColorFrameWgpuResourcePool>,
        request: GpuCompositeSpatialRequest<'_>,
        auxiliary_rasters: &[&[GpuSpatialAuxiliaryRaster<'_>]],
    ) -> Result<GpuCompositeRecord, GpuCompositeError> {
        validate_spatial_request(&request)?;
        let mut layers = Vec::with_capacity(request.layers.len());
        for entry in request.layers {
            layers.push(entry.layer);
        }
        let plain = GpuCompositeRequest {
            width: request.width,
            height: request.height,
            working_color_space: request.working_color_space,
            layers: &layers,
        };
        validate_request(&plain)?;
        let chains = prepare_spatial_chains(&request, auxiliary_rasters, table)?;
        self.record_layers(
            device,
            queue,
            encoder,
            ids,
            table,
            resource_pool,
            &plain,
            &|index| chains[index].as_ref(),
            auxiliary_rasters,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn record_layers<'a>(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        ids: &mut GpuColorFrameIdAllocator,
        table: &mut GpuColorFrameResourceTable<GpuColorFrameWgpuResource>,
        resource_pool: Option<&GpuColorFrameWgpuResourcePool>,
        request: &GpuCompositeRequest<'a>,
        spatial_chain_at: &dyn Fn(usize) -> Option<&'a GpuSpatialChainPlan>,
        auxiliary_rasters: &[&[GpuSpatialAuxiliaryRaster<'_>]],
    ) -> Result<GpuCompositeRecord, GpuCompositeError> {
        let has_spatial_plan =
            (0..request.layers.len()).any(|index| spatial_chain_at(index).is_some());
        // A spatial chain changes coverage in ways the passthrough fast path
        // cannot prove, so it must never fire while a chain is attached.
        if !has_spatial_plan && let Some(output) = single_layer_gpu_passthrough(request) {
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
        let footprints = request
            .layers
            .iter()
            .map(|layer| composite_layer_footprint(width, height, layer))
            .collect::<Vec<_>>();
        let execution = GpuCompositeExecutionPlanner::plan(
            width,
            height,
            &footprints,
            GpuCompositeExecutionPolicy::default(),
        )?;
        let output_descriptor = ColorFrameDescriptor {
            width,
            height,
            color_space: request.working_color_space.into(),
            domain: ColorFrameDomain::Working,
            encoding: ColorFrameEncoding::LinearFloat,
            residency: ColorFrameResidency::Gpu,
            alpha: composite_output_alpha(request, |index| spatial_chain_at(index).is_some()),
        };
        let opaque_normal_accumulator =
            can_use_opaque_normal_accumulator(request, &execution, has_spatial_plan);
        let target_a = create_working_resource(
            device,
            ids,
            output_descriptor,
            "gpu-composite-accum-a",
            resource_pool,
        )?;
        let target_b = if opaque_normal_accumulator {
            None
        } else {
            Some(create_working_resource(
                device,
                ids,
                output_descriptor,
                "gpu-composite-accum-b",
                resource_pool,
            )?)
        };
        // A first layer blends over canonical transparent black. Use the
        // existing immutable zero texture instead of clearing an accumulator
        // that is otherwise unused until a later layer writes it.
        if !opaque_normal_accumulator && execution.layers.is_empty() {
            clear_working_texture(
                encoder,
                &target_a.resource().texture_view,
                wgpu::Color::TRANSPARENT,
            );
        }

        let adjustment_processed = if execution.layers.iter().any(|execution| {
            let layer = &request.layers[execution.input_index];
            matches!(layer.source, GpuCompositeLayerSource::Adjustment)
                && layer.blend_policy == mondrian_effects::EffectPassBlendPolicy::Mix
        }) {
            Some(create_working_resource(
                device,
                ids,
                ColorFrameDescriptor {
                    alpha: crate::ColorFrameAlpha::StraightCoverage,
                    ..output_descriptor
                },
                "gpu-adjustment-processed",
                resource_pool,
            )?)
        } else {
            None
        };

        let mut transient_uploads = Vec::new();
        let mut uploaded_cpu_layers = false;
        let mut data_texture_uploads = 0_u64;
        let mut src_is_a = true;
        let mut spatial_effect_layers = 0_u64;
        let mut spatial_effect_passes = 0_u64;
        let mut spatial_point_stages = 0_u64;
        let mut spatial_scratch_textures_peak = 0_u32;
        let mut spatial_scratch_bytes_peak = 0_u64;
        let mut spatial_chain_gpu_choices = 0_u64;
        for (pass_index, layer_execution) in execution.layers.iter().enumerate() {
            let index = layer_execution.input_index;
            let layer = &request.layers[index];
            let (accum, dst) = if opaque_normal_accumulator {
                (&target_a, &target_a)
            } else if src_is_a {
                (
                    &target_a,
                    target_b.as_ref().expect("ping-pong target must exist"),
                )
            } else {
                (
                    target_b.as_ref().expect("ping-pong target must exist"),
                    &target_a,
                )
            };
            let (mut layer_binding, source_kind, solid_color, source_size) = match layer.source {
                GpuCompositeLayerSource::CpuFrame(frame) => {
                    uploaded_cpu_layers = true;
                    let descriptor = frame.descriptor();
                    let upload = GpuColorFrameUploadPlan::from_cpu_color_frame(
                        ids.allocate()?,
                        frame,
                        product_gpu_working_texture_format(),
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
            if let Some(chain) = spatial_chain_at(index) {
                let source_descriptor = layer_source_descriptor(layer.source).ok_or(
                    GpuCompositeError::SpatialPlanOnUnsupportedSource {
                        source_kind: composite_layer_source_name(layer.source),
                    },
                )?;
                // Clone the sampled texture, its view, and the resource table's
                // own handle out of the binding before the chain inserts its
                // intermediates, so no immutable and mutable table borrow
                // overlap.
                let (source_texture, source_view) = match layer_binding {
                    GpuCompositeTextureBinding::Resource(resource) => {
                        (resource.texture.clone(), resource.texture_view.clone())
                    }
                    GpuCompositeTextureBinding::TextureView(_)
                    | GpuCompositeTextureBinding::ProceduralDummy => {
                        return Err(GpuCompositeError::SpatialPlanOnUnsupportedSource {
                            source_kind: composite_layer_source_name(layer.source),
                        });
                    }
                };
                let chain_output = self.record_spatial_chain(
                    device,
                    queue,
                    encoder,
                    ids,
                    table,
                    resource_pool,
                    &source_texture,
                    &source_view,
                    source_descriptor,
                    chain,
                    layer.frame_seed,
                    auxiliary_rasters.get(index).copied().unwrap_or_default(),
                )?;
                layer_binding = GpuCompositeTextureBinding::Resource(
                    table.get(&chain_output).map_err(GpuCompositeError::ResourceTable)?.resource(),
                );
                spatial_effect_layers = spatial_effect_layers.saturating_add(1);
                spatial_chain_gpu_choices = spatial_chain_gpu_choices.saturating_add(1);
                spatial_effect_passes =
                    spatial_effect_passes.saturating_add(u64::from(chain.spatial_passes));
                spatial_point_stages =
                    spatial_point_stages.saturating_add(u64::from(chain.point_stages));
                spatial_scratch_textures_peak =
                    spatial_scratch_textures_peak.max(chain.scratch_texture_request());
                spatial_scratch_bytes_peak = spatial_scratch_bytes_peak.max(chain.scratch_bytes);
            }
            let inv_transform = invert_affine(layer.transform)
                .expect("validate_request rejects unsupported transforms");
            if !opaque_normal_accumulator && !layer_execution.initializes_accumulator {
                record_preserved_regions(
                    encoder,
                    &accum.resource().texture,
                    &dst.resource().texture,
                    &layer_execution.preserved_regions,
                );
            }
            let (layer_binding, effect_plan, pipeline) =
                if matches!(layer.source, GpuCompositeLayerSource::Adjustment,)
                    && layer.blend_policy == mondrian_effects::EffectPassBlendPolicy::Mix
                {
                    let processed = adjustment_processed
                        .as_ref()
                        .ok_or(GpuCompositeError::AdjustmentProcessedTargetMissing)?;
                    self.record_layer_pass(
                        device,
                        queue,
                        encoder,
                        GpuCompositeTextureBinding::ProceduralDummy,
                        &processed.resource().texture_view,
                        GpuCompositeTextureBinding::Resource(accum.resource()),
                        GpuCompositeUniforms {
                            opacity: 1.0,
                            source_kind: 3,
                            effect_count: 0,
                            blend_mode: gpu_blend_mode_id(BlendMode::Normal),
                            frame_seed_lo: layer.frame_seed as u32,
                            frame_seed_hi: (layer.frame_seed >> 32) as u32,
                            mask_controls: [0; 2],
                            solid_color: [0.0; 4],
                            inv_transform0: [1.0, 0.0, 0.0, 0.0],
                            inv_transform1: [1.0, 0.0, 0.0, 0.0],
                            geometry: [width as f32, height as f32, width as f32, height as f32],
                            effects: [GpuEffectUniform::zeroed(); MAX_FUSED_GPU_EFFECT_OPS],
                        },
                        layer.effect_plan,
                    )?;
                    (
                        GpuCompositeTextureBinding::Resource(processed.resource()),
                        None,
                        self.adjustment_mix_pipeline(device),
                    )
                } else {
                    (
                        layer_binding,
                        layer.effect_plan,
                        if opaque_normal_accumulator {
                            &self.opaque_normal_pipeline
                        } else {
                            &self.pipeline
                        },
                    )
                };
            self.record_layer_pass_scheduled(
                device,
                queue,
                encoder,
                if opaque_normal_accumulator || layer_execution.initializes_accumulator {
                    GpuCompositeTextureBinding::ProceduralDummy
                } else {
                    GpuCompositeTextureBinding::Resource(accum.resource())
                },
                &dst.resource().texture_view,
                layer_binding,
                GpuCompositeUniforms {
                    opacity: layer.opacity,
                    source_kind,
                    effect_count: layer
                        .effect_plan
                        .map_or(0, |plan| plan.operations().len() as u32),
                    blend_mode: gpu_pass_policy_id(layer.blend_policy),
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
                effect_plan.map(CompiledEffectGpuPlan::operations),
                layer_execution,
                pipeline,
                opaque_normal_accumulator.then_some(if pass_index == 0 {
                    wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)
                } else {
                    wgpu::LoadOp::Load
                }),
            )?;
            if !opaque_normal_accumulator {
                src_is_a = !src_is_a;
            }
        }

        let (output_resource, retained_resource) = if opaque_normal_accumulator {
            (target_a, None)
        } else if src_is_a {
            (
                target_a,
                Some(target_b.expect("ping-pong target must exist")),
            )
        } else {
            (
                target_b.expect("ping-pong target must exist"),
                Some(target_a),
            )
        };
        if let Some(processed) = adjustment_processed {
            table.insert(processed).map_err(GpuCompositeError::ResourceTable)?;
        }
        let output = output_resource.handle().clone();
        if let Some(retained_resource) = retained_resource {
            table.insert(retained_resource).map_err(GpuCompositeError::ResourceTable)?;
        }
        table.insert(output_resource).map_err(GpuCompositeError::ResourceTable)?;
        let mut diagnostics = GpuCompositingDiagnostics {
            gpu_composited_pixels: u64::from(width).saturating_mul(u64::from(height)),
            execution: execution.diagnostics,
            ..GpuCompositingDiagnostics::default()
        };
        if uploaded_cpu_layers {
            diagnostics.gpu_with_upload_composites = 1;
        } else {
            diagnostics.gpu_native_composites = 1;
        }
        diagnostics.data_texture_uploads = data_texture_uploads;
        diagnostics.spatial_effect_layers = spatial_effect_layers;
        diagnostics.spatial_effect_passes = spatial_effect_passes;
        diagnostics.spatial_point_stages = spatial_point_stages;
        diagnostics.spatial_scratch_textures_peak = spatial_scratch_textures_peak;
        diagnostics.spatial_scratch_bytes_peak = spatial_scratch_bytes_peak;
        diagnostics.spatial_chain_gpu_choices = spatial_chain_gpu_choices;
        if opaque_normal_accumulator {
            diagnostics.opaque_normal_single_accumulator_composites = 1;
            diagnostics.avoided_accumulator_sample_pixels = execution.diagnostics.shaded_pixels;
        }
        Ok(GpuCompositeRecord { output, diagnostics })
    }

    /// Run one lowered spatial chain over a layer's own straight-alpha raster.
    ///
    /// Every stage reads the previous stage's raster and writes a fresh table
    /// resource, so the chain never aliases a pass's source with its own
    /// destination and the layer draw can sample the chain output unchanged.
    #[allow(clippy::too_many_arguments)]
    fn record_spatial_chain(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        ids: &mut GpuColorFrameIdAllocator,
        table: &mut GpuColorFrameResourceTable<GpuColorFrameWgpuResource>,
        resource_pool: Option<&GpuColorFrameWgpuResourcePool>,
        source_texture: &wgpu::Texture,
        source_view: &wgpu::TextureView,
        descriptor: ColorFrameDescriptor,
        chain: &GpuSpatialChainPlan,
        frame_seed: i64,
        auxiliary_rasters: &[GpuSpatialAuxiliaryRaster<'_>],
    ) -> Result<GpuColorFrameHandle, GpuCompositeError> {
        let width = descriptor.width;
        let height = descriptor.height;
        // The chain writes working rasters, while the source descriptor may
        // still describe a CPU-resident upload; only the extent and colour
        // space carry over.
        let working_descriptor = ColorFrameDescriptor {
            domain: ColorFrameDomain::Working,
            encoding: ColorFrameEncoding::LinearFloat,
            residency: ColorFrameResidency::Gpu,
            alpha: ColorFrameAlpha::StraightCoverage,
            ..descriptor
        };
        let mut current_texture = source_texture.clone();
        let mut current_view = source_view.clone();
        let mut current_handle = None;
        for stage in &chain.stages {
            match stage {
                GpuSpatialStagePlan::PointOps { operations } => {
                    let output = create_working_resource(
                        device,
                        ids,
                        working_descriptor,
                        "gpu-spatial-point",
                        resource_pool,
                    )?;
                    self.record_point_ops_pass(
                        device,
                        queue,
                        encoder,
                        &current_view,
                        &output.resource().texture_view,
                        operations,
                        width,
                        height,
                        frame_seed,
                    )?;
                    let handle = output.handle().clone();
                    table.insert(output).map_err(GpuCompositeError::ResourceTable)?;
                    current_texture = table
                        .get(&handle)
                        .map_err(GpuCompositeError::ResourceTable)?
                        .resource()
                        .texture
                        .clone();
                    current_view = table
                        .get(&handle)
                        .map_err(GpuCompositeError::ResourceTable)?
                        .resource()
                        .texture_view
                        .clone();
                    current_handle = Some(handle);
                }
                GpuSpatialStagePlan::Spatial { pass, scratch_textures, .. } => {
                    // The pool holds `max(pass count) + 1` textures so no
                    // stage's destination aliases its own source; the guard
                    // here proves the kernel's declared pass count fits.
                    let scratch = {
                        let mut pool = self.spatial_scratch.lock();
                        pool.acquire(device, width, height, chain.scratch_texture_request())
                    };
                    if scratch.len() < usize::try_from(*scratch_textures).unwrap_or(usize::MAX) {
                        return Err(GpuCompositeError::SpatialChainFailed {
                            pass: "spatial_scratch_geometry",
                            detail: format!(
                                "stage needs {scratch_textures} scratch texture(s) but the pool \
                                 supplied {}",
                                scratch.len()
                            ),
                        });
                    }
                    let produced = if let Some(external_slot) = pass.auxiliary_slot() {
                        let binding = auxiliary_rasters
                            .iter()
                            .find(|binding| Some(binding.slot) == external_slot)
                            .ok_or_else(|| GpuCompositeError::SpatialChainFailed {
                                pass: "spatial_auxiliary_missing",
                                detail: format!(
                                    "auxiliary slot {external_slot:?} lost its binding"
                                ),
                            })?;
                        let resource =
                            table.get(binding.frame).map_err(GpuCompositeError::ResourceTable)?;
                        self.spatial.record_auxiliary(
                            device,
                            encoder,
                            pass,
                            &current_texture,
                            &resource.resource().texture,
                            &scratch[0],
                            binding.transform,
                        )
                    } else {
                        self.spatial.record(
                            device,
                            encoder,
                            pass,
                            &current_texture,
                            &scratch,
                            width,
                            height,
                        )
                    }
                    .map_err(|error| {
                        GpuCompositeError::SpatialChainFailed {
                            pass: spatial_failure_pass(&error),
                            detail: error.to_string(),
                        }
                    })?;
                    let output = create_working_resource(
                        device,
                        ids,
                        working_descriptor,
                        "gpu-spatial-pass",
                        resource_pool,
                    )?;
                    // `record` returns one of the rotating scratch textures,
                    // whose identity depends on the kernel's pass table; copy
                    // it out so every stage hands a table resource to the next
                    // one and to the layer draw.
                    encoder.copy_texture_to_texture(
                        wgpu::TexelCopyTextureInfo {
                            texture: &produced,
                            mip_level: 0,
                            origin: wgpu::Origin3d::ZERO,
                            aspect: wgpu::TextureAspect::All,
                        },
                        wgpu::TexelCopyTextureInfo {
                            texture: &output.resource().texture,
                            mip_level: 0,
                            origin: wgpu::Origin3d::ZERO,
                            aspect: wgpu::TextureAspect::All,
                        },
                        wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                    );
                    let handle = output.handle().clone();
                    table.insert(output).map_err(GpuCompositeError::ResourceTable)?;
                    current_texture = table
                        .get(&handle)
                        .map_err(GpuCompositeError::ResourceTable)?
                        .resource()
                        .texture
                        .clone();
                    current_view = table
                        .get(&handle)
                        .map_err(GpuCompositeError::ResourceTable)?
                        .resource()
                        .texture_view
                        .clone();
                    current_handle = Some(handle);
                }
            }
        }
        current_handle.ok_or(GpuCompositeError::SpatialChainFailed {
            pass: "spatial_chain",
            detail: "the spatial chain produced no raster".to_string(),
        })
    }

    /// Record a standalone full-raster point-op pass over a chain intermediate.
    #[allow(clippy::too_many_arguments)]
    fn record_point_ops_pass(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        source_view: &wgpu::TextureView,
        destination_view: &wgpu::TextureView,
        operations: &[EffectGpuPointOp],
        width: u32,
        height: u32,
        frame_seed: i64,
    ) -> Result<(), GpuCompositeError> {
        let canvas = GpuCompositeRect { x: 0, y: 0, width, height };
        let execution = GpuCompositeLayerExecution {
            input_index: 0,
            damage: canvas,
            preserved_regions: Vec::new(),
            tiles: vec![canvas],
            initializes_accumulator: true,
            fused_point_operations: operations.len() as u32,
        };
        self.record_layer_pass_scheduled(
            device,
            queue,
            encoder,
            GpuCompositeTextureBinding::ProceduralDummy,
            destination_view,
            GpuCompositeTextureBinding::TextureView(source_view),
            GpuCompositeUniforms {
                opacity: 1.0,
                source_kind: 3,
                effect_count: operations.len() as u32,
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
            Some(operations),
            &execution,
            &self.pipeline,
            None,
        )
    }

    /// Record a standalone point-effect pass over a typed effect-domain frame.
    ///
    /// The pass does not blend or relabel pixels. Renderer-owned OCIO passes
    /// are responsible for entering and leaving this exact intermediate domain.
    pub fn record_point_effect_pass(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
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
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
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
                blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_plan: None,
                frame_seed: 0,
            },
            GpuCompositeLayer {
                source: GpuCompositeLayerSource::GpuFrame(right),
                opacity: 1.0,
                blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
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

    /// Apply the canonical pass policy to a processed frame and its original accumulator.
    ///
    /// `Mix` interpolates associated scene-linear RGB and coverage. `Blend(mode)`
    /// performs explicit compositing, including Normal Over for graph Blend nodes.
    ///
    /// Unlike a two-layer general composite, this graph node samples the base
    /// accumulator directly and therefore records one full-frame pass and one
    /// output allocation.
    #[allow(clippy::too_many_arguments)]
    pub fn record_adjustment_blend_pass(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        ids: &mut GpuColorFrameIdAllocator,
        table: &mut GpuColorFrameResourceTable<GpuColorFrameWgpuResource>,
        resource_pool: Option<&GpuColorFrameWgpuResourcePool>,
        base: &GpuColorFrameHandle,
        processed: &GpuColorFrameHandle,
        opacity: f32,
        blend_policy: mondrian_effects::EffectPassBlendPolicy,
        frame_seed: i64,
        working_color_space: mondrian_core::WorkingColorSpace,
    ) -> Result<GpuCompositeRecord, GpuCompositeError> {
        let validation_layers = [
            GpuCompositeLayer {
                source: GpuCompositeLayerSource::GpuFrame(base),
                opacity: 1.0,
                blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_plan: None,
                frame_seed: 0,
            },
            GpuCompositeLayer {
                source: GpuCompositeLayerSource::GpuFrame(processed),
                opacity,
                // These layers admit the two input descriptors, not pass semantics.
                // The actual typed policy is consumed once by the shader below.
                blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_plan: None,
                frame_seed: 0,
            },
        ];
        let mut descriptor = base.descriptor();
        descriptor.alpha = adjustment_output_alpha(
            descriptor.alpha,
            processed.descriptor().alpha,
            opacity,
            blend_policy,
        );
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
        self.record_layer_pass_with_pipeline(
            device,
            queue,
            encoder,
            GpuCompositeTextureBinding::Resource(base_resource.resource()),
            &output_resource.resource().texture_view,
            GpuCompositeTextureBinding::Resource(processed_resource.resource()),
            GpuCompositeUniforms {
                opacity,
                source_kind: 0,
                effect_count: 0,
                blend_mode: gpu_pass_policy_id(blend_policy),
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
            match blend_policy {
                mondrian_effects::EffectPassBlendPolicy::Mix => {
                    self.adjustment_mix_pipeline(device)
                }
                mondrian_effects::EffectPassBlendPolicy::Blend(_) => &self.pipeline,
            },
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
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
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

    /// Record an auxiliary operation over two graph-local materializations.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_graph_auxiliary_pass(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        ids: &mut GpuColorFrameIdAllocator,
        table: &mut GpuColorFrameResourceTable<GpuColorFrameWgpuResource>,
        resource_pool: Option<&GpuColorFrameWgpuResourcePool>,
        input: &GpuColorFrameHandle,
        auxiliary: &GpuColorFrameHandle,
        pass: &EffectGpuSpatialPass,
        working_color_space: mondrian_core::WorkingColorSpace,
    ) -> Result<GpuColorFrameHandle, GpuCompositeError> {
        if pass.auxiliary_slot() != Some(None) {
            return Err(GpuCompositeError::SpatialChainFailed {
                pass: pass.name(),
                detail: "expected a graph-local auxiliary pass".to_owned(),
            });
        }
        if matches!(pass, EffectGpuSpatialPass::FillMatte { .. }) {
            validate_alpha_mask_inputs(input, auxiliary, working_color_space)?;
        } else {
            let expected = ColorFrameDescriptor {
                color_space: working_color_space.into(),
                domain: ColorFrameDomain::Working,
                encoding: ColorFrameEncoding::LinearFloat,
                residency: ColorFrameResidency::Gpu,
                alpha: crate::ColorFrameAlpha::StraightCoverage,
                ..input.descriptor()
            };
            for frame in [input, auxiliary] {
                let actual = frame.descriptor();
                if actual != expected {
                    return Err(GpuCompositeError::SourceDescriptorMismatch { expected, actual });
                }
                if frame.texture_format() != product_gpu_working_texture_format() {
                    return Err(GpuCompositeError::SpatialChainFailed {
                        pass: pass.name(),
                        detail: "expected working Float32 textures".to_owned(),
                    });
                }
            }
        }
        let input_resource = table.get(input).map_err(GpuCompositeError::ResourceTable)?;
        let auxiliary_resource = table.get(auxiliary).map_err(GpuCompositeError::ResourceTable)?;
        let output = create_working_resource(
            device,
            ids,
            input.descriptor(),
            "gpu-graph-auxiliary-output",
            resource_pool,
        )?;
        self.spatial
            .record_auxiliary(
                device,
                encoder,
                pass,
                &input_resource.resource().texture,
                &auxiliary_resource.resource().texture,
                &output.resource().texture,
                [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            )
            .map_err(|error| GpuCompositeError::SpatialChainFailed {
                pass: spatial_failure_pass(&error),
                detail: error.to_string(),
            })?;
        let handle = output.handle().clone();
        table.insert(output).map_err(GpuCompositeError::ResourceTable)?;
        Ok(handle)
    }

    /// Combine two AlphaMask-domain textures without crossing into picture RGB.
    #[allow(clippy::too_many_arguments)]
    pub fn record_alpha_mask_combine_pass(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
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
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
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
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
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
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
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
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
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

    fn adjustment_mix_pipeline(&self, device: &wgpu::Device) -> &wgpu::RenderPipeline {
        self.adjustment_mix_pipeline.get_or_init(|| {
            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("mondrian_gpu_adjustment_mix_shader"),
                source: wgpu::ShaderSource::Wgsl(GPU_ADJUSTMENT_MIX_SHADER.into()),
            });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("mondrian_gpu_adjustment_mix_pipeline"),
                layout: Some(&self.pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_mix"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: product_gpu_working_texture_format().to_wgpu(),
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
            })
        })
    }

    fn record_layer_pass(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        accum_binding: GpuCompositeTextureBinding<'_>,
        dst_view: &wgpu::TextureView,
        layer_binding: GpuCompositeTextureBinding<'_>,
        uniforms: GpuCompositeUniforms,
        effect_plan: Option<&CompiledEffectGpuPlan>,
    ) -> Result<(), GpuCompositeError> {
        self.record_layer_pass_with_pipeline(
            device,
            queue,
            encoder,
            accum_binding,
            dst_view,
            layer_binding,
            uniforms,
            effect_plan,
            &self.pipeline,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn record_layer_pass_with_pipeline(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        accum_binding: GpuCompositeTextureBinding<'_>,
        dst_view: &wgpu::TextureView,
        layer_binding: GpuCompositeTextureBinding<'_>,
        uniforms: GpuCompositeUniforms,
        effect_plan: Option<&CompiledEffectGpuPlan>,
        pipeline: &wgpu::RenderPipeline,
    ) -> Result<(), GpuCompositeError> {
        let width = uniforms.geometry[0].max(0.0) as u32;
        let height = uniforms.geometry[1].max(0.0) as u32;
        let full_canvas = GpuCompositeRect { x: 0, y: 0, width, height };
        let execution = GpuCompositeLayerExecution {
            input_index: 0,
            damage: full_canvas,
            preserved_regions: Vec::new(),
            tiles: vec![full_canvas],
            initializes_accumulator: true,
            fused_point_operations: effect_plan.map_or(0, |plan| plan.operations().len() as u32),
        };
        self.record_layer_pass_scheduled(
            device,
            queue,
            encoder,
            accum_binding,
            dst_view,
            layer_binding,
            uniforms,
            effect_plan.map(CompiledEffectGpuPlan::operations),
            &execution,
            pipeline,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn record_layer_pass_scheduled(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        accum_binding: GpuCompositeTextureBinding<'_>,
        dst_view: &wgpu::TextureView,
        layer_binding: GpuCompositeTextureBinding<'_>,
        mut uniforms: GpuCompositeUniforms,
        point_operations: Option<&[EffectGpuPointOp]>,
        execution: &GpuCompositeLayerExecution,
        pipeline: &wgpu::RenderPipeline,
        load_op: Option<wgpu::LoadOp<wgpu::Color>>,
    ) -> Result<(), GpuCompositeError> {
        let operations = point_operations.unwrap_or_default();
        let creative_lut_binding = self.creative_luts.prepare_ops(device, queue, operations)?;
        uniforms.effect_count = operations.len() as u32;
        uniforms.effects = effect_uniforms_for_ops(operations, Some(&creative_lut_binding))?;
        // Test signals places its geometry from the frame extent, but
        // `effect_uniforms_for_ops` has no extent in scope. Stamp it here, where
        // the composite path and `record_point_effect_pass` converge: the point
        // uniform's `extra0` is `[width, height, 0, 0]` for that op, exactly
        // as `TestSignalsGrade::gpu_controls` documents.
        {
            let source_extent = [uniforms.geometry[2], uniforms.geometry[3], 0.0, 0.0];
            for (uniform, operation) in uniforms.effects.iter_mut().zip(operations) {
                if matches!(operation, EffectGpuPointOp::TestSignals { .. }) {
                    uniform.extra0 = source_extent;
                }
            }
        }
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
            GpuCompositeTextureBinding::TextureView(texture_view) => {
                self.record_texture_binding_cache_result(false);
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
            GpuCompositeTextureBinding::TextureView(texture_view) => {
                self.record_texture_binding_cache_result(false);
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("mondrian_gpu_working_compositor_accum_binding"),
                    layout: &self.accum_texture_layout,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(texture_view),
                    }],
                })
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
                    load: load_op.unwrap_or({
                        if execution.initializes_accumulator
                            || execution.preserved_regions.is_empty()
                        {
                            wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)
                        } else {
                            wgpu::LoadOp::Load
                        }
                    }),
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &layer_bind_group, &[]);
        pass.set_bind_group(1, &accum_bind_group, &[]);
        pass.set_bind_group(2, &uniform_bind_group, &[uniform_offset as u32]);
        pass.set_bind_group(3, creative_lut_binding.bind_group(), &[]);
        for tile in &execution.tiles {
            pass.set_scissor_rect(tile.x, tile.y, tile.width, tile.height);
            pass.draw(0..4, 0..1);
        }
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
        if texture_format != product_gpu_working_texture_format() {
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
        if texture_format != product_gpu_working_texture_format() {
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
        if texture_format != product_gpu_working_texture_format() {
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
    if input.texture_format() != product_gpu_working_texture_format() {
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
    if input.texture_format() != product_gpu_working_texture_format() {
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
        extra2: [0.0; 4],
        extra3: [0.0; 4],
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
    request: &GpuCompositeRequest<'a>,
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
    // Straight coverage must still composite over transparent black at exact zero alpha.
    let preserves_pixels = layer.opacity.clamp(0.0, 1.0) == 1.0
        && layer.blend_policy == mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal)
        && is_identity_transform(layer.transform)
        && layer.effect_plan.is_none_or(CompiledEffectGpuPlan::is_identity)
        && descriptor.alpha == ColorFrameAlpha::Opaque
        && descriptor.width == request.width
        && descriptor.height == request.height
        && handle.texture_format() == product_gpu_working_texture_format();
    preserves_pixels.then_some(handle)
}

fn adjustment_output_alpha(
    base: crate::ColorFrameAlpha,
    processed: crate::ColorFrameAlpha,
    opacity: f32,
    policy: mondrian_effects::EffectPassBlendPolicy,
) -> crate::ColorFrameAlpha {
    if opacity == 0.0 {
        return base;
    }
    match policy {
        mondrian_effects::EffectPassBlendPolicy::Mix if opacity == 1.0 => processed,
        mondrian_effects::EffectPassBlendPolicy::Mix
            if base == crate::ColorFrameAlpha::Opaque
                && processed == crate::ColorFrameAlpha::Opaque =>
        {
            crate::ColorFrameAlpha::Opaque
        }
        mondrian_effects::EffectPassBlendPolicy::Mix => crate::ColorFrameAlpha::StraightCoverage,
        mondrian_effects::EffectPassBlendPolicy::Blend(_) => base,
    }
}

fn gpu_pass_policy_id(policy: mondrian_effects::EffectPassBlendPolicy) -> u32 {
    match policy {
        mondrian_effects::EffectPassBlendPolicy::Mix => 27,
        mondrian_effects::EffectPassBlendPolicy::Blend(mode) => gpu_blend_mode_id(mode),
    }
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

/// Plan-level wrapper kept for the uniform-packing tests; production paths hold
/// an operation slice and call [`effect_uniforms_for_ops`].
#[cfg(test)]
fn effect_uniforms(
    plan: Option<&CompiledEffectGpuPlan>,
    creative_luts: Option<&GpuCreativeLutPreparedBinding>,
) -> Result<[GpuEffectUniform; MAX_FUSED_GPU_EFFECT_OPS], GpuCompositeError> {
    effect_uniforms_for_ops(
        plan.map_or(&[], CompiledEffectGpuPlan::operations),
        creative_luts,
    )
}

fn effect_uniforms_for_ops(
    operations: &[EffectGpuPointOp],
    creative_luts: Option<&GpuCreativeLutPreparedBinding>,
) -> Result<[GpuEffectUniform; MAX_FUSED_GPU_EFFECT_OPS], GpuCompositeError> {
    let mut uniforms = [GpuEffectUniform::zeroed(); MAX_FUSED_GPU_EFFECT_OPS];
    for (target, operation) in uniforms.iter_mut().zip(operations) {
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
                extra2: [0.0; 4],
                extra3: [0.0; 4],
            },
            EffectGpuPointOp::WhiteBalance { grade } => {
                let matrix = grade.matrix();
                GpuEffectUniform {
                    header: [6, 0, 0, 0],
                    params: [matrix[0][0], matrix[0][1], matrix[0][2], 0.0],
                    color: [matrix[1][0], matrix[1][1], matrix[1][2], 0.0],
                    extra0: [matrix[2][0], matrix[2][1], matrix[2][2], 0.0],
                    extra1: [0.0; 4],
                    extra2: [0.0; 4],
                    extra3: [0.0; 4],
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
                    extra2: [0.0; 4],
                    extra3: [0.0; 4],
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
                    extra2: [0.0; 4],
                    extra3: [0.0; 4],
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
                    extra2: [0.0; 4],
                    extra3: [0.0; 4],
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
                    extra2: [0.0; 4],
                    extra3: [0.0; 4],
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
                    extra2: [0.0; 4],
                    extra3: [0.0; 4],
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
                    extra2: [0.0; 4],
                    extra3: [0.0; 4],
                }
            }
            EffectGpuPointOp::Vignette { intensity, feather } => GpuEffectUniform {
                header: [3, 0, 0, 0],
                params: [*intensity, *feather, 0.0, 0.0],
                color: [0.0; 4],
                extra0: [0.0; 4],
                extra1: [0.0; 4],
                extra2: [0.0; 4],
                extra3: [0.0; 4],
            },
            EffectGpuPointOp::Grain { amount } => GpuEffectUniform {
                header: [4, 0, 0, 0],
                params: [*amount, 0.0, 0.0, 0.0],
                color: [0.0; 4],
                extra0: [0.0; 4],
                extra1: [0.0; 4],
                extra2: [0.0; 4],
                extra3: [0.0; 4],
            },
            EffectGpuPointOp::Crop { left, top, right, bottom } => GpuEffectUniform {
                header: [5, 0, 0, 0],
                params: [*left, *top, *right, *bottom],
                color: [0.0; 4],
                extra0: [0.0; 4],
                extra1: [0.0; 4],
                extra2: [0.0; 4],
                extra3: [0.0; 4],
            },
            EffectGpuPointOp::Invert { color_mask, flags, blend_with_original } => {
                GpuEffectUniform {
                    header: [13, *color_mask, *flags, 0],
                    params: [*blend_with_original, 0.0, 0.0, 0.0],
                    color: [0.0; 4],
                    extra0: [0.0; 4],
                    extra1: [0.0; 4],
                    extra2: [0.0; 4],
                    extra3: [0.0; 4],
                }
            }
            EffectGpuPointOp::HueSaturation { grade } => {
                let controls = grade.gpu_controls();
                GpuEffectUniform {
                    header: [14, 0, 0, 0],
                    params: [controls[0], controls[1], controls[2], controls[3]],
                    color: [controls[4], controls[5], controls[6], controls[7]],
                    extra0: [0.0; 4],
                    extra1: [0.0; 4],
                    extra2: [0.0; 4],
                    extra3: [0.0; 4],
                }
            }
            EffectGpuPointOp::BlackAndWhite { grade } => {
                let controls = grade.gpu_controls();
                GpuEffectUniform {
                    header: [15, 0, 0, 0],
                    params: [controls[0], controls[1], controls[2], controls[3]],
                    color: [controls[4], controls[5], controls[6], 0.0],
                    extra0: [controls[7], controls[8], controls[9], 0.0],
                    extra1: [0.0; 4],
                    extra2: [0.0; 4],
                    extra3: [0.0; 4],
                }
            }
            EffectGpuPointOp::Threshold { level, luminance_coefficients } => GpuEffectUniform {
                header: [18, 0, 0, 0],
                params: [*level, 0.0, 0.0, 0.0],
                color: [
                    luminance_coefficients[0],
                    luminance_coefficients[1],
                    luminance_coefficients[2],
                    0.0,
                ],
                extra0: [0.0; 4],
                extra1: [0.0; 4],
                extra2: [0.0; 4],
                extra3: [0.0; 4],
            },
            EffectGpuPointOp::Posterize { levels } => GpuEffectUniform {
                header: [19, 0, 0, 0],
                params: [*levels, 0.0, 0.0, 0.0],
                color: [0.0; 4],
                extra0: [0.0; 4],
                extra1: [0.0; 4],
                extra2: [0.0; 4],
                extra3: [0.0; 4],
            },
            EffectGpuPointOp::Tint { grade } => {
                let (params, color, extra0) = grade.gpu_controls();
                GpuEffectUniform {
                    header: [20, 0, 0, 0],
                    params,
                    color,
                    extra0,
                    extra1: [0.0; 4],
                    extra2: [0.0; 4],
                    extra3: [0.0; 4],
                }
            }
            EffectGpuPointOp::Vibrance { grade } => {
                let (params, extra0) = grade.gpu_controls();
                GpuEffectUniform {
                    header: [21, 0, 0, 0],
                    params,
                    color: [0.0; 4],
                    extra0,
                    extra1: [0.0; 4],
                    extra2: [0.0; 4],
                    extra3: [0.0; 4],
                }
            }
            EffectGpuPointOp::Extract { grade } => {
                let (params, color, extra0) = grade.gpu_controls();
                GpuEffectUniform {
                    header: [24, 0, 0, 0],
                    params,
                    color,
                    extra0,
                    extra1: [0.0; 4],
                    extra2: [0.0; 4],
                    extra3: [0.0; 4],
                }
            }
            EffectGpuPointOp::BrightnessContrast { grade } => {
                let (params, extra0) = grade.gpu_controls();
                GpuEffectUniform {
                    header: [25, 0, 0, 0],
                    params,
                    color: [0.0; 4],
                    extra0,
                    extra1: [0.0; 4],
                    extra2: [0.0; 4],
                    extra3: [0.0; 4],
                }
            }
            EffectGpuPointOp::Levels { grade } => {
                let (params, extra0) = grade.gpu_controls();
                GpuEffectUniform {
                    header: [26, 0, 0, 0],
                    params,
                    color: [0.0; 4],
                    extra0,
                    extra1: [0.0; 4],
                    extra2: [0.0; 4],
                    extra3: [0.0; 4],
                }
            }
            EffectGpuPointOp::Ramp { grade } => {
                let (params, color, extra0, extra1) = grade.gpu_controls();
                GpuEffectUniform {
                    header: [27, 0, 0, 0],
                    params,
                    color,
                    extra0,
                    extra1,
                    extra2: [0.0; 4],
                    extra3: [0.0; 4],
                }
            }
            EffectGpuPointOp::Arithmetic { grade } => {
                let (params, extra0) = grade.gpu_controls();
                GpuEffectUniform {
                    header: [28, 0, 0, 0],
                    params,
                    color: [0.0; 4],
                    extra0,
                    extra1: [0.0; 4],
                    extra2: [0.0; 4],
                    extra3: [0.0; 4],
                }
            }
            EffectGpuPointOp::LeaveColor { grade } => {
                let (params, color) = grade.gpu_controls();
                GpuEffectUniform {
                    header: [29, 0, 0, 0],
                    params,
                    color,
                    extra0: [0.0; 4],
                    extra1: [0.0; 4],
                    extra2: [0.0; 4],
                    extra3: [0.0; 4],
                }
            }
            EffectGpuPointOp::BroadcastSafe { grade } => {
                let (params, extra0) = grade.gpu_controls();
                GpuEffectUniform {
                    header: [30, 0, 0, 0],
                    params,
                    color: [0.0; 4],
                    extra0,
                    extra1: [0.0; 4],
                    extra2: [0.0; 4],
                    extra3: [0.0; 4],
                }
            }
            EffectGpuPointOp::HdrToneMapping { grade } => {
                let (params, extra0) = grade.gpu_controls();
                GpuEffectUniform {
                    header: [40, 0, 0, 0],
                    params,
                    color: [0.0; 4],
                    extra0,
                    extra1: [0.0; 4],
                    extra2: [0.0; 4],
                    extra3: [0.0; 4],
                }
            }
            EffectGpuPointOp::RemoveColorMatting { grade } => {
                let (header, params) = grade.gpu_controls();
                GpuEffectUniform {
                    header: header.map(|value| value as u32),
                    params,
                    color: [0.0; 4],
                    extra0: [0.0; 4],
                    extra1: [0.0; 4],
                    extra2: [0.0; 4],
                    extra3: [0.0; 4],
                }
            }
            EffectGpuPointOp::PhotoFilter { grade } => {
                let (header, params, color) = grade.gpu_controls();
                GpuEffectUniform {
                    header,
                    params,
                    color,
                    extra0: [0.0; 4],
                    extra1: [0.0; 4],
                    extra2: [0.0; 4],
                    extra3: [0.0; 4],
                }
            }
            EffectGpuPointOp::JpegDamage { grade } => {
                let (header, params, color) = grade.gpu_controls();
                GpuEffectUniform {
                    header,
                    params,
                    color,
                    extra0: [0.0; 4],
                    extra1: [0.0; 4],
                    extra2: [0.0; 4],
                    extra3: [0.0; 4],
                }
            }
            EffectGpuPointOp::Colorama { grade } => {
                let controls = grade.gpu_controls();
                GpuEffectUniform {
                    header: controls.header,
                    params: controls.params,
                    color: controls.color,
                    extra0: controls.extra0,
                    extra1: controls.extra1,
                    extra2: controls.extra2,
                    extra3: controls.extra3,
                }
            }
            EffectGpuPointOp::TestSignals { grade } => {
                let (params, extra0) = grade.gpu_controls();
                GpuEffectUniform {
                    header: [31, 0, 0, 0],
                    params,
                    color: [0.0; 4],
                    extra0,
                    extra1: [0.0; 4],
                    extra2: [0.0; 4],
                    extra3: [0.0; 4],
                }
            }
            EffectGpuPointOp::ColorBalance { grade } => {
                let (params, color, extra0) = grade.gpu_controls();
                GpuEffectUniform {
                    header: [32, 0, 0, 0],
                    params,
                    color,
                    extra0,
                    extra1: [0.0; 4],
                    extra2: [0.0; 4],
                    extra3: [0.0; 4],
                }
            }
            EffectGpuPointOp::SpillSuppressor { grade } => {
                let (params, color, extra0) = grade.gpu_controls();
                GpuEffectUniform {
                    header: [33, 0, 0, 0],
                    params,
                    color,
                    extra0,
                    extra1: [0.0; 4],
                    extra2: [0.0; 4],
                    extra3: [0.0; 4],
                }
            }
            EffectGpuPointOp::ChangeToColor { grade } => {
                let (params, color) = grade.gpu_controls();
                GpuEffectUniform {
                    header: [34, grade.change_flags(), grade.change_by_index(), 0],
                    params,
                    color,
                    extra0: grade.gpu_source(),
                    extra1: [grade.gpu_view(), 0.0, 0.0, 0.0],
                    extra2: [0.0; 4],
                    extra3: [0.0; 4],
                }
            }
            EffectGpuPointOp::FourColorGradient { grade } => {
                let (params, color, extra0, extra1, extra2, extra3) = grade.gpu_controls();
                GpuEffectUniform {
                    header: [35, 0, 0, 0],
                    params,
                    color,
                    extra0,
                    extra1,
                    extra2,
                    extra3,
                }
            }
            EffectGpuPointOp::Checkerboard { grade } => {
                let (params, color, extra0) = grade.gpu_controls();
                GpuEffectUniform {
                    header: [36, grade.size_from().index(), grade.blend_mode_index(), 0],
                    params,
                    color,
                    extra0,
                    extra1: [0.0; 4],
                    extra2: [0.0; 4],
                    extra3: [0.0; 4],
                }
            }
            EffectGpuPointOp::Grid { grade } => {
                let (params, color, extra0, extra1) = grade.gpu_controls();
                GpuEffectUniform {
                    header: [
                        37,
                        grade.size_from().index(),
                        grade.blend_mode_index(),
                        u32::from(grade.invert()),
                    ],
                    params,
                    color,
                    extra0,
                    extra1,
                    extra2: [0.0; 4],
                    extra3: [0.0; 4],
                }
            }
            EffectGpuPointOp::CircleEllipse { grade } => {
                let (params, color, extra0, extra1) = grade.gpu_controls();
                GpuEffectUniform {
                    header: [38, grade.blend_mode_index(), 0, u32::from(grade.invert())],
                    params,
                    color,
                    extra0,
                    extra1,
                    extra2: [0.0; 4],
                    extra3: [0.0; 4],
                }
            }
            EffectGpuPointOp::Fill { grade } => {
                let (params, color) = grade.gpu_controls();
                GpuEffectUniform {
                    // `header.y` is free: the fill has no blend-mode parameter
                    // and always composites Normal.
                    header: [39, 0, 0, u32::from(grade.invert())],
                    params,
                    color,
                    extra0: [0.0; 4],
                    extra1: [0.0; 4],
                    extra2: [0.0; 4],
                    extra3: [0.0; 4],
                }
            }
            EffectGpuPointOp::CellPattern { grade } => {
                let (params, color, extra0, _extra1) = grade.gpu_controls();
                GpuEffectUniform {
                    // The cell type, the overflow mode and the invert/tiling/
                    // cycle flags share the header; the twelve unused scalars
                    // of the shared point uniform stay zero.
                    header: [
                        41,
                        grade.pattern().index(),
                        grade.overflow().index(),
                        grade.header_flags(),
                    ],
                    params,
                    color,
                    extra0,
                    extra1: [0.0; 4],
                    extra2: [0.0; 4],
                    extra3: [0.0; 4],
                }
            }
            EffectGpuPointOp::Fractal { grade } => {
                let (params, color, extra0, extra1, extra2) = grade.gpu_controls();
                GpuEffectUniform {
                    // The header carries the set choice, the equation and the
                    // resolved iteration budget; all three are loop/selection
                    // state and must stay integral. extra2.x is the square
                    // oversample side and extra2.y the precomputed view scale.
                    header: [
                        42,
                        grade.set_choice().index(),
                        grade.equation().index(),
                        grade.effective_iterations(),
                    ],
                    params,
                    color,
                    extra0,
                    extra1,
                    extra2: [extra2[0], extra2[1], 0.0, 0.0],
                    extra3: [0.0; 4],
                }
            }
            EffectGpuPointOp::Noise { grade } => GpuEffectUniform {
                header: [23, 0, 0, 0],
                params: grade.gpu_controls(),
                color: [0.0; 4],
                extra0: [0.0; 4],
                extra1: [0.0; 4],
                extra2: [0.0; 4],
                extra3: [0.0; 4],
            },
            EffectGpuPointOp::Tritone { grade } => {
                let (params, color, extra0, extra1) = grade.gpu_controls();
                GpuEffectUniform {
                    header: [22, 0, 0, 0],
                    params,
                    color,
                    extra0,
                    extra1,
                    extra2: [0.0; 4],
                    extra3: [0.0; 4],
                }
            }
            EffectGpuPointOp::ChannelMixer { grade } => {
                let (params, color, extra0, extra1) = grade.gpu_controls();
                GpuEffectUniform {
                    header: [17, 0, 0, 0],
                    params,
                    color,
                    extra0,
                    extra1,
                    extra2: [0.0; 4],
                    extra3: [0.0; 4],
                }
            }
            EffectGpuPointOp::ShiftChannels { grade } => GpuEffectUniform {
                header: [16, grade.packed_sources(), 0, 0],
                params: {
                    let coefficients = grade.luminance_coefficients();
                    [coefficients[0], coefficients[1], coefficients[2], 0.0]
                },
                color: [0.0; 4],
                extra0: [0.0; 4],
                extra1: [0.0; 4],
                extra2: [0.0; 4],
                extra3: [0.0; 4],
            },
            EffectGpuPointOp::ChannelCombiner { grade } => {
                let (header, params) = grade.gpu_controls();
                GpuEffectUniform {
                    header,
                    params,
                    color: [0.0; 4],
                    extra0: [0.0; 4],
                    extra1: [0.0; 4],
                    extra2: [0.0; 4],
                    extra3: [0.0; 4],
                }
            }
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
                    extra2: [0.0; 4],
                    extra3: [0.0; 4],
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
    // All request weights are admitted before inactive/identity elimination,
    // source capability queries or any resource/pipeline allocation.
    for layer in request.layers {
        if !layer.opacity.is_finite() || !(0.0..=1.0).contains(&layer.opacity) {
            return Err(GpuCompositeError::InvalidOpacity { bits: layer.opacity.to_bits() });
        }
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
        if layer.blend_policy == mondrian_effects::EffectPassBlendPolicy::Mix
            && !matches!(layer.source, GpuCompositeLayerSource::Adjustment)
        {
            return Err(GpuCompositeError::MixRequiresAdjustment);
        }
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
            && layer.effect_plan.is_none()
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

/// Reject spatial effect plans the compositor cannot honour, before any GPU
/// work is recorded.
///
/// The spatial plan is not a field of [`GpuCompositeRequest`], so this is the
/// admission check for [`GpuFrameCompositor::record_with_spatial_effect_chains`]
/// in exactly the role `validate_request` plays for [`GpuFrameCompositor::record`].
fn validate_spatial_request(
    request: &GpuCompositeSpatialRequest<'_>,
) -> Result<(), GpuCompositeError> {
    for entry in request.layers {
        let Some(plan) = entry.spatial_plan else {
            continue;
        };
        // A spatial chain already flushed the point run; a fused point plan
        // alongside it would apply the same operations twice.
        if entry.layer.effect_plan.is_some() {
            return Err(GpuCompositeError::SpatialPlanWithPointPlan);
        }
        match entry.layer.source {
            GpuCompositeLayerSource::CpuFrame(_) | GpuCompositeLayerSource::GpuFrame(_) => {}
            GpuCompositeLayerSource::CpuDataTexture(_)
            | GpuCompositeLayerSource::SolidColor(_)
            | GpuCompositeLayerSource::Adjustment => {
                return Err(GpuCompositeError::SpatialPlanOnUnsupportedSource {
                    source_kind: composite_layer_source_name(entry.layer.source),
                });
            }
        }
        let domain = plan.processing_domain();
        if domain != EffectColorDomain::SceneLinearRgb {
            return Err(GpuCompositeError::EffectDomainRequiresExternalPass { domain });
        }
    }
    Ok(())
}

fn prepare_spatial_chains(
    request: &GpuCompositeSpatialRequest<'_>,
    auxiliary_rasters: &[&[GpuSpatialAuxiliaryRaster<'_>]],
    table: &GpuColorFrameResourceTable<GpuColorFrameWgpuResource>,
) -> Result<Vec<Option<GpuSpatialChainPlan>>, GpuCompositeError> {
    if !auxiliary_rasters.is_empty() && auxiliary_rasters.len() != request.layers.len() {
        return Err(GpuCompositeError::SpatialChainFailed {
            pass: "spatial_auxiliary_bindings",
            detail: "auxiliary binding groups must match the composite layers".into(),
        });
    }
    let mut chains = Vec::with_capacity(request.layers.len());
    for (index, entry) in request.layers.iter().enumerate() {
        let bindings = auxiliary_rasters.get(index).copied().unwrap_or_default();
        let Some(pipeline) = entry.spatial_plan else {
            if !bindings.is_empty() {
                return Err(GpuCompositeError::SpatialChainFailed {
                    pass: "spatial_auxiliary_bindings",
                    detail: format!("layer {index} has auxiliary bindings without a spatial chain"),
                });
            }
            chains.push(None);
            continue;
        };
        let primary = layer_source_descriptor(entry.layer.source).ok_or(
            GpuCompositeError::SpatialPlanOnUnsupportedSource {
                source_kind: composite_layer_source_name(entry.layer.source),
            },
        )?;
        let slots: Vec<_> = bindings.iter().map(|binding| binding.slot).collect();
        let chain =
            gpu_spatial_chain_plan_with_auxiliary(pipeline, primary.width, primary.height, &slots)
                .map_err(|blocker| GpuCompositeError::SpatialChainFailed {
                    pass: spatial_chain_blocker_pass(&blocker),
                    detail: blocker.to_string(),
                })?;
        for (binding_index, binding) in bindings.iter().enumerate() {
            let declared = pipeline.stages().iter().any(|stage| {
                matches!(
                    stage,
                    EffectGpuStage::Spatial { pass, .. }
                        if pass.auxiliary_slot() == Some(Some(binding.slot))
                )
            });
            if !declared || slots[..binding_index].contains(&binding.slot) {
                return Err(GpuCompositeError::SpatialChainFailed {
                    pass: "spatial_auxiliary_bindings",
                    detail: format!(
                        "layer {index} has an undeclared or duplicate auxiliary slot {}",
                        binding.slot
                    ),
                });
            }
            let descriptor = binding.frame.descriptor();
            let texture = &table
                .get(binding.frame)
                .map_err(GpuCompositeError::ResourceTable)?
                .resource()
                .texture;
            if descriptor.color_space
                != crate::ColorFrameSpace::Working(request.working_color_space)
                || descriptor.domain != ColorFrameDomain::Working
                || descriptor.encoding != ColorFrameEncoding::LinearFloat
                || descriptor.residency != ColorFrameResidency::Gpu
                || texture.format() != wgpu::TextureFormat::Rgba32Float
                || texture.width() != descriptor.width
                || texture.height() != descriptor.height
                || !texture.usage().contains(wgpu::TextureUsages::TEXTURE_BINDING)
            {
                return Err(GpuCompositeError::SpatialChainFailed {
                    pass: "spatial_auxiliary_format",
                    detail: format!(
                        "layer {index} auxiliary slot {} has no working Float32 texture",
                        binding.slot
                    ),
                });
            }
            require_straight_compatible_alpha(descriptor.alpha)?;
            if binding.transform.iter().any(|value| !value.is_finite())
                || (binding.policy.resolution() == AuxiliaryResolution::MatchPrimary
                    && (descriptor.width, descriptor.height) != (primary.width, primary.height))
            {
                return Err(GpuCompositeError::SpatialChainFailed {
                    pass: "spatial_auxiliary_geometry",
                    detail: format!(
                        "layer {index} auxiliary slot {} contradicts its registration",
                        binding.slot
                    ),
                });
            }
        }
        chains.push(Some(chain));
    }
    Ok(chains)
}

fn require_straight_compatible_alpha(
    alpha: crate::ColorFrameAlpha,
) -> Result<(), GpuCompositeError> {
    if alpha.is_coverage() && alpha.is_straight_compatible() {
        Ok(())
    } else {
        Err(GpuCompositeError::InputNotStraightCompatibleAlpha { actual: alpha })
    }
}

fn layer_has_zero_contribution(layer: &GpuCompositeLayer<'_>) -> bool {
    if layer.opacity.clamp(0.0, 1.0) == 0.0 {
        return true;
    }
    if matches!(layer.source, GpuCompositeLayerSource::Adjustment)
        && layer.blend_policy == mondrian_effects::EffectPassBlendPolicy::Mix
        && layer.effect_plan.is_some_and(CompiledEffectGpuPlan::is_identity)
    {
        return true;
    }

    if matches!(layer.source, GpuCompositeLayerSource::SolidColor(color) if color.a.clamp(0.0, 1.0) == 0.0)
    {
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

fn composite_output_alpha(
    request: &GpuCompositeRequest<'_>,
    has_spatial_plan_at: impl Fn(usize) -> bool,
) -> crate::ColorFrameAlpha {
    let mut opaque = false;
    for (index, layer) in request.layers.iter().enumerate() {
        if layer_has_zero_contribution(layer) {
            continue;
        }
        if matches!(layer.source, GpuCompositeLayerSource::Adjustment)
            && layer.blend_policy == mondrian_effects::EffectPassBlendPolicy::Mix
            && !effect_plan_preserves_full_coverage(layer.effect_plan)
        {
            // A processed Crop can lower coverage even over an opaque original.
            opaque = false;
        } else if layer_establishes_opaque_canvas(
            request.width,
            request.height,
            layer,
            has_spatial_plan_at(index),
        ) {
            opaque = true;
        }
    }
    if opaque {
        crate::ColorFrameAlpha::Opaque
    } else {
        crate::ColorFrameAlpha::StraightCoverage
    }
}

fn effect_plan_preserves_full_coverage(plan: Option<&CompiledEffectGpuPlan>) -> bool {
    plan.is_none_or(|plan| {
        plan.operations()
            .iter()
            .all(|operation| !matches!(operation, EffectGpuPointOp::Crop { .. }))
    })
}

fn layer_establishes_opaque_canvas(
    output_width: u32,
    output_height: u32,
    layer: &GpuCompositeLayer<'_>,
    has_spatial_plan: bool,
) -> bool {
    // A spatial chain runs on the layer raster before the draw and can change
    // coverage (colour key, split, and every remapping kernel), so no spatial
    // layer establishes an opaque canvas.
    if has_spatial_plan
        || layer.opacity.clamp(0.0, 1.0) != 1.0
        || layer.blend_policy != mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal)
        || !is_identity_transform(layer.transform)
        || !effect_plan_preserves_full_coverage(layer.effect_plan)
    {
        return false;
    }
    match layer.source {
        GpuCompositeLayerSource::SolidColor(color) => color.a.clamp(0.0, 1.0) == 1.0,
        GpuCompositeLayerSource::CpuFrame(frame) => {
            let descriptor = frame.descriptor();
            descriptor.width == output_width
                && descriptor.height == output_height
                && descriptor.alpha == crate::ColorFrameAlpha::Opaque
        }
        GpuCompositeLayerSource::GpuFrame(handle) => {
            let descriptor = handle.descriptor();
            descriptor.width == output_width
                && descriptor.height == output_height
                && descriptor.alpha == crate::ColorFrameAlpha::Opaque
        }
        GpuCompositeLayerSource::CpuDataTexture(_) | GpuCompositeLayerSource::Adjustment => false,
    }
}

fn can_use_opaque_normal_accumulator(
    request: &GpuCompositeRequest<'_>,
    execution: &crate::gpu_composite_execution::GpuCompositeExecutionPlan,
    has_spatial_plan: bool,
) -> bool {
    if has_spatial_plan {
        return false;
    }
    let Some(first) = execution.layers.first() else {
        return false;
    };
    let canvas = GpuCompositeRect {
        x: 0,
        y: 0,
        width: request.width,
        height: request.height,
    };
    first.damage == canvas
        && layer_establishes_opaque_canvas(
            request.width,
            request.height,
            &request.layers[first.input_index],
            has_spatial_plan,
        )
        && execution.layers.iter().all(|scheduled| {
            let layer = &request.layers[scheduled.input_index];
            layer.blend_policy == mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal)
                && !matches!(layer.source, GpuCompositeLayerSource::Adjustment)
        })
}

fn composite_layer_footprint(
    output_width: u32,
    output_height: u32,
    layer: &GpuCompositeLayer<'_>,
) -> GpuCompositeLayerFootprint {
    if layer_has_zero_contribution(layer) {
        return GpuCompositeLayerFootprint::NoContribution;
    }
    let fused_point_operations = layer.effect_plan.map_or(0, |plan| plan.operations().len() as u32);
    match layer.source {
        GpuCompositeLayerSource::Adjustment => {
            GpuCompositeLayerFootprint::FullCanvas { fused_point_operations }
        }
        GpuCompositeLayerSource::SolidColor(_) => GpuCompositeLayerFootprint::TransformedSource {
            source_width: output_width,
            source_height: output_height,
            transform: layer.transform,
            crop: composite_source_crop(layer.effect_plan),
            fused_point_operations,
        },
        GpuCompositeLayerSource::CpuFrame(frame)
        | GpuCompositeLayerSource::CpuDataTexture(frame) => {
            let descriptor = frame.descriptor();
            GpuCompositeLayerFootprint::TransformedSource {
                source_width: descriptor.width,
                source_height: descriptor.height,
                transform: layer.transform,
                crop: composite_source_crop(layer.effect_plan),
                fused_point_operations,
            }
        }
        GpuCompositeLayerSource::GpuFrame(handle) => {
            let descriptor = handle.descriptor();
            GpuCompositeLayerFootprint::TransformedSource {
                source_width: descriptor.width,
                source_height: descriptor.height,
                transform: layer.transform,
                crop: composite_source_crop(layer.effect_plan),
                fused_point_operations,
            }
        }
    }
}

fn composite_source_crop(effect_plan: Option<&CompiledEffectGpuPlan>) -> GpuCompositeSourceCrop {
    effect_plan.map_or_else(GpuCompositeSourceCrop::default, |plan| {
        plan.operations()
            .iter()
            .fold(
                GpuCompositeSourceCrop::default(),
                |crop, operation| match operation {
                    EffectGpuPointOp::Crop { left, top, right, bottom } => {
                        crop.intersect(GpuCompositeSourceCrop {
                            left: *left,
                            top: *top,
                            right: *right,
                            bottom: *bottom,
                        })
                    }
                    _ => crop,
                },
            )
    })
}

fn record_preserved_regions(
    encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
    source: &wgpu::Texture,
    destination: &wgpu::Texture,
    regions: &[GpuCompositeRect],
) {
    for region in regions {
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: source,
                mip_level: 0,
                origin: wgpu::Origin3d { x: region.x, y: region.y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: destination,
                mip_level: 0,
                origin: wgpu::Origin3d { x: region.x, y: region.y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: region.width,
                height: region.height,
                depth_or_array_layers: 1,
            },
        );
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

fn spatial_chain_blocker_pass(
    blocker: &crate::spatial_driver::GpuSpatialChainBlocker,
) -> &'static str {
    match blocker {
        crate::spatial_driver::GpuSpatialChainBlocker::UnsupportedPass { pass, .. } => pass,
        other => other.code(),
    }
}

fn spatial_failure_pass(error: &crate::spatial_pass_gpu::EffectSpatialGpuError) -> &'static str {
    match error {
        crate::spatial_pass_gpu::EffectSpatialGpuError::UnsupportedPass(pass) => pass,
        crate::spatial_pass_gpu::EffectSpatialGpuError::EmptyFrame => "empty_frame",
        crate::spatial_pass_gpu::EffectSpatialGpuError::MissingAuxiliaryInput => {
            "spatial_auxiliary_missing"
        }
    }
}

fn composite_layer_source_name(source: GpuCompositeLayerSource<'_>) -> &'static str {
    match source {
        GpuCompositeLayerSource::CpuFrame(_) => "cpu_frame",
        GpuCompositeLayerSource::CpuDataTexture(_) => "cpu_data_texture",
        GpuCompositeLayerSource::GpuFrame(_) => "gpu_frame",
        GpuCompositeLayerSource::SolidColor(_) => "solid_color",
        GpuCompositeLayerSource::Adjustment => "adjustment",
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
        product_gpu_working_texture_format(),
        label,
    )?;
    let allocation = GpuColorFrameAllocationPlan::for_handle(handle);
    Ok(resource_pool.map_or_else(
        || GpuColorFrameUploader::allocate(device, &allocation),
        |pool| pool.acquire(device, &allocation),
    ))
}

fn clear_working_texture(
    encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
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

    #[tokio::test]
    #[ignore = "requires an actual wgpu adapter; root runs OLD and GREEN explicitly"]
    async fn canonical_adjustment_default_opacity_preserves_coverage_on_actual_gpu() {
        use mondrian_core::automation::{PropertyHost, PropertyMutation, PropertyValue};
        use mondrian_core::{AssetId, FramePosition, Resolution, TimelineTime};
        use mondrian_effects::{
            lower_effect_graph_to_gpu_plan, EffectNode, EffectNodeExt, EffectType,
        };
        use mondrian_timeline::clip::Transform2D;
        use mondrian_timeline::{Clip, Sequence};

        let context = crate::GpuContext::new().await.expect("actual GPU adapter required");
        let adapter_info = context.adapter.get_info();
        eprintln!("GPU_ADJUSTMENT_NATIVE_PRIMARY: {adapter_info:?}");
        assert!(
            matches!(
                adapter_info.device_type,
                wgpu::DeviceType::DiscreteGpu | wgpu::DeviceType::IntegratedGpu
            ),
            "Adjustment GPU qualification requires actual hardware: {adapter_info:?}",
        );
        let mut sequence = Sequence::new("canonical Adjustment default GPU strength");
        sequence.settings.resolution = Resolution { width: 4, height: 4 };
        sequence.settings.color.working_color_space = WorkingColorSpace::LinearRec709;
        let duration =
            TimelineTime::from_frame_position(FramePosition::new(10, sequence.time_base()))
                .expect("duration");
        let solid = Clip::new_solid_color(
            AssetId::new(),
            Color { r: 2.0, g: -4.0, b: 8.0, a: 0.5 },
            TimelineTime::ZERO,
            duration,
        )
        .expect("solid");
        sequence.video_tracks[0].add_clip(solid).expect("solid track");
        let mut adjustment =
            Clip::new_adjustment_layer(AssetId::new(), TimelineTime::ZERO, duration)
                .expect("Adjustment");
        adjustment
            .transform
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: Transform2D::OPACITY_PATH.to_owned(),
                value: PropertyValue::Float(0.5),
            })
            .expect("Adjustment effect strength");
        let mut correction = EffectNode::with_defaults(EffectType::BasicCorrection);
        correction
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::BasicCorrection.property_path("exposure"),
                value: PropertyValue::Float(1.0),
            })
            .expect("one-stop exposure");
        adjustment.add_effect_node(correction);
        sequence.video_tracks[1].add_clip(adjustment).expect("Adjustment track");
        let program =
            crate::PreparedVisualProgram::prepare(&sequence).expect("sole prepared program");
        let plan = crate::evaluate_prepared_visual_program(
            &program,
            crate::TimelineEvaluationRequest::reference_output(FramePosition::new(
                0,
                sequence.time_base(),
            )),
        )
        .expect("sole evaluated plan");
        assert_eq!(plan.elements.len(), 2);
        let crate::TimelineRenderPlanElement::SolidColor(solid) = &plan.elements[0] else {
            panic!("canonical solid placement");
        };
        let crate::TimelineRenderPlanElement::Adjustment(adjustment) = &plan.elements[1] else {
            panic!("canonical Adjustment placement");
        };
        let effect_plan =
            lower_effect_graph_to_gpu_plan(&adjustment.effect_graph).expect("GPU correction");
        let layers = [
            GpuCompositeLayer {
                source: GpuCompositeLayerSource::SolidColor(solid.color),
                opacity: solid.opacity,
                blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(solid.blend_mode),
                transform: solid.transform,
                effect_plan: None,
                frame_seed: solid.frame_seed,
            },
            GpuCompositeLayer {
                source: GpuCompositeLayerSource::Adjustment,
                opacity: adjustment.opacity,
                blend_policy: adjustment.blend_policy,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_plan: Some(&effect_plan),
                frame_seed: adjustment.frame_seed,
            },
        ];
        let actual = readback_test_composite(&context, &layers);
        // Independently: exposure doubles RGB; half effect strength interpolates
        // original/effected associated light without adding another coverage layer.
        for pixel in actual {
            for (component, expected) in pixel.into_iter().zip([3.0, -6.0, 12.0, 0.5]) {
                assert!((component - expected).abs() <= 2.0e-5, "actual {pixel:?}");
            }
        }
    }

    #[test]
    fn gpu_mix_requires_the_original_adjustment_accumulator() {
        let layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::SolidColor(Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 }),
            opacity: 0.5,
            blend_policy: mondrian_effects::EffectPassBlendPolicy::Mix,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: None,
            frame_seed: 0,
        };
        assert_eq!(
            validate_request(&GpuCompositeRequest {
                width: 4,
                height: 4,
                working_color_space: WorkingColorSpace::LinearRec709,
                layers: &[layer],
            }),
            Err(GpuCompositeError::MixRequiresAdjustment),
        );
    }

    #[test]
    fn gpu_adjustment_mix_revokes_opaque_coverage_after_crop_in_order() {
        use mondrian_effects::{
            compile_reference_render_graph, lower_effect_graph_to_gpu_plan,
            EffectGraphBuilderState, EffectPassBlendPolicy, EffectRenderOp,
        };
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(EffectRenderOp::Crop { left: 0.5, top: 0.0, right: 0.0, bottom: 0.0 });
        let graph = compile_reference_render_graph(builder.finish()).expect("crop graph");
        let plan = lower_effect_graph_to_gpu_plan(&graph).expect("crop GPU plan");
        let base = GpuCompositeLayer {
            source: GpuCompositeLayerSource::SolidColor(Color { r: 2.0, g: -3.0, b: 4.0, a: 1.0 }),
            opacity: 1.0,
            blend_policy: EffectPassBlendPolicy::Blend(BlendMode::Normal),
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: None,
            frame_seed: 0,
        };
        let mut adjustment = GpuCompositeLayer {
            source: GpuCompositeLayerSource::Adjustment,
            opacity: 0.5,
            blend_policy: EffectPassBlendPolicy::Mix,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: Some(&plan),
            frame_seed: 0,
        };
        let output_alpha = |layers: &[GpuCompositeLayer<'_>]| {
            composite_output_alpha(
                &GpuCompositeRequest {
                    width: 4,
                    height: 4,
                    working_color_space: WorkingColorSpace::LinearRec709,
                    layers,
                },
                |_| false,
            )
        };
        assert_eq!(
            output_alpha(&[base, adjustment]),
            crate::ColorFrameAlpha::StraightCoverage
        );
        assert_eq!(
            output_alpha(&[base, adjustment, base]),
            crate::ColorFrameAlpha::Opaque
        );
        adjustment.blend_policy = EffectPassBlendPolicy::Blend(BlendMode::Normal);
        assert_eq!(
            output_alpha(&[base, adjustment]),
            crate::ColorFrameAlpha::Opaque
        );
        adjustment.blend_policy = EffectPassBlendPolicy::Mix;
        adjustment.opacity = 0.0;
        assert_eq!(
            output_alpha(&[base, adjustment]),
            crate::ColorFrameAlpha::Opaque
        );
    }

    #[test]
    fn gpu_adjustment_direct_mix_alpha_metadata_uses_both_actual_inputs() {
        use crate::ColorFrameAlpha::{Opaque, StraightCoverage};
        use mondrian_effects::EffectPassBlendPolicy::{Blend, Mix};
        assert_eq!(
            adjustment_output_alpha(Opaque, StraightCoverage, 0.5, Mix),
            StraightCoverage
        );
        assert_eq!(
            adjustment_output_alpha(StraightCoverage, Opaque, 0.5, Mix),
            StraightCoverage
        );
        assert_eq!(adjustment_output_alpha(Opaque, Opaque, 0.5, Mix), Opaque);
        assert_eq!(
            adjustment_output_alpha(Opaque, StraightCoverage, 0.0, Mix),
            Opaque
        );
        assert_eq!(
            adjustment_output_alpha(StraightCoverage, Opaque, 1.0, Mix),
            Opaque
        );
        assert_eq!(
            adjustment_output_alpha(Opaque, StraightCoverage, 0.5, Blend(BlendMode::Normal)),
            Opaque
        );
        for mode in [
            BlendMode::Normal,
            BlendMode::Multiply,
            BlendMode::Luminosity,
        ] {
            assert_ne!(gpu_pass_policy_id(Mix), gpu_pass_policy_id(Blend(mode)));
        }
    }

    #[tokio::test]
    #[ignore = "requires an actual wgpu adapter; root qualification runs this explicitly"]
    async fn gpu_adjustment_mix_matches_independent_associated_pixel_oracles() {
        use mondrian_effects::EffectPassBlendPolicy::{Blend, Mix};
        let context = crate::GpuContext::new().await.expect("actual GPU adapter required");
        let adapter_info = context.adapter.get_info();
        eprintln!("GPU_ADJUSTMENT_NATIVE_PRIMARY: {adapter_info:?}");
        assert!(
            matches!(
                adapter_info.device_type,
                wgpu::DeviceType::DiscreteGpu | wgpu::DeviceType::IntegratedGpu
            ),
            "Adjustment GPU qualification requires actual hardware: {adapter_info:?}",
        );
        // Independently calculated rational cases. The identical-colour case
        // distinguishes effect strength from duplicating alpha with Normal Over.
        let cases = [
            (
                [2.0, -4.0, 8.0, 0.5],
                [2.0, -4.0, 8.0, 0.5],
                0.5,
                Mix,
                [2.0, -4.0, 8.0, 0.5],
            ),
            (
                [2.0, -4.0, 8.0, 0.5],
                [6.0, 8.0, -2.0, 0.25],
                0.5,
                Mix,
                [10.0 / 3.0, 0.0, 14.0 / 3.0, 0.375],
            ),
            (
                [2.0, -4.0, 8.0, 0.5],
                [2.0, -4.0, 8.0, 0.5],
                0.5,
                Blend(BlendMode::Normal),
                [2.0, -4.0, 8.0, 0.625],
            ),
            (
                [7.0, -3.0, 11.0, 0.0],
                [2.0, 4.0, 8.0, 0.5],
                0.0,
                Mix,
                [7.0, -3.0, 11.0, 0.0],
            ),
            (
                [2.0, 4.0, 8.0, 0.5],
                [7.0, -3.0, 11.0, 0.0],
                1.0,
                Mix,
                [7.0, -3.0, 11.0, 0.0],
            ),
            (
                [7.0, -3.0, 11.0, 0.0],
                [7.0, -3.0, 11.0, 0.0],
                0.5,
                Mix,
                [7.0, -3.0, 11.0, 0.0],
            ),
            (
                [7.0, -3.0, 11.0, 0.0],
                [-5.0, 9.0, 2.0, 0.0],
                0.5,
                Mix,
                [0.0; 4],
            ),
        ];
        for (base, processed, opacity, policy, expected) in cases {
            let compositor = GpuFrameCompositor::new(&context.device).expect("GPU compositor");
            let mut ids = GpuColorFrameIdAllocator::new(1).expect("frame IDs");
            let mut table = GpuColorFrameResourceTable::new();
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("adjustment-associated-mix-independent-oracle"),
                });
            let mut materialize = |sample: [f32; 4]| {
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
                        Color {
                            r: sample[0],
                            g: sample[1],
                            b: sample[2],
                            a: sample[3],
                        },
                    )
                    .expect("source materialization preserves actual straight samples")
                    .output
            };
            let original = materialize(base);
            let effected = materialize(processed);
            let result = compositor
                .record_adjustment_blend_pass(
                    &context.device,
                    &context.queue,
                    &mut encoder,
                    &mut ids,
                    &mut table,
                    None,
                    &original,
                    &effected,
                    opacity,
                    policy,
                    0,
                    WorkingColorSpace::LinearRec709,
                )
                .expect("typed adjustment pass");
            assert_eq!(
                result.output.descriptor().alpha,
                crate::ColorFrameAlpha::StraightCoverage
            );
            let readback = context.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("adjustment-mix-readback"),
                size: 256 * 4,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let resource = table.get(&result.output).expect("output resource");
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: &resource.resource().texture,
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
                    for (actual, wanted) in pixel.iter().zip(expected) {
                        assert!(actual.is_finite());
                        assert!(
                            (*actual - wanted).abs() <= 2.0e-5,
                            "{policy:?}, t={opacity}: {pixel:?} vs {expected:?}"
                        );
                    }
                }
            }
            readback.unmap();
        }
    }

    #[tokio::test]
    #[ignore = "requires an actual wgpu adapter; root qualification runs this explicitly"]
    async fn gpu_adjustment_crop_mix_lowers_coverage_and_identity_explicit_blend_remains_over() {
        use mondrian_effects::{
            compile_reference_render_graph, lower_effect_graph_to_gpu_plan,
            EffectGraphBuilderState, EffectPassBlendPolicy, EffectRenderOp,
        };
        let context = crate::GpuContext::new().await.expect("actual GPU adapter required");
        let adapter_info = context.adapter.get_info();
        eprintln!("GPU_ADJUSTMENT_NATIVE_PRIMARY: {adapter_info:?}");
        assert!(
            matches!(
                adapter_info.device_type,
                wgpu::DeviceType::DiscreteGpu | wgpu::DeviceType::IntegratedGpu
            ),
            "Adjustment GPU qualification requires actual hardware: {adapter_info:?}",
        );
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(EffectRenderOp::Crop { left: 0.5, top: 0.0, right: 0.0, bottom: 0.0 });
        let crop_graph = compile_reference_render_graph(builder.finish()).expect("crop graph");
        let crop_plan = lower_effect_graph_to_gpu_plan(&crop_graph).expect("crop GPU plan");
        let base = GpuCompositeLayer {
            source: GpuCompositeLayerSource::SolidColor(Color { r: 2.0, g: -3.0, b: 4.0, a: 1.0 }),
            opacity: 1.0,
            blend_policy: EffectPassBlendPolicy::Blend(BlendMode::Normal),
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: None,
            frame_seed: 0,
        };
        let crop = GpuCompositeLayer {
            source: GpuCompositeLayerSource::Adjustment,
            opacity: 0.5,
            blend_policy: EffectPassBlendPolicy::Mix,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: Some(&crop_plan),
            frame_seed: 0,
        };
        let mixed = readback_test_composite(&context, &[base, crop]);
        for (index, pixel) in mixed.iter().enumerate() {
            let expected = [2.0, -3.0, 4.0, if index % 4 < 2 { 0.5 } else { 1.0 }];
            for (actual, wanted) in pixel.iter().zip(expected) {
                assert!(
                    (*actual - wanted).abs() <= 2.0e-5,
                    "crop pixel {index}: {pixel:?}"
                );
            }
        }
        let identity_graph =
            compile_reference_render_graph(EffectGraphBuilderState::new().finish())
                .expect("identity graph");
        let identity_plan =
            lower_effect_graph_to_gpu_plan(&identity_graph).expect("identity GPU plan");
        let half_covered = GpuCompositeLayer {
            source: GpuCompositeLayerSource::SolidColor(Color { r: 2.0, g: -3.0, b: 4.0, a: 0.5 }),
            ..base
        };
        for (policy, expected_alpha) in [
            (EffectPassBlendPolicy::Mix, 0.5),
            (EffectPassBlendPolicy::Blend(BlendMode::Normal), 0.625),
        ] {
            let identity_adjustment = GpuCompositeLayer {
                effect_plan: Some(&identity_plan),
                blend_policy: policy,
                ..crop
            };
            let actual = readback_test_composite(&context, &[half_covered, identity_adjustment]);
            for pixel in actual {
                let expected = [2.0, -3.0, 4.0, expected_alpha];
                for (lane, wanted) in pixel.iter().zip(expected) {
                    assert!(
                        (*lane - wanted).abs() <= 2.0e-5,
                        "identity {policy:?}: {pixel:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn gpu_adjustment_mix_refuses_unrestricted_alpha_before_shader_execution() {
        use mondrian_core::ColorFrameAlpha::{PremultipliedFloat, StraightFloat};
        use mondrian_effects::{
            compile_reference_render_graph, lower_effect_graph_to_gpu_plan,
            EffectGraphBuilderState, EffectPassBlendPolicy,
        };
        let graph = compile_reference_render_graph(EffectGraphBuilderState::new().finish())
            .expect("identity graph");
        let plan = lower_effect_graph_to_gpu_plan(&graph).expect("identity GPU plan");
        for (alpha, sample) in [
            (StraightFloat, [2.0, -4.0, 8.0, 1.5]),
            (StraightFloat, [2.0, -4.0, 8.0, -0.5]),
            (PremultipliedFloat, [7.0, -3.0, 11.0, 0.0]),
        ] {
            let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
                sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
                width: 4,
                height: 4,
                alpha,
                color_space: WorkingColorSpace::LinearRec709,
                data: vec![sample; 16],
            });
            let layers = [
                GpuCompositeLayer {
                    source: GpuCompositeLayerSource::CpuFrame(&frame),
                    opacity: 1.0,
                    blend_policy: EffectPassBlendPolicy::Blend(BlendMode::Normal),
                    transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                    effect_plan: None,
                    frame_seed: 0,
                },
                GpuCompositeLayer {
                    source: GpuCompositeLayerSource::Adjustment,
                    opacity: 0.5,
                    blend_policy: EffectPassBlendPolicy::Mix,
                    transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                    effect_plan: Some(&plan),
                    frame_seed: 0,
                },
            ];
            assert_eq!(
                validate_request(&GpuCompositeRequest {
                    width: 4,
                    height: 4,
                    working_color_space: WorkingColorSpace::LinearRec709,
                    layers: &layers,
                }),
                Err(GpuCompositeError::InputNotStraightCompatibleAlpha { actual: alpha }),
            );
        }
    }

    #[tokio::test]
    #[ignore = "requires an actual hardware GPU; independent finite Coverage cancellation oracle"]
    async fn gpu_mix_finite_coverage_cancellation_matches_exact_dyadic_oracle() {
        let context = crate::GpuContext::new().await.expect("actual GPU adapter required");
        let adapter_info = context.adapter.get_info();
        eprintln!("GPU_ADJUSTMENT_NATIVE_PRIMARY: {adapter_info:?}");
        assert!(
            matches!(
                adapter_info.device_type,
                wgpu::DeviceType::DiscreteGpu | wgpu::DeviceType::IntegratedGpu
            ),
            "actual hardware GPU required: {adapter_info:?}"
        );
        assert_eq!(
            product_gpu_working_texture_format(),
            GpuColorFrameTextureFormat::Rgba32Float,
            "this oracle requires the actual Float32 working route"
        );
        let rgb = f32::from_bits(0x6480_0000); // 2^74
        let amount = f32::from_bits(0x1a00_0000); // 2^-75
        let compositor = GpuFrameCompositor::new(&context.device).expect("GPU compositor");
        let mut ids = GpuColorFrameIdAllocator::new(1).expect("frame IDs");
        let mut table = GpuColorFrameResourceTable::new();
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("exact-associated-mix-two-input-textures"),
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
                Color { r: rgb, g: rgb, b: rgb, a: amount },
            )
            .expect("actual original texture")
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
                Color { r: -rgb, g: -rgb, b: -rgb, a: 1.0 },
            )
            .expect("actual processed texture")
            .output;
        let output = compositor
            .record_adjustment_blend_pass(
                &context.device,
                &context.queue,
                &mut encoder,
                &mut ids,
                &mut table,
                None,
                &left,
                &right,
                amount,
                mondrian_core::EffectPassBlendPolicy::Mix,
                0,
                WorkingColorSpace::LinearRec709,
            )
            .expect("actual admitted finite coverage Mix")
            .output;
        assert_eq!(
            output.descriptor().alpha,
            crate::ColorFrameAlpha::StraightCoverage
        );
        let readback = context.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("exact-associated-mix-readback"),
            size: 256 * 4,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let resource = table.get(&output).expect("actual output resource");
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &resource.resource().texture,
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
        // Exact Fraction arithmetic on the input Float32 values:
        // associated = -2^-76; A = 2^-74 - 2^-150.
        // One correct Float32 delivery is [-.25, -.25, -.25, 2^-74].
        // No CPU mixer, converter, compositing or shader supplies this oracle.
        let expected = [0xbe80_0000, 0xbe80_0000, 0xbe80_0000, 0x1a80_0000];
        for (row_index, row) in mapped.chunks_exact(256).take(4).enumerate() {
            for (column, pixel) in
                bytemuck::cast_slice::<u8, f32>(&row[..64]).chunks_exact(4).enumerate()
            {
                for (lane, bits) in pixel.iter().zip(expected) {
                    assert_eq!(
                        lane.to_bits(),
                        bits,
                        "pixel ({column},{row_index}) exact cancellation: {pixel:?}"
                    );
                }
            }
        }
        readback.unmap();
    }

    #[tokio::test]
    #[ignore = "requires an actual hardware GPU; numerical identity preserves the exact left signed-zero texel"]
    async fn gpu_mix_numerically_identical_hidden_rgb_keeps_signed_zero() {
        let context = crate::GpuContext::new().await.expect("actual GPU adapter required");
        let adapter_info = context.adapter.get_info();
        eprintln!("GPU_ADJUSTMENT_NATIVE_PRIMARY: {adapter_info:?}");
        assert!(
            matches!(
                adapter_info.device_type,
                wgpu::DeviceType::DiscreteGpu | wgpu::DeviceType::IntegratedGpu
            ),
            "actual hardware GPU required: {adapter_info:?}"
        );
        assert_eq!(
            product_gpu_working_texture_format(),
            GpuColorFrameTextureFormat::Rgba32Float,
            "this oracle requires the actual Float32 working route"
        );
        let amount = 0.5;
        let compositor = GpuFrameCompositor::new(&context.device).expect("GPU compositor");
        let mut ids = GpuColorFrameIdAllocator::new(1).expect("frame IDs");
        let mut table = GpuColorFrameResourceTable::new();
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("exact-associated-mix-two-input-textures"),
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
                Color {
                    r: -1.0,
                    g: 2.0,
                    b: f32::from_bits(0x8000_0000),
                    a: 0.0,
                },
            )
            .expect("actual original texture")
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
                Color { r: -1.0, g: 2.0, b: 0.0, a: 0.0 },
            )
            .expect("actual processed texture")
            .output;
        let output = compositor
            .record_adjustment_blend_pass(
                &context.device,
                &context.queue,
                &mut encoder,
                &mut ids,
                &mut table,
                None,
                &left,
                &right,
                amount,
                mondrian_core::EffectPassBlendPolicy::Mix,
                0,
                WorkingColorSpace::LinearRec709,
            )
            .expect("actual admitted finite coverage Mix")
            .output;
        assert_eq!(
            output.descriptor().alpha,
            crate::ColorFrameAlpha::StraightCoverage
        );
        let readback = context.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("exact-associated-mix-readback"),
            size: 256 * 4,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let resource = table.get(&output).expect("actual output resource");
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &resource.resource().texture,
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
        // +0 and -0 are numerically equal. Identical samples preserve
        // the exact original input, including nonzero hidden RGB and -0.
        // This expected left texel is independent from all mixing code.
        let expected = [0xbf80_0000, 0x4000_0000, 0x8000_0000, 0x0000_0000];
        for (row_index, row) in mapped.chunks_exact(256).take(4).enumerate() {
            for (column, pixel) in
                bytemuck::cast_slice::<u8, f32>(&row[..64]).chunks_exact(4).enumerate()
            {
                for (lane, bits) in pixel.iter().zip(expected) {
                    assert_eq!(
                        lane.to_bits(),
                        bits,
                        "pixel ({column},{row_index}) numerical identity: {pixel:?}"
                    );
                }
            }
        }
        readback.unmap();
    }

    #[test]
    fn ordinary_compositor_module_excludes_exact_adjustment_mix_arithmetic() {
        let ordinary = naga::front::wgsl::parse_str(GPU_COMPOSITOR_SHADER)
            .expect("ordinary composite shader parses");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&ordinary)
        .expect("ordinary composite shader validates");
        assert!(!ordinary
            .types
            .iter()
            .any(|(_, ty)| ty.name.as_deref().is_some_and(|name| name.starts_with("Mix"))));
        assert!(!ordinary.functions.iter().any(|(_, function)| function
            .name
            .as_deref()
            .is_some_and(|name| name.starts_with("mix_"))));
        let mix = naga::front::wgsl::parse_str(GPU_ADJUSTMENT_MIX_SHADER)
            .expect("dedicated Mix shader parses");
        assert!(mix.types.iter().any(|(_, ty)| ty.name.as_deref() == Some("MixUnsigned")));
        assert_eq!(
            mix.entry_points
                .iter()
                .filter(|entry| entry.stage == naga::ShaderStage::Fragment)
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["fs_mix"]
        );
    }

    #[tokio::test]
    #[ignore = "actual hardware GPU; canonical inline Mix owner/lazy-pipeline qualification"]
    async fn gpu_inline_mix_materializes_one_owned_processed_texture() {
        use mondrian_effects::{
            compile_reference_render_graph, lower_effect_graph_to_gpu_plan,
            EffectGraphBuilderState, EffectPassBlendPolicy, EffectRenderOp,
        };
        let context = crate::GpuContext::new().await.expect("actual GPU adapter required");
        let info = context.adapter.get_info();
        eprintln!("GPU_ADJUSTMENT_NATIVE_PRIMARY: {info:?}");
        assert!(matches!(
            info.device_type,
            wgpu::DeviceType::DiscreteGpu | wgpu::DeviceType::IntegratedGpu
        ));
        assert_eq!(
            product_gpu_working_texture_format(),
            GpuColorFrameTextureFormat::Rgba32Float
        );
        let compositor = GpuFrameCompositor::new(&context.device).expect("GPU compositor");
        assert!(compositor.adjustment_mix_pipeline.get().is_none());
        // The public record() path must independently skip identity Mix before
        // allocation; the visual preparation shortcut alone cannot prove it.
        let identity_graph =
            compile_reference_render_graph(EffectGraphBuilderState::new().finish())
                .expect("canonical identity graph");
        let identity_plan = lower_effect_graph_to_gpu_plan(&identity_graph).expect("identity plan");
        let identity_layers = [
            GpuCompositeLayer {
                source: GpuCompositeLayerSource::SolidColor(Color {
                    r: 2.0,
                    g: -3.0,
                    b: 4.0,
                    a: 1.0,
                }),
                opacity: 1.0,
                blend_policy: EffectPassBlendPolicy::Blend(BlendMode::Normal),
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_plan: None,
                frame_seed: 0,
            },
            GpuCompositeLayer {
                source: GpuCompositeLayerSource::Adjustment,
                opacity: 0.5,
                blend_policy: EffectPassBlendPolicy::Mix,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_plan: Some(&identity_plan),
                frame_seed: 0,
            },
        ];
        {
            let mut ids = GpuColorFrameIdAllocator::new(1).expect("identity frame IDs");
            let mut table = GpuColorFrameResourceTable::new();
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("inline-identity-mix-does-not-allocate-processed"),
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
                        layers: &identity_layers,
                    },
                )
                .expect("public identity record");
            assert_eq!(table.len(), 1, "ordinary opaque accumulator only");
            assert_eq!(table.logical_texture_bytes(), Some(4 * 4 * 16));
            assert!(compositor.adjustment_mix_pipeline.get().is_none());
            let submission = context.queue.submit(std::iter::once(encoder.finish()));
            context
                .device
                .poll(wgpu::PollType::Wait {
                    submission_index: Some(submission),
                    timeout: Some(std::time::Duration::from_secs(10)),
                })
                .expect("bounded identity completion before owners drop");
        }
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(EffectRenderOp::Crop { left: 0.5, top: 0.0, right: 0.0, bottom: 0.0 });
        let graph = compile_reference_render_graph(builder.finish()).expect("sole Crop graph");
        let plan = lower_effect_graph_to_gpu_plan(&graph).expect("sole compiled GPU point plan");
        let layers = [
            GpuCompositeLayer {
                source: GpuCompositeLayerSource::SolidColor(Color {
                    r: 2.0,
                    g: -3.0,
                    b: 4.0,
                    a: 1.0,
                }),
                opacity: 1.0,
                blend_policy: EffectPassBlendPolicy::Blend(BlendMode::Normal),
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_plan: None,
                frame_seed: 0,
            },
            GpuCompositeLayer {
                source: GpuCompositeLayerSource::Adjustment,
                opacity: 0.5,
                blend_policy: EffectPassBlendPolicy::Mix,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_plan: Some(&plan),
                frame_seed: 0,
            },
        ];
        let mut ids = GpuColorFrameIdAllocator::new(1).expect("frame IDs");
        let mut table = GpuColorFrameResourceTable::new();
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("canonical-inline-mix-real-resource-owner"),
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
                    layers: &layers,
                },
            )
            .expect("actual canonical inline Adjustment");
        assert_eq!(
            record.output.descriptor().alpha,
            crate::ColorFrameAlpha::StraightCoverage
        );
        assert_eq!(
            table.len(),
            3,
            "two accumulators plus one real processed owner"
        );
        assert_eq!(table.logical_texture_bytes(), Some(3 * 4 * 4 * 16));
        assert!(compositor.adjustment_mix_pipeline.get().is_some());
        let submission = context.queue.submit(std::iter::once(encoder.finish()));
        context
            .device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: Some(std::time::Duration::from_secs(10)),
            })
            .expect("bounded command completion before resource owners are dropped");
        // Pixel semantics are independently exercised by the unchanged canonical
        // [3,-6,12,.5] and Crop/explicit-Blend hardware oracles, not this ledger test.
    }

    #[test]
    fn inline_identity_mix_never_enters_processed_allocation_or_exact_pipeline() {
        use mondrian_effects::{
            compile_reference_render_graph, lower_effect_graph_to_gpu_plan,
            EffectGraphBuilderState, EffectPassBlendPolicy,
        };
        let graph = compile_reference_render_graph(EffectGraphBuilderState::new().finish())
            .expect("actual identity graph");
        let plan = lower_effect_graph_to_gpu_plan(&graph).expect("actual identity point plan");
        assert!(plan.is_identity());
        let mut layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::Adjustment,
            opacity: 0.5,
            blend_policy: EffectPassBlendPolicy::Mix,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: Some(&plan),
            frame_seed: 0,
        };
        assert!(layer_has_zero_contribution(&layer));
        assert!(matches!(
            composite_layer_footprint(4, 4, &layer),
            GpuCompositeLayerFootprint::NoContribution
        ));
        layer.blend_policy = EffectPassBlendPolicy::Blend(BlendMode::Normal);
        assert!(
            !layer_has_zero_contribution(&layer),
            "explicit Normal is still Over"
        );
        layer.opacity = 0.0;
        assert!(
            layer_has_zero_contribution(&layer),
            "zero opacity is inactive for either policy"
        );
    }

    #[test]
    fn gpu_opacity_rejects_nonfinite_and_out_of_range_before_inactive_or_identity_elision() {
        use mondrian_effects::{
            compile_reference_render_graph, lower_effect_graph_to_gpu_plan,
            EffectGraphBuilderState, EffectPassBlendPolicy,
        };
        let graph = compile_reference_render_graph(EffectGraphBuilderState::new().finish())
            .expect("canonical identity graph");
        let plan = lower_effect_graph_to_gpu_plan(&graph).expect("actual identity plan");
        for opacity in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -0.1, 1.1] {
            let ordinary = GpuCompositeLayer {
                source: GpuCompositeLayerSource::SolidColor(Color {
                    r: 1.0,
                    g: -2.0,
                    b: 3.0,
                    a: 1.0,
                }),
                opacity,
                blend_policy: EffectPassBlendPolicy::Blend(BlendMode::Normal),
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_plan: None,
                frame_seed: 0,
            };
            let identity = GpuCompositeLayer {
                source: GpuCompositeLayerSource::Adjustment,
                opacity,
                blend_policy: EffectPassBlendPolicy::Mix,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_plan: Some(&plan),
                frame_seed: 0,
            };
            for layer in [ordinary, identity] {
                let result = validate_request(&GpuCompositeRequest {
                    width: 4,
                    height: 4,
                    working_color_space: WorkingColorSpace::LinearRec709,
                    layers: &[layer],
                });
                assert!(
                    result.is_err(),
                    "invalid opacity {:08x} was admitted",
                    opacity.to_bits()
                );
            }
        }
    }

    #[test]
    fn gpu_opacity_refusal_retains_exact_bits_and_normalized_endpoints() {
        use mondrian_effects::{
            compile_reference_render_graph, lower_effect_graph_to_gpu_plan,
            EffectGraphBuilderState, EffectPassBlendPolicy,
        };
        let graph = compile_reference_render_graph(EffectGraphBuilderState::new().finish())
            .expect("canonical identity graph");
        let plan = lower_effect_graph_to_gpu_plan(&graph).expect("actual identity plan");
        for bits in [
            0x7fc1_2345_u32,
            0xffc1_2345,
            0x7f80_0000,
            0xff80_0000,
            (-0.1_f32).to_bits(),
            1.1_f32.to_bits(),
        ] {
            let layer = GpuCompositeLayer {
                source: GpuCompositeLayerSource::Adjustment,
                opacity: f32::from_bits(bits),
                blend_policy: EffectPassBlendPolicy::Mix,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_plan: Some(&plan),
                frame_seed: 0,
            };
            assert_eq!(
                validate_request(&GpuCompositeRequest {
                    width: 4,
                    height: 4,
                    working_color_space: WorkingColorSpace::LinearRec709,
                    layers: &[layer],
                }),
                Err(GpuCompositeError::InvalidOpacity { bits })
            );
        }
        for opacity in [-0.0_f32, 0.0, f32::from_bits(1), 0.5, 1.0] {
            let layer = GpuCompositeLayer {
                source: GpuCompositeLayerSource::Adjustment,
                opacity,
                blend_policy: EffectPassBlendPolicy::Mix,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_plan: Some(&plan),
                frame_seed: 0,
            };
            assert_eq!(
                validate_request(&GpuCompositeRequest {
                    width: 4,
                    height: 4,
                    working_color_space: WorkingColorSpace::LinearRec709,
                    layers: &[layer],
                }),
                Ok(())
            );
        }
    }

    #[test]
    fn exact_adjustment_mix_shader_has_a_valid_typed_module() {
        let module =
            naga::front::wgsl::parse_str(GPU_ADJUSTMENT_MIX_SHADER).expect("GPU Mix shader parses");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("complete Mix shader type validation");
    }

    #[tokio::test]
    #[ignore = "requires an actual hardware GPU; exact Fraction-generated finite Coverage corpus"]
    async fn gpu_mix_exact_coverage_dyadic_boundary_corpus() {
        let context = crate::GpuContext::new().await.expect("actual GPU adapter required");
        let adapter_info = context.adapter.get_info();
        eprintln!("GPU_ADJUSTMENT_NATIVE_PRIMARY: {adapter_info:?}");
        assert!(
            matches!(
                adapter_info.device_type,
                wgpu::DeviceType::DiscreteGpu | wgpu::DeviceType::IntegratedGpu
            ),
            "actual hardware GPU required: {adapter_info:?}"
        );
        assert_eq!(
            product_gpu_working_texture_format(),
            GpuColorFrameTextureFormat::Rgba32Float
        );
        let compositor = GpuFrameCompositor::new(&context.device).expect("GPU compositor");
        assert!(
            compositor.adjustment_mix_pipeline.get().is_none(),
            "Mix is not eager"
        );
        let mut owned_mix_pipeline = None;
        let mut ids = GpuColorFrameIdAllocator::new(1).expect("frame IDs");
        // Exact expected Float32 bits were generated by the independent
        // Fraction/lattice-midpoint oracle, never the production shader or
        // CPU mixer. This includes max finite HDR, minimum positive alpha,
        // rounded-zero alpha with representable straight RGB, subnormal
        // delivery, ties-even, chosen endpoints and hidden RGB identity.
        // Background, processed, opacity and expected output bits.
        type MixCase = ([u32; 4], [u32; 4], u32, [u32; 4]);
        let cases: [MixCase; 15] = [
            (
                [0x64800000, 0x64800000, 0x64800000, 0x1a000000],
                [0xe4800000, 0xe4800000, 0xe4800000, 0x3f800000],
                0x1a000000,
                [0xbe800000, 0xbe800000, 0xbe800000, 0x1a800000],
            ),
            (
                [0x3f800000, 0x3f800000, 0x3f800000, 0x00000000],
                [0x40000000, 0x40000000, 0x40000000, 0x00000001],
                0x00000001,
                [0x40000000, 0x40000000, 0x40000000, 0x00000000],
            ),
            (
                [0x7f7fffff, 0x7f7fffff, 0x7f7fffff, 0x3f800000],
                [0xff7fffff, 0xff7fffff, 0xff7fffff, 0x3f800000],
                0x3f000000,
                [0x00000000, 0x00000000, 0x00000000, 0x3f800000],
            ),
            (
                [0x7f7fffff, 0x7f7fffff, 0x7f7fffff, 0x00000001],
                [0xff7fffff, 0xff7fffff, 0xff7fffff, 0x3f800000],
                0x00000001,
                [0xb47fffff, 0xb47fffff, 0xb47fffff, 0x00000002],
            ),
            (
                [0x7f7fffff, 0x7f7fffff, 0x7f7fffff, 0x00000001],
                [0x7f7fffff, 0x7f7fffff, 0x7f7fffff, 0x3f800000],
                0x00000001,
                [0x7f7fffff, 0x7f7fffff, 0x7f7fffff, 0x00000002],
            ),
            (
                [0x00000001, 0x00000001, 0x00000001, 0x3f800000],
                [0x00000002, 0x00000002, 0x00000002, 0x3f800000],
                0x3f000000,
                [0x00000002, 0x00000002, 0x00000002, 0x3f800000],
            ),
            (
                [0x00000002, 0x00000002, 0x00000002, 0x3f800000],
                [0x00000003, 0x00000003, 0x00000003, 0x3f800000],
                0x3f000000,
                [0x00000002, 0x00000002, 0x00000002, 0x3f800000],
            ),
            (
                [0x80000001, 0x80000001, 0x80000001, 0x3f800000],
                [0x80000002, 0x80000002, 0x80000002, 0x3f800000],
                0x3f000000,
                [0x80000002, 0x80000002, 0x80000002, 0x3f800000],
            ),
            (
                [0x3f800000, 0x3f800000, 0x3f800000, 0x3f800000],
                [0x3f800001, 0x3f800001, 0x3f800001, 0x3f800000],
                0x3f000000,
                [0x3f800000, 0x3f800000, 0x3f800000, 0x3f800000],
            ),
            (
                [0x3f800001, 0x3f800001, 0x3f800001, 0x3f800000],
                [0x3f800002, 0x3f800002, 0x3f800002, 0x3f800000],
                0x3f000000,
                [0x3f800002, 0x3f800002, 0x3f800002, 0x3f800000],
            ),
            (
                [0x7f7fffff, 0x7f7fffff, 0x7f7fffff, 0x3f800000],
                [0xff7fffff, 0xff7fffff, 0xff7fffff, 0x3f800000],
                0x3f7fffff,
                [0xff7ffffd, 0xff7ffffd, 0xff7ffffd, 0x3f800000],
            ),
            (
                [0x40e00000, 0xc0400000, 0x41300000, 0x00000000],
                [0x3f800000, 0x3f800000, 0x3f800000, 0x00000001],
                0x00000000,
                [0x40e00000, 0xc0400000, 0x41300000, 0x00000000],
            ),
            (
                [0x3f800000, 0x3f800000, 0x3f800000, 0x00000001],
                [0x40e00000, 0xc0400000, 0x41300000, 0x00000000],
                0x3f800000,
                [0x40e00000, 0xc0400000, 0x41300000, 0x00000000],
            ),
            (
                [0x40e00000, 0xc0400000, 0x41300000, 0x00000000],
                [0x40e00000, 0xc0400000, 0x41300000, 0x00000000],
                0x3f000000,
                [0x40e00000, 0xc0400000, 0x41300000, 0x00000000],
            ),
            (
                [0x40e00000, 0xc0400000, 0x41300000, 0x00000000],
                [0x40000000, 0x40000000, 0x40000000, 0x00000000],
                0x3f000000,
                [0x00000000, 0x00000000, 0x00000000, 0x00000000],
            ),
        ];
        for (case_index, (left, right, amount, expected)) in cases.into_iter().enumerate() {
            let mut table = GpuColorFrameResourceTable::new();
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("exact-mix-independent-corpus"),
                });
            let mut sources = Vec::new();
            for input in [left, right] {
                let input = input.map(f32::from_bits);
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
                        Color { r: input[0], g: input[1], b: input[2], a: input[3] },
                    )
                    .expect("actual corpus input texture");
                sources.push(record.output);
            }
            if owned_mix_pipeline.is_none() {
                assert!(
                    compositor.adjustment_mix_pipeline.get().is_none(),
                    "ordinary source materialization must not compile Mix"
                );
            }
            let output = compositor
                .record_adjustment_blend_pass(
                    &context.device,
                    &context.queue,
                    &mut encoder,
                    &mut ids,
                    &mut table,
                    None,
                    &sources[0],
                    &sources[1],
                    f32::from_bits(amount),
                    mondrian_core::EffectPassBlendPolicy::Mix,
                    0,
                    WorkingColorSpace::LinearRec709,
                )
                .expect("admitted exact finite Coverage Mix")
                .output;
            let actual_pipeline = compositor
                .adjustment_mix_pipeline
                .get()
                .expect("actual typed Mix initialized its own pipeline")
                as *const _;
            if let Some(original_pipeline) = owned_mix_pipeline {
                assert_eq!(
                    actual_pipeline, original_pipeline,
                    "same compositor reuses Mix pipeline"
                );
            } else {
                owned_mix_pipeline = Some(actual_pipeline);
            }

            let readback = context.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("exact-mix-corpus-readback"),
                size: 256 * 4,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let resource = table.get(&output).expect("actual output resource");
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: &resource.resource().texture,
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
            for (row_index, row) in mapped.chunks_exact(256).take(4).enumerate() {
                for (column, pixel) in
                    bytemuck::cast_slice::<u8, f32>(&row[..64]).chunks_exact(4).enumerate()
                {
                    for (lane, bits) in pixel.iter().zip(expected) {
                        assert_eq!(lane.to_bits(), bits,
                            "case {case_index}, pixel ({column},{row_index}), left={left:x?}, right={right:x?}, amount={amount:x}: {pixel:?}");
                    }
                }
            }
            readback.unmap();
        }
    }

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
            opaque_normal_single_accumulator_composites: 2,
            avoided_accumulator_sample_pixels: 800,
            gpu_composited_pixels: 1000,
            execution: GpuCompositeExecutionDiagnostics {
                render_passes: 3,
                shaded_pixels: 1000,
                ..GpuCompositeExecutionDiagnostics::default()
            },
            ..GpuCompositingDiagnostics::default()
        };
        let b = GpuCompositingDiagnostics {
            gpu_passthrough_frames: 1,
            cpu_fallback_composites: 1,
            opaque_normal_single_accumulator_composites: 1,
            avoided_accumulator_sample_pixels: 200,
            cpu_composited_pixels: 500,
            execution: GpuCompositeExecutionDiagnostics {
                render_passes: 1,
                shaded_pixels: 250,
                avoided_shader_pixels: 250,
                ..GpuCompositeExecutionDiagnostics::default()
            },
            first_blocker: Some(GpuCompositingBlockerReason::EffectRequiresCpu),
            ..GpuCompositingDiagnostics::default()
        };
        a.accumulate(b);
        assert_eq!(a.gpu_passthrough_frames, 3);
        assert_eq!(a.gpu_native_composites, 3);
        assert_eq!(a.opaque_normal_single_accumulator_composites, 3);
        assert_eq!(a.avoided_accumulator_sample_pixels, 1000);
        assert_eq!(a.cpu_fallback_composites, 1);
        assert_eq!(a.gpu_composited_pixels, 1000);
        assert_eq!(a.cpu_composited_pixels, 500);
        assert_eq!(a.execution.render_passes, 4);
        assert_eq!(a.execution.shaded_pixels, 1250);
        assert_eq!(a.execution.avoided_shader_pixels, 250);
        assert_eq!(
            a.first_blocker,
            Some(GpuCompositingBlockerReason::EffectRequiresCpu)
        );
    }

    #[test]
    fn composite_execution_eliminates_exact_no_op_layers() {
        let layers = [GpuCompositeLayer {
            source: GpuCompositeLayerSource::SolidColor(Color { r: 4.0, g: -2.0, b: 1.0, a: 0.0 }),
            opacity: 1.0,
            blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: None,
            frame_seed: 0,
        }];
        let footprints = layers
            .iter()
            .map(|layer| composite_layer_footprint(1920, 1080, layer))
            .collect::<Vec<_>>();
        let plan = GpuCompositeExecutionPlanner::plan(
            1920,
            1080,
            &footprints,
            GpuCompositeExecutionPolicy::default(),
        )
        .expect("plan");

        assert!(plan.layers.is_empty());
        assert_eq!(plan.diagnostics.eliminated_layers, 1);
        assert_eq!(plan.diagnostics.render_passes, 0);
        assert_eq!(plan.diagnostics.shaded_pixels, 0);
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
                alpha: crate::ColorFrameAlpha::Opaque,
            },
            GpuColorFrameTextureFormat::Rgba32Float,
            "passthrough-test",
        )
        .expect("valid passthrough handle");
        let mut layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::GpuFrame(&handle),
            opacity: 1.0,
            blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
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

        let straight = GpuColorFrameHandle::new(
            crate::GpuColorFrameId::from_raw(100),
            ColorFrameDescriptor {
                alpha: ColorFrameAlpha::StraightCoverage,
                ..handle.descriptor()
            },
            GpuColorFrameTextureFormat::Rgba32Float,
            "straight-coverage-source",
        )
        .expect("valid straight coverage handle");
        let straight_layers = [GpuCompositeLayer {
            source: GpuCompositeLayerSource::GpuFrame(&straight),
            ..layer
        }];
        let straight_request = GpuCompositeRequest { layers: &straight_layers, ..request };
        assert_eq!(single_layer_gpu_passthrough(&straight_request), None);

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
            GpuCompositingBlockerReason::SpatialChainUnsupported,
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
    fn gpu_effect_uniforms_carry_the_packed_invert_scope() {
        use mondrian_effects::{
            compile_reference_render_graph, lower_effect_graph_to_gpu_plan,
            EffectGraphBuilderState, EffectRenderOp, InvertChannels,
        };

        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(EffectRenderOp::Invert {
            channels: InvertChannels::RedGreenAlpha,
            blend_with_original: 0.25,
        });
        let graph = compile_reference_render_graph(builder.finish()).expect("invert graph");
        let plan = lower_effect_graph_to_gpu_plan(&graph).expect("invert GPU plan");
        assert_eq!(plan.operations().len(), 1);

        let uniforms = effect_uniforms(Some(&plan), None).expect("invert uniforms");
        assert_eq!(uniforms[0].header, [13, 0b011, 0b0001, 0]);
        assert_eq!(uniforms[0].params, [0.25, 0.0, 0.0, 0.0]);
        assert_eq!(uniforms[1].header[0], 0);

        let mut hls = EffectGraphBuilderState::new();
        hls.append_unary(EffectRenderOp::Invert {
            channels: InvertChannels::Hls,
            blend_with_original: 0.0,
        });
        let graph = compile_reference_render_graph(hls.finish()).expect("hls graph");
        let plan = lower_effect_graph_to_gpu_plan(&graph).expect("hls GPU plan");
        let uniforms = effect_uniforms(Some(&plan), None).expect("hls uniforms");
        assert_eq!(uniforms[0].header, [13, 0, 0b1110, 0]);
    }

    #[test]
    fn gpu_effect_uniforms_carry_the_hue_saturation_controls() {
        use mondrian_effects::{
            compile_reference_render_graph, lower_effect_graph_to_gpu_plan,
            EffectGraphBuilderState, EffectRenderOp, HueBand, HueSaturationGrade,
        };

        let grade =
            HueSaturationGrade::new(HueBand::Greens, -90.0, 25.0, -12.0, 0.5, 45.0, 60.0, 8.0)
                .expect("valid hue/saturation grade");
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(EffectRenderOp::HueSaturation { grade });
        let graph = compile_reference_render_graph(builder.finish()).expect("hue/saturation graph");
        let plan = lower_effect_graph_to_gpu_plan(&graph).expect("hue/saturation GPU plan");
        assert_eq!(plan.operations().len(), 1);

        let uniforms = effect_uniforms(Some(&plan), None).expect("hue/saturation uniforms");
        let controls = grade.gpu_controls();
        assert_eq!(uniforms[0].header, [14, 0, 0, 0]);
        assert_eq!(
            uniforms[0].params,
            [controls[0], controls[1], controls[2], controls[3]]
        );
        assert_eq!(
            uniforms[0].color,
            [controls[4], controls[5], controls[6], controls[7]]
        );
        assert_eq!(uniforms[1].header[0], 0);

        let master = HueSaturationGrade::new(HueBand::Master, 0.0, 0.0, 0.0, 0.0, 0.0, 25.0, 0.0)
            .expect("valid master grade");
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(EffectRenderOp::HueSaturation { grade: master });
        let graph = compile_reference_render_graph(builder.finish()).expect("master graph");
        let plan = lower_effect_graph_to_gpu_plan(&graph).expect("master GPU plan");
        let uniforms = effect_uniforms(Some(&plan), None).expect("master uniforms");
        assert!(
            uniforms[0].color[3] < 0.0,
            "master band packs a negative center"
        );
    }

    #[test]
    fn gpu_effect_uniforms_carry_the_black_and_white_weights() {
        use mondrian_effects::{
            compile_reference_render_graph, lower_effect_graph_to_gpu_plan, BlackAndWhiteGrade,
            EffectGraphBuilderState, EffectRenderOp,
        };

        let grade = BlackAndWhiteGrade::new(
            [40.0, 60.0, 40.0, 60.0, 20.0, 80.0],
            0.25,
            [0.88, 0.83, 0.70],
        )
        .expect("valid black and white grade");
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(EffectRenderOp::BlackAndWhite { grade });
        let graph =
            compile_reference_render_graph(builder.finish()).expect("black and white graph");
        let plan = lower_effect_graph_to_gpu_plan(&graph).expect("black and white GPU plan");
        assert_eq!(plan.operations().len(), 1);

        let uniforms = effect_uniforms(Some(&plan), None).expect("black and white uniforms");
        let controls = grade.gpu_controls();
        assert_eq!(uniforms[0].header, [15, 0, 0, 0]);
        assert_eq!(
            uniforms[0].params,
            [controls[0], controls[1], controls[2], controls[3]]
        );
        assert_eq!(
            uniforms[0].color,
            [controls[4], controls[5], controls[6], 0.0]
        );
        assert_eq!(
            uniforms[0].extra0,
            [controls[7], controls[8], controls[9], 0.0]
        );
        assert_eq!(uniforms[1].header[0], 0);
    }

    #[test]
    fn gpu_effect_uniforms_carry_the_channel_mixer_rows() {
        use mondrian_effects::{
            compile_reference_render_graph, lower_effect_graph_to_gpu_plan, ChannelMixerGrade,
            EffectGraphBuilderState, EffectRenderOp,
        };

        let mut weights = [0.0_f32; 12];
        weights[1] = 100.0;
        weights[5] = 100.0;
        weights[10] = 100.0;
        weights[11] = 25.0;
        let grade = ChannelMixerGrade::new(weights, true).expect("valid mixer");
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(EffectRenderOp::ChannelMixer { grade });
        let graph = compile_reference_render_graph(builder.finish()).expect("channel mixer graph");
        let plan = lower_effect_graph_to_gpu_plan(&graph).expect("channel mixer GPU plan");

        let uniforms = effect_uniforms(Some(&plan), None).expect("channel mixer uniforms");
        assert_eq!(uniforms[0].header, [17, 0, 0, 0]);
        assert_eq!(uniforms[0].params, [0.0, 100.0, 0.0, 0.0]);
        assert_eq!(uniforms[0].color, [0.0, 100.0, 0.0, 0.0]);
        assert_eq!(uniforms[0].extra0, [0.0, 0.0, 100.0, 25.0]);
        assert_eq!(uniforms[0].extra1, [1.0, 0.0, 0.0, 0.0]);
        assert_eq!(uniforms[1].header[0], 0);
    }

    #[test]
    fn gpu_effect_uniforms_carry_the_shift_channels_sources() {
        use mondrian_effects::{
            compile_reference_render_graph, lower_effect_graph_to_gpu_plan, ChannelSource,
            EffectGraphBuilderState, EffectRenderOp, ShiftChannelsGrade,
        };

        let grade = ShiftChannelsGrade::new(
            ChannelSource::Green,
            ChannelSource::Red,
            ChannelSource::Alpha,
            ChannelSource::Luminance,
            [0.2126, 0.7152, 0.0722],
        );
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(EffectRenderOp::ShiftChannels { grade });
        let graph = compile_reference_render_graph(builder.finish()).expect("shift channels graph");
        let plan = lower_effect_graph_to_gpu_plan(&graph).expect("shift channels GPU plan");
        assert_eq!(plan.operations().len(), 1);

        let uniforms = effect_uniforms(Some(&plan), None).expect("shift channels uniforms");
        assert_eq!(uniforms[0].header, [16, grade.packed_sources(), 0, 0]);
        assert_eq!(uniforms[0].params, [0.2126, 0.7152, 0.0722, 0.0]);
        assert_eq!(uniforms[0].header[0], 16);
        assert_eq!(uniforms[1].header[0], 0);
    }

    #[test]
    fn gpu_composite_request_rejects_adjustment_without_effect_plan() {
        let layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::Adjustment,
            opacity: 1.0,
            blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
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
            blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
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
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
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
        let expected = apply_compiled_effect_graph_rgba_f32(
            mondrian_effects::EffectFloatFrameView::new(
                mondrian_effects::EffectFrameExtent::new(4, 4),
                mondrian_effects::EffectValueFormat::new(
                    mondrian_effects::EffectWorkingPrecision::Float32,
                    mondrian_effects::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &frame.rgba_f32().data,
            ),
            &graph,
            23,
        )
        .expect("CPU float reference");

        let media_layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::CpuFrame(&frame),
            opacity: 1.0,
            blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: Some(&plan),
            frame_seed: 23,
        };
        let (actual, diagnostics) = readback_test_composite_with_diagnostics(
            &context,
            &[media_layer],
            4,
            4,
            WorkingColorSpace::LinearRec709,
        );
        assert_eq!(diagnostics.execution.fused_layer_passes, 1);
        assert_eq!(
            diagnostics.execution.fused_point_operations,
            plan.operations().len() as u64
        );
        assert_test_pixels_accurate(&expected, &actual);

        let adjustment_opacity = 0.55;
        let expected_adjustment = apply_compiled_effect_graph_pass_rgba_f32(
            mondrian_effects::EffectFloatFrameView::new(
                mondrian_effects::EffectFrameExtent::new(4, 4),
                mondrian_effects::EffectValueFormat::new(
                    mondrian_effects::EffectWorkingPrecision::Float32,
                    mondrian_effects::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &frame.rgba_f32().data,
            ),
            &graph,
            adjustment_opacity,
            mondrian_effects::EffectPassBlendPolicy::Mix,
            23,
        )
        .expect("CPU float adjustment reference");
        let base_layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::CpuFrame(&frame),
            opacity: 1.0,
            blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: None,
            frame_seed: 0,
        };
        let adjustment_layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::Adjustment,
            opacity: adjustment_opacity,
            blend_policy: mondrian_effects::EffectPassBlendPolicy::Mix,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: Some(&plan),
            frame_seed: 23,
        };
        let actual_adjustment = readback_test_composite(&context, &[base_layer, adjustment_layer]);
        assert_test_pixels_accurate(&expected_adjustment, &actual_adjustment);

        let aces_frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
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
        let aces_expected = apply_compiled_effect_graph_rgba_f32(
            mondrian_effects::EffectFloatFrameView::new(
                mondrian_effects::EffectFrameExtent::new(2, 2),
                mondrian_effects::EffectValueFormat::new(
                    mondrian_effects::EffectWorkingPrecision::Float32,
                    mondrian_effects::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &aces_frame.rgba_f32().data,
            ),
            &aces_graph,
            0,
        )
        .expect("ACES CPU reference");
        let aces_layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::CpuFrame(&aces_frame),
            opacity: 1.0,
            blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
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
    async fn gpu_posterize_matches_the_cpu_reference_on_real_wgpu_device() {
        use crate::color_accuracy::{
            compare_linear_rgba, LinearAccuracyBudget, LinearRgbaAccuracyBudget,
        };
        use mondrian_effects::{
            apply_compiled_effect_graph_rgba_f32, compile_reference_render_graph,
            lower_effect_graph_to_gpu_plan, EffectGraphBuilderState, EffectRenderOp,
            PosterizeGrade,
        };

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping GPU posterize parity test: no GPU adapter available");
            return;
        };
        let data = vec![
            [0.1_f32, 0.5, 0.9, 1.0],
            [0.04, 0.08, 0.5, 1.0],
            [2.0, -0.5, 1.75, 0.5],
            [0.0, 1.0, 0.25, 0.25],
        ];
        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
            width: 2,
            height: 2,
            color_space: WorkingColorSpace::LinearRec709,
            data,
        });
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(EffectRenderOp::Posterize {
            grade: PosterizeGrade::new(7).expect("valid posterize"),
        });
        let graph = compile_reference_render_graph(builder.finish()).expect("valid graph");
        let plan = lower_effect_graph_to_gpu_plan(&graph).expect("supported point effect");
        assert_eq!(plan.operations().len(), 1);
        let expected = apply_compiled_effect_graph_rgba_f32(
            mondrian_effects::EffectFloatFrameView::new(
                mondrian_effects::EffectFrameExtent::new(2, 2),
                mondrian_effects::EffectValueFormat::new(
                    mondrian_effects::EffectWorkingPrecision::Float32,
                    mondrian_effects::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &frame.rgba_f32().data,
            ),
            &graph,
            0,
        )
        .expect("CPU float reference");
        let layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::CpuFrame(&frame),
            opacity: 1.0,
            blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: Some(&plan),
            frame_seed: 0,
        };
        let (actual, diagnostics) = readback_test_composite_with_diagnostics(
            &context,
            &[layer],
            2,
            2,
            WorkingColorSpace::LinearRec709,
        );
        assert_eq!(diagnostics.execution.fused_point_operations, 1);
        let report = compare_linear_rgba(
            expected.pixels(),
            &actual,
            LinearRgbaAccuracyBudget {
                rgb: LinearAccuracyBudget::finite(1.0e-6, 1.0e-7, 1.0e-7),
                alpha: LinearAccuracyBudget::finite(1.0e-6, 1.0e-7, 1.0e-6),
            },
        )
        .expect("comparable frames");
        assert!(
            report.within_budget,
            "GPU posterize accuracy budget exceeded: {report:#?}"
        );
    }

    #[tokio::test]
    async fn gpu_extract_matches_the_cpu_reference_on_real_wgpu_device() {
        use crate::color_accuracy::{
            compare_linear_rgba, LinearAccuracyBudget, LinearRgbaAccuracyBudget,
        };
        use mondrian_effects::{
            apply_compiled_effect_graph_rgba_f32, compile_reference_render_graph,
            lower_effect_graph_to_gpu_plan, EffectGraphBuilderState, EffectRenderOp,
            ExtractChannel, ExtractGrade,
        };

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping GPU extract parity test: no GPU adapter available");
            return;
        };
        let data = vec![
            [0.18_f32, 0.18, 0.18, 1.0],
            [0.25, 0.55, 0.05, 1.0],
            [2.0, -0.5, 1.75, 0.5],
            [0.0, 0.0, 0.0, 0.0],
            [0.5, 0.9, 0.1, 1.0],
            [0.35, 0.35, 0.35, 0.25],
        ];
        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
            width: 3,
            height: 2,
            color_space: WorkingColorSpace::LinearRec709,
            data,
        });
        let coefficients = WorkingColorSpace::LinearRec709.luminance_coefficients();
        for (channel, black, white, black_softness, white_softness, invert) in [
            (
                ExtractChannel::Luma,
                0.0_f32,
                1.0_f32,
                0.0_f32,
                0.0_f32,
                false,
            ),
            (ExtractChannel::Red, 0.2, 0.8, 0.1, 0.15, false),
            (ExtractChannel::Blue, 0.1, 0.9, 0.0, 0.2, true),
            (ExtractChannel::Alpha, 0.25, 0.75, 0.05, 0.05, false),
        ] {
            let grade = ExtractGrade::new(
                channel,
                black,
                white,
                black_softness,
                white_softness,
                invert,
                coefficients,
            )
            .expect("valid extract grade");
            let mut builder = EffectGraphBuilderState::new();
            builder.append_unary(EffectRenderOp::Extract { grade });
            let graph = compile_reference_render_graph(builder.finish()).expect("valid graph");
            let plan = lower_effect_graph_to_gpu_plan(&graph).expect("supported point effect");
            assert_eq!(plan.operations().len(), 1);
            let expected = apply_compiled_effect_graph_rgba_f32(
                mondrian_effects::EffectFloatFrameView::new(
                    mondrian_effects::EffectFrameExtent::new(3, 2),
                    mondrian_effects::EffectValueFormat::new(
                        mondrian_effects::EffectWorkingPrecision::Float32,
                        mondrian_effects::EffectColorDomain::SceneLinearRgb,
                        mondrian_core::ColorFrameAlpha::StraightCoverage,
                    ),
                    mondrian_core::ImageSampling::UNIT_SQUARE,
                    &frame.rgba_f32().data,
                ),
                &graph,
                0,
            )
            .expect("CPU float reference");
            let layer = GpuCompositeLayer {
                source: GpuCompositeLayerSource::CpuFrame(&frame),
                opacity: 1.0,
                blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_plan: Some(&plan),
                frame_seed: 0,
            };
            let (actual, diagnostics) = readback_test_composite_with_diagnostics(
                &context,
                &[layer],
                3,
                2,
                WorkingColorSpace::LinearRec709,
            );
            assert_eq!(diagnostics.execution.fused_point_operations, 1);
            let report = compare_linear_rgba(
                expected.pixels(),
                &actual,
                LinearRgbaAccuracyBudget {
                    rgb: LinearAccuracyBudget::finite(1.0e-6, 1.0e-7, 1.0e-6),
                    alpha: LinearAccuracyBudget::finite(1.0e-6, 1.0e-7, 1.0e-6),
                },
            )
            .expect("comparable frames");
            assert!(
                report.within_budget,
                "GPU extract accuracy budget exceeded for {channel:?}: {report:#?}"
            );
        }
    }

    #[tokio::test]
    async fn gpu_brightness_contrast_matches_the_cpu_kernel() {
        use crate::color_accuracy::{
            compare_linear_rgba, LinearAccuracyBudget, LinearRgbaAccuracyBudget,
        };
        use mondrian_effects::{
            apply_compiled_effect_graph_rgba_f32, compile_reference_render_graph,
            lower_effect_graph_to_gpu_plan, BrightnessContrastGrade, EffectGraphBuilderState,
            EffectRenderOp,
        };

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping GPU brightness contrast parity test: no GPU adapter available");
            return;
        };
        let data = vec![
            [0.18_f32, 0.18, 0.18, 1.0],
            [0.25, 0.55, 0.05, 1.0],
            [2.5, -0.4, 0.9, 1.0],
            [0.0, 0.0, 0.0, 0.0],
            [0.5, 0.9, 0.1, 1.0],
            [0.35, 0.35, 0.35, 0.25],
        ];
        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
            width: 3,
            height: 2,
            color_space: WorkingColorSpace::LinearRec709,
            data,
        });
        for (brightness, contrast, legacy) in [
            (0.0_f32, 0.0_f32, false),
            (60.0, 50.0, false),
            (-40.0, -70.0, false),
            (-30.0, 25.0, true),
        ] {
            let grade = BrightnessContrastGrade::new(brightness, contrast, legacy)
                .expect("valid brightness contrast grade");
            let mut builder = EffectGraphBuilderState::new();
            builder.append_unary(EffectRenderOp::BrightnessContrast { grade });
            let graph = compile_reference_render_graph(builder.finish()).expect("valid graph");
            let plan = lower_effect_graph_to_gpu_plan(&graph).expect("supported point effect");
            assert_eq!(plan.operations().len(), 1);
            let expected = apply_compiled_effect_graph_rgba_f32(
                mondrian_effects::EffectFloatFrameView::new(
                    mondrian_effects::EffectFrameExtent::new(3, 2),
                    mondrian_effects::EffectValueFormat::new(
                        mondrian_effects::EffectWorkingPrecision::Float32,
                        mondrian_effects::EffectColorDomain::SceneLinearRgb,
                        mondrian_core::ColorFrameAlpha::StraightCoverage,
                    ),
                    mondrian_core::ImageSampling::UNIT_SQUARE,
                    &frame.rgba_f32().data,
                ),
                &graph,
                0,
            )
            .expect("CPU float reference");
            let layer = GpuCompositeLayer {
                source: GpuCompositeLayerSource::CpuFrame(&frame),
                opacity: 1.0,
                blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_plan: Some(&plan),
                frame_seed: 0,
            };
            let (actual, diagnostics) = readback_test_composite_with_diagnostics(
                &context,
                &[layer],
                3,
                2,
                WorkingColorSpace::LinearRec709,
            );
            assert_eq!(diagnostics.execution.fused_point_operations, 1);
            let report = compare_linear_rgba(
                expected.pixels(),
                &actual,
                LinearRgbaAccuracyBudget {
                    rgb: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
                    alpha: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
                },
            )
            .expect("comparable frames");
            assert!(
                report.within_budget,
                "GPU brightness contrast accuracy budget exceeded for ({brightness}, {contrast}, {legacy}): {report:#?}"
            );
        }
    }

    #[tokio::test]
    async fn gpu_levels_matches_the_cpu_kernel() {
        use crate::color_accuracy::{
            compare_linear_rgba, LinearAccuracyBudget, LinearRgbaAccuracyBudget,
        };
        use mondrian_effects::{
            apply_compiled_effect_graph_rgba_f32, compile_reference_render_graph,
            lower_effect_graph_to_gpu_plan, EffectGraphBuilderState, EffectRenderOp, LevelsChannel,
            LevelsClip, LevelsGrade,
        };

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping GPU levels parity test: no GPU adapter available");
            return;
        };
        let data = vec![
            [0.18_f32, 0.18, 0.18, 1.0],
            [0.25, 0.55, 0.05, 1.0],
            [2.5, -0.4, 0.9, 1.0],
            [0.0, 0.0, 0.0, 0.0],
            [0.5, 0.9, 0.1, 1.0],
            [0.35, 0.35, 0.35, 0.25],
        ];
        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
            width: 3,
            height: 2,
            color_space: WorkingColorSpace::LinearRec709,
            data,
        });
        for (
            input_black,
            input_white,
            gamma,
            output_black,
            output_white,
            channel,
            clip_black,
            clip_white,
        ) in [
            // Identity, still appended so the fused kind runs end to end.
            (
                0.0_f32,
                1.0_f32,
                1.0_f32,
                0.0_f32,
                1.0_f32,
                LevelsChannel::Rgb,
                LevelsClip::Off,
                LevelsClip::Off,
            ),
            // Strong contrast push.
            (
                0.2,
                0.85,
                1.0,
                0.05,
                0.95,
                LevelsChannel::Rgb,
                LevelsClip::Off,
                LevelsClip::Off,
            ),
            // Gamma only.
            (
                0.0,
                1.0,
                2.2,
                0.0,
                1.0,
                LevelsChannel::Rgb,
                LevelsClip::Off,
                LevelsClip::Off,
            ),
            // Alpha: authors coverage, including a pixel that arrives empty.
            (
                0.0,
                1.0,
                1.0,
                0.25,
                1.0,
                LevelsChannel::Alpha,
                LevelsClip::Off,
                LevelsClip::Off,
            ),
            // Display clipping: the compressed input range pushes 2.5 and -0.4
            // far outside [0, 1] before the clip.
            (
                0.25,
                0.75,
                1.0,
                0.0,
                1.0,
                LevelsChannel::Rgb,
                LevelsClip::ClampDisplay,
                LevelsClip::ClampDisplay,
            ),
        ] {
            let grade = LevelsGrade::new(
                input_black,
                input_white,
                gamma,
                output_black,
                output_white,
                channel,
                clip_black,
                clip_white,
            )
            .expect("valid levels grade");
            let mut builder = EffectGraphBuilderState::new();
            builder.append_unary(EffectRenderOp::Levels { grade });
            let graph = compile_reference_render_graph(builder.finish()).expect("valid graph");
            let plan = lower_effect_graph_to_gpu_plan(&graph).expect("supported point effect");
            assert_eq!(plan.operations().len(), 1);
            let expected = apply_compiled_effect_graph_rgba_f32(
                mondrian_effects::EffectFloatFrameView::new(
                    mondrian_effects::EffectFrameExtent::new(3, 2),
                    mondrian_effects::EffectValueFormat::new(
                        mondrian_effects::EffectWorkingPrecision::Float32,
                        mondrian_effects::EffectColorDomain::SceneLinearRgb,
                        mondrian_core::ColorFrameAlpha::StraightCoverage,
                    ),
                    mondrian_core::ImageSampling::UNIT_SQUARE,
                    &frame.rgba_f32().data,
                ),
                &graph,
                0,
            )
            .expect("CPU float reference");
            let layer = GpuCompositeLayer {
                source: GpuCompositeLayerSource::CpuFrame(&frame),
                opacity: 1.0,
                blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_plan: Some(&plan),
                frame_seed: 0,
            };
            let (actual, diagnostics) = readback_test_composite_with_diagnostics(
                &context,
                &[layer],
                3,
                2,
                WorkingColorSpace::LinearRec709,
            );
            assert_eq!(diagnostics.execution.fused_point_operations, 1);
            let report = compare_linear_rgba(
                expected.pixels(),
                &actual,
                LinearRgbaAccuracyBudget {
                    rgb: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
                    alpha: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
                },
            )
            .expect("comparable frames");
            assert!(
                report.within_budget,
                "GPU levels accuracy budget exceeded for {grade:?}: {report:#?}"
            );
        }
    }

    #[tokio::test]
    async fn gpu_ramp_matches_the_cpu_kernel() {
        use crate::color_accuracy::{
            compare_linear_rgba, LinearAccuracyBudget, LinearRgbaAccuracyBudget,
        };
        use mondrian_effects::{
            apply_compiled_effect_graph_rgba_f32, compile_reference_render_graph,
            lower_effect_graph_to_gpu_plan, EffectGraphBuilderState, EffectRenderOp, RampGrade,
            RampShape,
        };

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping GPU ramp parity test: no GPU adapter available");
            return;
        };
        // 5x4: one HDR colour, one fully transparent pixel and one half-covered
        // pixel exercise the unclamped gradient, the zero-coverage guard and the
        // straight-alpha blend.
        let mut data = vec![[0.2_f32, 0.4, 0.6, 1.0]; 20];
        data[2] = [2.5, -0.4, 0.9, 1.0];
        data[3] = [0.0, 0.0, 0.0, 0.0];
        data[7] = [0.35, 0.35, 0.35, 0.5];
        data[13] = [0.9, 0.1, 0.3, 1.0];
        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
            width: 5,
            height: 4,
            color_space: WorkingColorSpace::LinearRec709,
            data,
        });
        for (start, start_color, end, end_color, shape, scatter, blend) in [
            // The authoring defaults: black at row 0 to white at row 256.
            (
                [128.0_f32, 0.0_f32],
                [0.0_f32, 0.0, 0.0],
                [128.0, 256.0],
                [1.0, 1.0, 1.0],
                RampShape::Linear,
                0.0_f32,
                0.0_f32,
            ),
            // Scattered radial ramp along the diagonal.
            (
                [0.5, 0.5],
                [1.0, 0.2, 0.0],
                [4.5, 3.5],
                [0.0, 0.4, 1.0],
                RampShape::Radial,
                120.0,
                0.0,
            ),
            // Partial blend over the source.
            (
                [0.0, 3.0],
                [0.9, 0.9, 0.9],
                [4.0, 0.0],
                [0.1, 0.0, 0.3],
                RampShape::Linear,
                0.0,
                0.35,
            ),
        ] {
            let grade = RampGrade::new(start, start_color, end, end_color, shape, scatter, blend)
                .expect("valid ramp grade");
            let mut builder = EffectGraphBuilderState::new();
            builder.append_unary(EffectRenderOp::Ramp { grade });
            let graph = compile_reference_render_graph(builder.finish()).expect("valid graph");
            let plan = lower_effect_graph_to_gpu_plan(&graph).expect("supported point effect");
            assert_eq!(plan.operations().len(), 1);
            let expected = apply_compiled_effect_graph_rgba_f32(
                mondrian_effects::EffectFloatFrameView::new(
                    mondrian_effects::EffectFrameExtent::new(5, 4),
                    mondrian_effects::EffectValueFormat::new(
                        mondrian_effects::EffectWorkingPrecision::Float32,
                        mondrian_effects::EffectColorDomain::SceneLinearRgb,
                        mondrian_core::ColorFrameAlpha::StraightCoverage,
                    ),
                    mondrian_core::ImageSampling::UNIT_SQUARE,
                    &frame.rgba_f32().data,
                ),
                &graph,
                0,
            )
            .expect("CPU float reference");
            let layer = GpuCompositeLayer {
                source: GpuCompositeLayerSource::CpuFrame(&frame),
                opacity: 1.0,
                blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_plan: Some(&plan),
                frame_seed: 0,
            };
            let (actual, diagnostics) = readback_test_composite_with_diagnostics(
                &context,
                &[layer],
                5,
                4,
                WorkingColorSpace::LinearRec709,
            );
            assert_eq!(diagnostics.execution.fused_point_operations, 1);
            let report = compare_linear_rgba(
                expected.pixels(),
                &actual,
                LinearRgbaAccuracyBudget {
                    rgb: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
                    alpha: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
                },
            )
            .expect("comparable frames");
            assert!(
                report.within_budget,
                "GPU ramp accuracy budget exceeded for {grade:?}: {report:#?}"
            );
        }
    }

    #[tokio::test]
    async fn gpu_arithmetic_matches_the_cpu_kernel() {
        use crate::color_accuracy::{
            compare_linear_rgba, LinearAccuracyBudget, LinearRgbaAccuracyBudget,
        };
        use mondrian_effects::{
            apply_compiled_effect_graph_rgba_f32, compile_reference_render_graph,
            lower_effect_graph_to_gpu_plan, ArithmeticGrade, ArithmeticOperator,
            EffectGraphBuilderState, EffectRenderOp,
        };

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping GPU arithmetic parity test: no GPU adapter available");
            return;
        };
        // 5x4: one HDR colour, one fully transparent pixel and one half-covered
        // pixel exercise the unclamped operators, the zero-coverage guard and the
        // passthrough alpha. The sample values include exact eight-bit readings
        // and values that only match after the documented rounding.
        let mut data = vec![[0.2_f32, 0.4, 0.6, 1.0]; 20];
        data[0] = [0.0, 0.0, 0.0, 1.0];
        data[1] = [1.0, 0.5, 0.25, 1.0];
        data[2] = [2.5, -0.4, 0.9, 1.0];
        data[3] = [0.0, 0.0, 0.0, 0.0];
        data[7] = [0.35, 0.35, 0.35, 0.5];
        data[13] = [0.9, 0.1, 0.3, 1.0];
        data[18] = [0.55, 0.15, 0.7, 1.0];
        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
            width: 5,
            height: 4,
            color_space: WorkingColorSpace::LinearRec709,
            data,
        });
        for (operator, red, green, blue, clipping) in [
            // The authoring defaults: `and` with three zero values, clamped.
            (ArithmeticOperator::And, 0.0_f32, 0.0_f32, 0.0_f32, true),
            // A bitwise `or` against three eight-bit readings.
            (ArithmeticOperator::Or, 85.0, 170.0, 51.0, true),
            // `add` over a mid grey lands above one before the clamp.
            (ArithmeticOperator::Add, 128.0, 64.0, 200.0, true),
            // `slice` thresholds every channel against its own value.
            (ArithmeticOperator::Slice, 40.0, 180.0, 90.0, true),
            // `screen` without the clamp keeps the HDR excursions.
            (ArithmeticOperator::Screen, 200.0, 130.0, 60.0, false),
        ] {
            let grade = ArithmeticGrade::new(operator, red, green, blue, clipping)
                .expect("valid arithmetic grade");
            let mut builder = EffectGraphBuilderState::new();
            builder.append_unary(EffectRenderOp::Arithmetic { grade });
            let graph = compile_reference_render_graph(builder.finish()).expect("valid graph");
            let plan = lower_effect_graph_to_gpu_plan(&graph).expect("supported point effect");
            assert_eq!(plan.operations().len(), 1);
            let expected = apply_compiled_effect_graph_rgba_f32(
                mondrian_effects::EffectFloatFrameView::new(
                    mondrian_effects::EffectFrameExtent::new(5, 4),
                    mondrian_effects::EffectValueFormat::new(
                        mondrian_effects::EffectWorkingPrecision::Float32,
                        mondrian_effects::EffectColorDomain::SceneLinearRgb,
                        mondrian_core::ColorFrameAlpha::StraightCoverage,
                    ),
                    mondrian_core::ImageSampling::UNIT_SQUARE,
                    &frame.rgba_f32().data,
                ),
                &graph,
                0,
            )
            .expect("CPU float reference");
            let layer = GpuCompositeLayer {
                source: GpuCompositeLayerSource::CpuFrame(&frame),
                opacity: 1.0,
                blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_plan: Some(&plan),
                frame_seed: 0,
            };
            let (actual, diagnostics) = readback_test_composite_with_diagnostics(
                &context,
                &[layer],
                5,
                4,
                WorkingColorSpace::LinearRec709,
            );
            assert_eq!(diagnostics.execution.fused_point_operations, 1);
            let report = compare_linear_rgba(
                expected.pixels(),
                &actual,
                LinearRgbaAccuracyBudget {
                    rgb: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
                    alpha: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
                },
            )
            .expect("comparable frames");
            assert!(
                report.within_budget,
                "GPU arithmetic accuracy budget exceeded for {grade:?}: {report:#?}"
            );
        }
    }

    #[tokio::test]
    async fn gpu_leave_color_matches_the_cpu_kernel() {
        use crate::color_accuracy::{
            compare_linear_rgba, LinearAccuracyBudget, LinearRgbaAccuracyBudget,
        };
        use mondrian_effects::{
            apply_compiled_effect_graph_rgba_f32, compile_reference_render_graph,
            lower_effect_graph_to_gpu_plan, EffectGraphBuilderState, EffectRenderOp,
            LeaveColorGrade, LeaveColorMatch,
        };

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping GPU leave color parity test: no GPU adapter available");
            return;
        };
        // 5x4: an exact match, a same-hue darker tint, a far colour, an HDR read,
        // a half-covered pixel and a fully transparent pixel exercise both match
        // criteria, the zero-coverage guard and the passthrough alpha. The
        // transparent pixel keeps a zero colour because compositing a zero-coverage
        // layer onto a transparent target discards its rgb, so only its alpha is
        // observable in the readback; the kernel's bit-identical skip is covered by
        // the unit tests.
        let mut data = vec![[0.2_f32, 0.4, 0.6, 1.0]; 20];
        data[0] = [1.0, 0.0, 0.0, 1.0];
        data[1] = [0.25, 0.0, 0.0, 1.0];
        data[2] = [0.0, 1.0, 0.0, 1.0];
        data[3] = [2.5, -0.4, 0.9, 1.0];
        data[5] = [0.5, 0.5, 0.5, 0.5];
        data[7] = [0.0, 0.0, 0.0, 0.0];
        data[9] = [0.2, 0.4, 0.6, 1.0];
        data[11] = [0.7, 0.15, 0.3, 1.0];
        data[13] = [0.05, 0.9, 0.5, 0.75];
        data[17] = [-0.3, 0.2, 1.4, 1.0];
        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
            width: 5,
            height: 4,
            color_space: WorkingColorSpace::LinearRec709,
            data,
        });
        for (match_mode, amount, colour, tolerance, softness) in [
            // The full decolour of everything that is not the kept red.
            (
                LeaveColorMatch::Rgb,
                100.0_f32,
                [1.0_f32, 0.0, 0.0],
                20.0_f32,
                0.0_f32,
            ),
            // The hue criterion with a wider soft edge.
            (LeaveColorMatch::Hue, 80.0, [0.2, 0.4, 0.6], 10.0, 30.0),
            // A partial amount leaves the far colours half saturated.
            (LeaveColorMatch::Rgb, 50.0, [0.2, 0.4, 0.6], 15.0, 0.0),
            // A zero tolerance only keeps the exact kept colour.
            (LeaveColorMatch::Rgb, 100.0, [0.5, 0.5, 0.5], 0.0, 0.0),
        ] {
            let grade = LeaveColorGrade::new(amount, colour, tolerance, softness, match_mode)
                .expect("valid leave color grade");
            let mut builder = EffectGraphBuilderState::new();
            builder.append_unary(EffectRenderOp::LeaveColor { grade });
            let graph = compile_reference_render_graph(builder.finish()).expect("valid graph");
            let plan = lower_effect_graph_to_gpu_plan(&graph).expect("supported point effect");
            assert_eq!(plan.operations().len(), 1);
            let expected = apply_compiled_effect_graph_rgba_f32(
                mondrian_effects::EffectFloatFrameView::new(
                    mondrian_effects::EffectFrameExtent::new(5, 4),
                    mondrian_effects::EffectValueFormat::new(
                        mondrian_effects::EffectWorkingPrecision::Float32,
                        mondrian_effects::EffectColorDomain::SceneLinearRgb,
                        mondrian_core::ColorFrameAlpha::StraightCoverage,
                    ),
                    mondrian_core::ImageSampling::UNIT_SQUARE,
                    &frame.rgba_f32().data,
                ),
                &graph,
                0,
            )
            .expect("CPU float reference");
            let layer = GpuCompositeLayer {
                source: GpuCompositeLayerSource::CpuFrame(&frame),
                opacity: 1.0,
                blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_plan: Some(&plan),
                frame_seed: 0,
            };
            let (actual, diagnostics) = readback_test_composite_with_diagnostics(
                &context,
                &[layer],
                5,
                4,
                WorkingColorSpace::LinearRec709,
            );
            assert_eq!(diagnostics.execution.fused_point_operations, 1);
            let report = compare_linear_rgba(
                expected.pixels(),
                &actual,
                LinearRgbaAccuracyBudget {
                    rgb: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
                    alpha: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
                },
            )
            .expect("comparable frames");
            assert!(
                report.within_budget,
                "GPU leave color accuracy budget exceeded for {grade:?}: {report:#?}"
            );
        }
    }

    #[test]
    fn change_to_color_uniforms_pack_the_authored_slots() {
        use mondrian_effects::{
            compile_reference_render_graph, lower_effect_graph_to_gpu_plan, ChangeToColorBy,
            ChangeToColorChange, ChangeToColorGrade, EffectGraphBuilderState, EffectRenderOp,
        };

        // The documented frame-1 configuration: red source, green target, hue
        // only, relative change-by, hard edge, no matte view.
        let grade = ChangeToColorGrade::new(
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            ChangeToColorChange::Hue,
            ChangeToColorBy::Relative,
            1.0,
            1.0,
            1.0,
            0.0,
            0.0,
        )
        .expect("valid change to color grade");
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(EffectRenderOp::ChangeToColor { grade });
        let graph = compile_reference_render_graph(builder.finish()).expect("valid graph");
        let plan = lower_effect_graph_to_gpu_plan(&graph).expect("supported point effect");
        let uniforms = effect_uniforms(Some(&plan), None).expect("uniforms");
        assert_eq!(uniforms[0].header, [34, 1, 0, 0]);
        assert_eq!(uniforms[0].params, [1.0, 1.0, 1.0, 0.0]);
        assert_eq!(uniforms[0].color, [0.0, 1.0, 0.0, 0.0]);
        assert_eq!(uniforms[0].extra0, [1.0, 0.0, 0.0, 0.0]);
        assert_eq!(uniforms[0].extra1, [0.0, 0.0, 0.0, 0.0]);
    }

    #[tokio::test]
    async fn gpu_change_to_color_matches_the_cpu_kernel() {
        use crate::color_accuracy::{
            compare_linear_rgba, LinearAccuracyBudget, LinearRgbaAccuracyBudget,
        };
        use mondrian_effects::{
            apply_compiled_effect_graph_rgba_f32, compile_reference_render_graph,
            lower_effect_graph_to_gpu_plan, ChangeToColorBy, ChangeToColorChange,
            ChangeToColorGrade, EffectGraphBuilderState, EffectRenderOp,
        };

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping GPU change to color parity test: no GPU adapter available");
            return;
        };
        // One authored configuration per case. The first four pixels carry the
        // documented reading; every other pixel keeps `[0.2, 0.4, 0.6, 1.0]`,
        // which is far enough from the red source (`d = 0.79`) to take a zero
        // weight in all three cases.
        struct Case {
            from: [f32; 3],
            to: [f32; 3],
            change: ChangeToColorChange,
            by: ChangeToColorBy,
            amounts: [f32; 3],
            softness: f32,
            view: f32,
            authored: [[f32; 4]; 4],
            expected: [[f32; 4]; 4],
        }
        let cases = [
            // An exact match is replaced by the target hue; the target colour
            // itself, a grey and the unfilled pixels keep their value.
            Case {
                from: [1.0, 0.0, 0.0],
                to: [0.0, 1.0, 0.0],
                change: ChangeToColorChange::Hue,
                by: ChangeToColorBy::Relative,
                amounts: [1.0, 1.0, 1.0],
                softness: 0.0,
                view: 0.0,
                authored: [
                    [1.0, 0.0, 0.0, 1.0],
                    [0.0, 1.0, 0.0, 1.0],
                    [0.0, 0.0, 0.0, 0.0],
                    [0.5, 0.5, 0.5, 1.0],
                ],
                expected: [
                    [0.0, 1.0, 0.0, 1.0],
                    [0.0, 1.0, 0.0, 1.0],
                    [0.0, 0.0, 0.0, 0.0],
                    [0.5, 0.5, 0.5, 1.0],
                ],
            },
            // Absolute change-by moves every matched sample to the target HSV:
            // `(0.5, 0, 0)` is `(0, 1, 0.5)` in HSV, and the absolute deltas
            // `(1/12 - 0, 1 - 1, 1 - 0.5)` land on `(1, 0.5, 0)`.
            Case {
                from: [1.0, 0.0, 0.0],
                to: [1.0, 0.5, 0.0],
                change: ChangeToColorChange::HueLightnessSaturation,
                by: ChangeToColorBy::Absolute,
                amounts: [1.0, 1.0, 1.0],
                softness: 0.0,
                view: 0.0,
                authored: [
                    [1.0, 0.0, 0.0, 1.0],
                    [0.5, 0.0, 0.0, 1.0],
                    [0.0, 1.0, 1.0, 1.0],
                    [0.0, 0.0, 0.0, 0.0],
                ],
                expected: [
                    [1.0, 0.5, 0.0, 1.0],
                    [1.0, 0.5, 0.0, 1.0],
                    [0.0, 1.0, 1.0, 1.0],
                    [0.0, 0.0, 0.0, 0.0],
                ],
            },
            // The matte view replaces the corrected colour by the weight, and a
            // zero weight is black rather than a skipped pixel: `d = 0.25` with
            // softness 0.5 halves the ramp, and the cyan sample is black.
            Case {
                from: [1.0, 0.0, 0.0],
                to: [0.0, 1.0, 0.0],
                change: ChangeToColorChange::Hue,
                by: ChangeToColorBy::Relative,
                amounts: [1.0, 1.0, 1.0],
                softness: 0.5,
                view: 1.0,
                authored: [
                    [1.0, 0.0, 0.0, 1.0],
                    [1.0, 0.433013, 0.0, 1.0],
                    [0.0, 1.0, 1.0, 1.0],
                    [0.0, 0.0, 0.0, 0.0],
                ],
                expected: [
                    [1.0, 1.0, 1.0, 1.0],
                    [0.5001, 0.5001, 0.5001, 1.0],
                    [0.0, 0.0, 0.0, 1.0],
                    [0.0, 0.0, 0.0, 0.0],
                ],
            },
        ];
        for case in cases {
            let mut data = vec![[0.2_f32, 0.4, 0.6, 1.0]; 20];
            data[..4].copy_from_slice(&case.authored);
            let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
                sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
                alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
                width: 5,
                height: 4,
                color_space: WorkingColorSpace::LinearRec709,
                data,
            });
            let grade = ChangeToColorGrade::new(
                case.from,
                case.to,
                case.change,
                case.by,
                case.amounts[0],
                case.amounts[1],
                case.amounts[2],
                case.softness,
                case.view,
            )
            .expect("valid change to color grade");
            let mut builder = EffectGraphBuilderState::new();
            builder.append_unary(EffectRenderOp::ChangeToColor { grade });
            let graph = compile_reference_render_graph(builder.finish()).expect("valid graph");
            let plan = lower_effect_graph_to_gpu_plan(&graph).expect("supported point effect");
            assert_eq!(plan.operations().len(), 1);
            let expected = apply_compiled_effect_graph_rgba_f32(
                mondrian_effects::EffectFloatFrameView::new(
                    mondrian_effects::EffectFrameExtent::new(5, 4),
                    mondrian_effects::EffectValueFormat::new(
                        mondrian_effects::EffectWorkingPrecision::Float32,
                        mondrian_effects::EffectColorDomain::SceneLinearRgb,
                        mondrian_core::ColorFrameAlpha::StraightCoverage,
                    ),
                    mondrian_core::ImageSampling::UNIT_SQUARE,
                    &frame.rgba_f32().data,
                ),
                &graph,
                0,
            )
            .expect("CPU float reference");
            for (index, want) in case.expected.iter().enumerate() {
                let got = expected.pixels()[index];
                for channel in 0..4 {
                    // The compositor discards the rgb of a zero-coverage layer,
                    // so the reference itself must already read all zero there.
                    assert!(
                        (got[channel] - want[channel]).abs() <= 1.0e-3,
                        "reference pixel {index} channel {channel}: got {got:?}, want {want:?}"
                    );
                }
            }
            let layer = GpuCompositeLayer {
                source: GpuCompositeLayerSource::CpuFrame(&frame),
                opacity: 1.0,
                blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_plan: Some(&plan),
                frame_seed: 0,
            };
            let (actual, diagnostics) = readback_test_composite_with_diagnostics(
                &context,
                &[layer],
                5,
                4,
                WorkingColorSpace::LinearRec709,
            );
            assert_eq!(diagnostics.execution.fused_point_operations, 1);
            let report = compare_linear_rgba(
                expected.pixels(),
                &actual,
                LinearRgbaAccuracyBudget {
                    rgb: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
                    alpha: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
                },
            )
            .expect("comparable frames");
            assert!(
                report.within_budget,
                "GPU change to color accuracy budget exceeded for {grade:?}: {report:#?}"
            );
        }
    }

    #[tokio::test]
    async fn gpu_broadcast_safe_matches_the_cpu_kernel() {
        use crate::color_accuracy::{
            compare_linear_rgba, LinearAccuracyBudget, LinearRgbaAccuracyBudget,
        };
        use mondrian_effects::{
            apply_compiled_effect_graph_rgba_f32, compile_reference_render_graph,
            lower_effect_graph_to_gpu_plan, BroadcastLocale, BroadcastSafeGrade, BroadcastSafeMode,
            EffectGraphBuilderState, EffectRenderOp,
        };

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping GPU broadcast safe parity test: no GPU adapter available");
            return;
        };
        // 5x4: a bright primary, a saturated green, an HDR read with a negative
        // channel, a half-covered pixel, a fully transparent pixel and the flat
        // default. The transparent pixel keeps a zero colour because compositing a
        // zero-coverage layer onto a transparent target discards its rgb, so the
        // composite only observes its alpha; the kernel's bit-identical skip is
        // covered by the unit tests.
        let mut data = vec![[0.2_f32, 0.4, 0.6, 1.0]; 20];
        data[0] = [1.0, 0.0, 0.0, 1.0];
        data[2] = [0.0, 1.0, 0.0, 1.0];
        data[3] = [2.5, -0.4, 0.9, 1.0];
        data[5] = [0.5, 0.5, 0.5, 0.5];
        data[7] = [0.0, 0.0, 0.0, 0.0];
        data[9] = [1.5, 1.5, 1.5, 1.0];
        data[11] = [0.7, 0.15, 0.3, 1.0];
        data[13] = [0.05, 0.9, 0.5, 0.75];
        data[17] = [-0.3, 0.2, 1.4, 1.0];
        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
            width: 5,
            height: 4,
            color_space: WorkingColorSpace::LinearRec709,
            data,
        });
        for (locale, mode, amplitude) in [
            // The luminance reduction under the NTSC setup: the HDR read keeps a
            // luma under the limit even though its brightest channel is not.
            (
                BroadcastLocale::Ntsc,
                BroadcastSafeMode::ReduceLuminance,
                100.0_f32,
            ),
            // The saturation reduction under the PAL mapping.
            (
                BroadcastLocale::Pal,
                BroadcastSafeMode::ReduceSaturation,
                110.0,
            ),
            // Keying the unsafe samples out of the NTSC frame.
            (
                BroadcastLocale::Ntsc,
                BroadcastSafeMode::KeyOutUnsafe,
                110.0,
            ),
            // The hard per-channel ceiling under the PAL mapping.
            (BroadcastLocale::Pal, BroadcastSafeMode::ClampRgb, 120.0),
            // Keying the safe samples out of the NTSC frame.
            (BroadcastLocale::Ntsc, BroadcastSafeMode::KeyOutSafe, 105.0),
        ] {
            let grade = BroadcastSafeGrade::new(locale, mode, amplitude)
                .expect("valid broadcast safe grade");
            let mut builder = EffectGraphBuilderState::new();
            builder.append_unary(EffectRenderOp::BroadcastSafe { grade });
            let graph = compile_reference_render_graph(builder.finish()).expect("valid graph");
            let plan = lower_effect_graph_to_gpu_plan(&graph).expect("supported point effect");
            assert_eq!(plan.operations().len(), 1);
            let expected = apply_compiled_effect_graph_rgba_f32(
                mondrian_effects::EffectFloatFrameView::new(
                    mondrian_effects::EffectFrameExtent::new(5, 4),
                    mondrian_effects::EffectValueFormat::new(
                        mondrian_effects::EffectWorkingPrecision::Float32,
                        mondrian_effects::EffectColorDomain::SceneLinearRgb,
                        mondrian_core::ColorFrameAlpha::StraightCoverage,
                    ),
                    mondrian_core::ImageSampling::UNIT_SQUARE,
                    &frame.rgba_f32().data,
                ),
                &graph,
                0,
            )
            .expect("CPU float reference");
            // The two keying modes lower alpha to zero and leave the rgb in the
            // kernel, but compositing a zero-coverage pixel onto the transparent
            // target discards that rgb before the readback.
            let expected_pixels: Vec<[f32; 4]> = expected
                .pixels()
                .iter()
                .map(|pixel| if pixel[3] <= 0.0 { [0.0; 4] } else { *pixel })
                .collect();
            let layer = GpuCompositeLayer {
                source: GpuCompositeLayerSource::CpuFrame(&frame),
                opacity: 1.0,
                blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_plan: Some(&plan),
                frame_seed: 0,
            };
            let (actual, diagnostics) = readback_test_composite_with_diagnostics(
                &context,
                &[layer],
                5,
                4,
                WorkingColorSpace::LinearRec709,
            );
            assert_eq!(diagnostics.execution.fused_point_operations, 1);
            let report = compare_linear_rgba(
                &expected_pixels,
                &actual,
                LinearRgbaAccuracyBudget {
                    rgb: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
                    alpha: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
                },
            )
            .expect("comparable frames");
            assert!(
                report.within_budget,
                "GPU broadcast safe accuracy budget exceeded for {grade:?}: {report:#?}"
            );
        }
    }

    #[tokio::test]
    async fn gpu_hdr_tone_mapping_matches_the_cpu_kernel() {
        use crate::color_accuracy::{
            compare_linear_rgba, LinearAccuracyBudget, LinearRgbaAccuracyBudget,
        };
        use mondrian_effects::{
            apply_compiled_effect_graph_rgba_f32, compile_reference_render_graph,
            lower_effect_graph_to_gpu_plan, EffectGraphBuilderState, EffectRenderOp,
            HdrToneMappingGrade, ToneMapMode,
        };

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping GPU hdr tone mapping parity test: no GPU adapter available");
            return;
        };
        // A 64x64 gradient in scene-linear light: red sweeps from a negative
        // channel up to 8.5, green from zero to 12 (past the 11.2 white point) and
        // blue over a 2D cross term. Four alpha levels ride the pixel index so
        // every row sees full, partial and zero coverage. The transparent pixels
        // keep a zero colour because compositing a zero-coverage layer onto a
        // transparent target discards their rgb, so the composite only observes
        // their alpha; the kernel's bit-identical skip is covered by the unit
        // tests.
        let width = 64_u32;
        let height = 64_u32;
        let mut data = Vec::with_capacity((width * height) as usize);
        for y in 0..height {
            for x in 0..width {
                let u = x as f32 / (width - 1) as f32;
                let v = y as f32 / (height - 1) as f32;
                let alpha = match (y * width + x) % 4 {
                    0 => 0.0,
                    1 => 0.25,
                    2 => 0.75,
                    _ => 1.0,
                };
                data.push([9.0 * u - 0.5, 12.0 * v, 0.05 + 4.0 * u * v, alpha]);
            }
        }
        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
            width,
            height,
            color_space: WorkingColorSpace::LinearRec709,
            data,
        });
        for (mode, gain, gamma) in [
            // The global compression with a display encoding on top.
            (ToneMapMode::Reinhard, 2.0_f32, 2.2_f32),
            // The filmic curve with its white point pinned to one.
            (ToneMapMode::Hable, 4.0, 0.5),
        ] {
            let grade =
                HdrToneMappingGrade::new(mode, gain, gamma).expect("valid hdr tone mapping grade");
            let mut builder = EffectGraphBuilderState::new();
            builder.append_unary(EffectRenderOp::HdrToneMapping { grade });
            let graph = compile_reference_render_graph(builder.finish()).expect("valid graph");
            let plan = lower_effect_graph_to_gpu_plan(&graph).expect("supported point effect");
            assert_eq!(plan.operations().len(), 1);
            let expected = apply_compiled_effect_graph_rgba_f32(
                mondrian_effects::EffectFloatFrameView::new(
                    mondrian_effects::EffectFrameExtent::new(width, height),
                    mondrian_effects::EffectValueFormat::new(
                        mondrian_effects::EffectWorkingPrecision::Float32,
                        mondrian_effects::EffectColorDomain::SceneLinearRgb,
                        mondrian_core::ColorFrameAlpha::StraightCoverage,
                    ),
                    mondrian_core::ImageSampling::UNIT_SQUARE,
                    &frame.rgba_f32().data,
                ),
                &graph,
                0,
            )
            .expect("CPU float reference");
            let expected_pixels: Vec<[f32; 4]> = expected
                .pixels()
                .iter()
                .map(|pixel| if pixel[3] <= 0.0 { [0.0; 4] } else { *pixel })
                .collect();
            let layer = GpuCompositeLayer {
                source: GpuCompositeLayerSource::CpuFrame(&frame),
                opacity: 1.0,
                blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_plan: Some(&plan),
                frame_seed: 0,
            };
            let (actual, diagnostics) = readback_test_composite_with_diagnostics(
                &context,
                &[layer],
                width,
                height,
                WorkingColorSpace::LinearRec709,
            );
            assert_eq!(diagnostics.execution.fused_point_operations, 1);
            let report = compare_linear_rgba(
                &expected_pixels,
                &actual,
                LinearRgbaAccuracyBudget {
                    rgb: LinearAccuracyBudget::finite(1.0e-6, 1.0e-6, 1.0e-6),
                    alpha: LinearAccuracyBudget::finite(1.0e-6, 1.0e-6, 1.0e-6),
                },
            )
            .expect("comparable frames");
            assert!(
                report.within_budget,
                "GPU hdr tone mapping accuracy budget exceeded for {grade:?}: {report:#?}"
            );
        }
    }

    /// P1: the fused point kernel mirrors the CPU formula. Five alpha levels
    /// cross two backgrounds (black and white) with the clip on and off, so the
    /// unmix term, the negative-channel case and the single final clamp are all
    /// exercised against the reference frame.
    #[tokio::test]
    async fn gpu_remove_color_matting_matches_the_cpu_kernel() {
        use crate::color_accuracy::{
            compare_linear_rgba, LinearAccuracyBudget, LinearRgbaAccuracyBudget,
        };
        use mondrian_effects::{
            apply_compiled_effect_graph_rgba_f32, compile_reference_render_graph,
            lower_effect_graph_to_gpu_plan, EffectGraphBuilderState, EffectRenderOp,
            RemoveColorMattingGrade,
        };

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping GPU remove color matting parity test: no GPU adapter available");
            return;
        };
        // Five alpha levels ride the column index so every row sees zero, tiny,
        // half, three-quarter and full coverage. The colour stays in range so
        // the unclipped white matte produces the large negative channels the
        // formula promises.
        let alphas = [0.0_f32, 0.25, 0.5, 0.75, 1.0];
        let width = 64_u32;
        let height = 8_u32;
        let mut data = Vec::with_capacity((width * height) as usize);
        for y in 0..height {
            for x in 0..width {
                let u = x as f32 / (width - 1) as f32;
                let v = y as f32 / (height - 1) as f32;
                let alpha = alphas[(x % alphas.len() as u32) as usize];
                data.push([0.15 + 0.7 * u, 0.2 + 0.6 * v, 0.9 - 0.5 * u * v, alpha]);
            }
        }
        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
            width,
            height,
            color_space: WorkingColorSpace::LinearRec709,
            data,
        });
        for background in [[0.0_f32; 4], [1.0, 1.0, 1.0, 1.0]] {
            for clip in [true, false] {
                let grade = RemoveColorMattingGrade::new(background, clip)
                    .expect("valid remove color matting grade");
                let mut builder = EffectGraphBuilderState::new();
                builder.append_unary(EffectRenderOp::RemoveColorMatting { grade });
                let graph = compile_reference_render_graph(builder.finish()).expect("valid graph");
                let plan = lower_effect_graph_to_gpu_plan(&graph).expect("supported point effect");
                assert_eq!(plan.operations().len(), 1);
                let expected = apply_compiled_effect_graph_rgba_f32(
                    mondrian_effects::EffectFloatFrameView::new(
                        mondrian_effects::EffectFrameExtent::new(width, height),
                        mondrian_effects::EffectValueFormat::new(
                            mondrian_effects::EffectWorkingPrecision::Float32,
                            mondrian_effects::EffectColorDomain::SceneLinearRgb,
                            mondrian_core::ColorFrameAlpha::StraightCoverage,
                        ),
                        mondrian_core::ImageSampling::UNIT_SQUARE,
                        &frame.rgba_f32().data,
                    ),
                    &graph,
                    0,
                )
                .expect("CPU float reference");
                // Compositing a zero-coverage layer onto a transparent target
                // discards its rgb, so the composite only observes the alpha of
                // those pixels; the kernel's bit-identical skip is covered by the
                // unit tests.
                let expected_pixels: Vec<[f32; 4]> = expected
                    .pixels()
                    .iter()
                    .map(|pixel| if pixel[3] <= 0.0 { [0.0; 4] } else { *pixel })
                    .collect();
                let layer = GpuCompositeLayer {
                    source: GpuCompositeLayerSource::CpuFrame(&frame),
                    opacity: 1.0,
                    blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
                    transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                    effect_plan: Some(&plan),
                    frame_seed: 0,
                };
                let (actual, diagnostics) = readback_test_composite_with_diagnostics(
                    &context,
                    &[layer],
                    width,
                    height,
                    WorkingColorSpace::LinearRec709,
                );
                assert_eq!(diagnostics.execution.fused_point_operations, 1);
                let report = compare_linear_rgba(
                    &expected_pixels,
                    &actual,
                    LinearRgbaAccuracyBudget {
                        rgb: LinearAccuracyBudget::finite(1.0e-5, 1.0e-5, 1.0e-5),
                        alpha: LinearAccuracyBudget::finite(1.0e-5, 1.0e-5, 1.0e-5),
                    },
                )
                .expect("comparable frames");
                assert!(
                    report.within_budget,
                    "GPU remove color matting budget exceeded for background \
                     {background:?} clip {clip}: {report:#?}"
                );
            }
        }
    }

    /// P1: the fused point kernel mirrors the CPU formula. Three gels cross two
    /// densities and both preserve-luminosity states, and every seventh column
    /// has zero coverage so the untouched-pixel contract is exercised too.
    ///
    /// Without preserve-luminosity the two kernels are bit-identical on this
    /// frame (asserted, not budgeted). With it on, the only difference the GPU
    /// may introduce is the last-bit rounding of the reciprocal used for the
    /// luma rescale, so the budget is one part in a million.
    #[tokio::test]
    async fn gpu_photo_filter_matches_the_cpu_kernel() {
        use crate::color_accuracy::{
            compare_linear_rgba, LinearAccuracyBudget, LinearRgbaAccuracyBudget,
        };
        use mondrian_effects::{
            apply_compiled_effect_graph_rgba_f32, compile_reference_render_graph,
            lower_effect_graph_to_gpu_plan, EffectGraphBuilderState, EffectRenderOp,
            PhotoFilterGrade, PHOTO_FILTER_DEFAULT_COLOR,
        };

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping GPU photo filter parity test: no GPU adapter available");
            return;
        };
        // The samples stay inside [0, 1] so the composite target cannot clamp
        // them; out-of-range passthrough is covered by the unit tests. Every
        // seventh column is fully transparent.
        let width = 64_u32;
        let height = 8_u32;
        let mut data = Vec::with_capacity((width * height) as usize);
        for y in 0..height {
            for x in 0..width {
                let u = x as f32 / (width - 1) as f32;
                let v = y as f32 / (height - 1) as f32;
                let alpha = if x % 7 == 0 { 0.0 } else { 0.25 + 0.75 * v };
                data.push([0.05 + 0.9 * u, 0.1 + 0.8 * v, 0.9 - 0.85 * u * v, alpha]);
            }
        }
        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
            width,
            height,
            color_space: WorkingColorSpace::LinearRec709,
            data,
        });
        let cases = [
            (PHOTO_FILTER_DEFAULT_COLOR, 25.0_f32, false),
            (PHOTO_FILTER_DEFAULT_COLOR, 100.0, false),
            ([0.0, 0.28, 0.73], 60.0, false),
            (PHOTO_FILTER_DEFAULT_COLOR, 25.0, true),
            ([0.12, 0.7, 0.2], 80.0, true),
        ];
        for (color, density, preserve_luminosity) in cases {
            let grade = PhotoFilterGrade::new(color, density, preserve_luminosity)
                .expect("valid photo filter grade");
            let mut builder = EffectGraphBuilderState::new();
            builder.append_unary(EffectRenderOp::PhotoFilter { grade });
            let graph = compile_reference_render_graph(builder.finish()).expect("valid graph");
            let plan = lower_effect_graph_to_gpu_plan(&graph).expect("supported point effect");
            assert_eq!(plan.operations().len(), 1);
            let expected = apply_compiled_effect_graph_rgba_f32(
                mondrian_effects::EffectFloatFrameView::new(
                    mondrian_effects::EffectFrameExtent::new(width, height),
                    mondrian_effects::EffectValueFormat::new(
                        mondrian_effects::EffectWorkingPrecision::Float32,
                        mondrian_effects::EffectColorDomain::SceneLinearRgb,
                        mondrian_core::ColorFrameAlpha::StraightCoverage,
                    ),
                    mondrian_core::ImageSampling::UNIT_SQUARE,
                    &frame.rgba_f32().data,
                ),
                &graph,
                0,
            )
            .expect("CPU float reference");
            // Compositing a zero-coverage layer onto a transparent target
            // discards its rgb, so the composite only observes the alpha there;
            // the kernel's untouched-pixel skip is covered by the unit tests.
            let expected_pixels: Vec<[f32; 4]> = expected
                .pixels()
                .iter()
                .map(|pixel| if pixel[3] <= 0.0 { [0.0; 4] } else { *pixel })
                .collect();
            let layer = GpuCompositeLayer {
                source: GpuCompositeLayerSource::CpuFrame(&frame),
                opacity: 1.0,
                blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_plan: Some(&plan),
                frame_seed: 0,
            };
            let (actual, diagnostics) = readback_test_composite_with_diagnostics(
                &context,
                &[layer],
                width,
                height,
                WorkingColorSpace::LinearRec709,
            );
            assert_eq!(diagnostics.execution.fused_point_operations, 1);
            let mut max_abs = 0.0_f32;
            for (expected, actual) in expected_pixels.iter().zip(&actual) {
                for channel in 0..4 {
                    max_abs = max_abs.max((expected[channel] - actual[channel]).abs());
                }
            }
            if preserve_luminosity {
                let report = compare_linear_rgba(
                    &expected_pixels,
                    &actual,
                    LinearRgbaAccuracyBudget {
                        rgb: LinearAccuracyBudget::finite(1.0e-6, 1.0e-6, 1.0e-6),
                        alpha: LinearAccuracyBudget::finite(0.0, 0.0, 0.0),
                    },
                )
                .expect("comparable frames");
                eprintln!(
                    "photo filter preserve-luminosity max abs diff {max_abs:.9} \
                     (colour {color:?} density {density})"
                );
                assert!(
                    report.within_budget,
                    "GPU photo filter budget exceeded for colour {color:?} density {density}: \
                     max abs {max_abs:.9} {report:#?}"
                );
            } else {
                let mut mismatch = None;
                for (index, (expected, actual)) in expected_pixels.iter().zip(&actual).enumerate() {
                    if expected.map(f32::to_bits) != actual.map(f32::to_bits) {
                        mismatch = Some((index, *expected, *actual));
                        break;
                    }
                }
                assert!(
                    mismatch.is_none(),
                    "GPU photo filter is not bit-identical for colour {color:?} density \
                     {density}: max abs {max_abs:.9} first mismatch {mismatch:?}"
                );
            }
        }
    }

    /// FLM-06: the fragment shader and the CPU kernel must agree on the block
    /// grid, the phase-dependent attenuation, the edge/ring gain and the mosaic
    /// offset. The 7x5 frame covers the block origin, the first row and ring,
    /// both phase quadrants and alpha 0/0.5/1. D3D12 may lower `f32(bin) / 255.0`
    /// into a reciprocal multiply, so the budget is 1e-5 rather than bitwise.
    #[tokio::test]
    async fn gpu_jpeg_damage_matches_the_cpu_kernel() {
        use crate::color_accuracy::{
            compare_linear_rgba, LinearAccuracyBudget, LinearRgbaAccuracyBudget,
        };
        use mondrian_effects::{
            apply_compiled_effect_graph_rgba_f32, compile_reference_render_graph,
            lower_effect_graph_to_gpu_plan, EffectGraphBuilderState, EffectRenderOp,
            JpegDamageGrade, JpegDamageScalingComponent,
        };

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping GPU JPEG damage parity test: no GPU adapter available");
            return;
        };
        // The samples stay inside [0, 1] so the composite target cannot clamp
        // them; out-of-range passthrough is covered by the unit tests. Every
        // seventh column and every other row carry the graded alpha.
        let width = 7_u32;
        let height = 5_u32;
        let mut data = Vec::with_capacity((width * height) as usize);
        for y in 0..height {
            for x in 0..width {
                let alpha = if x % 7 == 0 {
                    0.0
                } else if y % 2 == 0 {
                    0.5
                } else {
                    1.0
                };
                let red = 0.30 + 0.04 * x as f32 + 0.01 * y as f32;
                let green = 0.35 + 0.03 * y as f32 + 0.02 * (x % 3) as f32;
                let blue = 0.40 + 0.05 * (x % 2) as f32 + 0.02 * y as f32;
                data.push([red, green, blue, alpha]);
            }
        }
        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
            width,
            height,
            color_space: WorkingColorSpace::LinearRec709,
            data,
        });
        let cases = [
            JpegDamageGrade::default(),
            JpegDamageGrade::new(50.0, 1.0, 1.0, 0.25, JpegDamageScalingComponent::Luma)
                .expect("valid JPEG damage grade"),
            JpegDamageGrade::new(0.0, 2.0, 2.0, 0.0, JpegDamageScalingComponent::Both)
                .expect("valid JPEG damage grade"),
        ];
        for grade in cases {
            let mut builder = EffectGraphBuilderState::new();
            builder.append_unary(EffectRenderOp::JpegDamage { grade });
            let graph = compile_reference_render_graph(builder.finish()).expect("valid graph");
            let plan = lower_effect_graph_to_gpu_plan(&graph).expect("supported point effect");
            assert_eq!(plan.operations().len(), 1);
            let expected = apply_compiled_effect_graph_rgba_f32(
                mondrian_effects::EffectFloatFrameView::new(
                    mondrian_effects::EffectFrameExtent::new(width, height),
                    mondrian_effects::EffectValueFormat::new(
                        mondrian_effects::EffectWorkingPrecision::Float32,
                        mondrian_effects::EffectColorDomain::SceneLinearRgb,
                        mondrian_core::ColorFrameAlpha::StraightCoverage,
                    ),
                    mondrian_core::ImageSampling::UNIT_SQUARE,
                    &frame.rgba_f32().data,
                ),
                &graph,
                0,
            )
            .expect("CPU float reference");
            // Compositing a zero-coverage layer onto a transparent target
            // discards its rgb, so the composite only observes the alpha there;
            // the kernel's untouched-pixel skip is covered by the unit tests.
            let expected_pixels: Vec<[f32; 4]> = expected
                .pixels()
                .iter()
                .map(|pixel| if pixel[3] <= 0.0 { [0.0; 4] } else { *pixel })
                .collect();
            let layer = GpuCompositeLayer {
                source: GpuCompositeLayerSource::CpuFrame(&frame),
                opacity: 1.0,
                blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_plan: Some(&plan),
                frame_seed: 0,
            };
            let (actual, diagnostics) = readback_test_composite_with_diagnostics(
                &context,
                &[layer],
                width,
                height,
                WorkingColorSpace::LinearRec709,
            );
            assert_eq!(diagnostics.execution.fused_point_operations, 1);
            let report = compare_linear_rgba(
                &expected_pixels,
                &actual,
                LinearRgbaAccuracyBudget {
                    rgb: LinearAccuracyBudget::finite(1.0e-5, 1.0e-5, 1.0e-5),
                    alpha: LinearAccuracyBudget::finite(1.0e-5, 1.0e-5, 1.0e-5),
                },
            )
            .expect("comparable frames");
            assert!(
                report.within_budget,
                "GPU JPEG damage budget exceeded for grade {grade:?}: {report:#?}"
            );
        }
    }

    /// COL-20: the WGSL branch and the CPU kernel must agree on the packed
    /// phase/mode lanes, the five-stop palette, the matching weight, the mask
    /// and the composite. The 64x8 frame spans several palette cycles and
    /// includes zero-coverage pixels, which both sides must leave untouched.
    #[tokio::test]
    async fn gpu_colorama_matches_the_cpu_kernel() {
        use crate::color_accuracy::{
            compare_linear_rgba, LinearAccuracyBudget, LinearRgbaAccuracyBudget,
        };
        use mondrian_effects::{
            apply_compiled_effect_graph_rgba_f32, compile_reference_render_graph,
            lower_effect_graph_to_gpu_plan, ColoramaGrade, ColoramaMatchMode, ColoramaModify,
            ColoramaPalette, ColoramaPhaseSource, ColoramaSettings, EffectGraphBuilderState,
            EffectRenderOp,
        };

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping GPU colour map parity test: no GPU adapter available");
            return;
        };
        let width = 64_u32;
        let height = 8_u32;
        let mut data = Vec::with_capacity((width * height) as usize);
        for y in 0..height {
            for x in 0..width {
                let red = 0.05 + 0.90 * (x as f32 + 0.5) / width as f32;
                let green = 0.10 + 0.80 * ((x * 3 + y * 5) % width) as f32 / width as f32;
                let blue = 0.15 + 0.70 * ((x * 7 + y * 11) % width) as f32 / width as f32;
                let alpha = if x % 9 == 0 { 0.0 } else { 0.65 };
                data.push([red, green, blue, alpha]);
            }
        }
        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
            width,
            height,
            color_space: WorkingColorSpace::LinearRec709,
            data,
        });
        // Two cases: the ordinary opaque palette, and `Alpha Ramp` with the
        // alpha switch on, which is the only palette whose samples carry a
        // per-stop alpha (`t`). The second case is what pins the shader's
        // palette-alpha lane; a plain multiplication would leave it at 1.
        let cases = [
            (
                "fire",
                ColoramaSettings {
                    get_phase_from: ColoramaPhaseSource::Red,
                    preset_palette: ColoramaPalette::FIRE,
                    cycle_repetitions: 3.0,
                    phase_shift: 47.0,
                    matching_mode: ColoramaMatchMode::Rgb,
                    matching_color: [0.45, 0.20, 0.15],
                    matching_tolerance: 0.35,
                    matching_softness: 0.0,
                    modify: ColoramaModify::All,
                    ..ColoramaSettings::default()
                },
            ),
            (
                "alpha ramp",
                ColoramaSettings {
                    get_phase_from: ColoramaPhaseSource::Red,
                    preset_palette: ColoramaPalette::ALPHA_RAMP,
                    cycle_repetitions: 2.0,
                    modify: ColoramaModify::All,
                    modify_alpha: 1.0,
                    ..ColoramaSettings::default()
                },
            ),
        ];
        for (name, settings) in cases {
            let grade = ColoramaGrade::new(settings).expect("valid colour map grade");
            let mut builder = EffectGraphBuilderState::new();
            builder.append_unary(EffectRenderOp::Colorama { grade });
            let graph = compile_reference_render_graph(builder.finish()).expect("valid graph");
            let plan = lower_effect_graph_to_gpu_plan(&graph).expect("supported point effect");
            assert_eq!(plan.operations().len(), 1);
            let expected = apply_compiled_effect_graph_rgba_f32(
                mondrian_effects::EffectFloatFrameView::new(
                    mondrian_effects::EffectFrameExtent::new(width, height),
                    mondrian_effects::EffectValueFormat::new(
                        mondrian_effects::EffectWorkingPrecision::Float32,
                        mondrian_effects::EffectColorDomain::SceneLinearRgb,
                        mondrian_core::ColorFrameAlpha::StraightCoverage,
                    ),
                    mondrian_core::ImageSampling::UNIT_SQUARE,
                    &frame.rgba_f32().data,
                ),
                &graph,
                0,
            )
            .expect("CPU float reference");
            let expected_pixels: Vec<[f32; 4]> = expected
                .pixels()
                .iter()
                .map(|pixel| if pixel[3] <= 0.0 { [0.0; 4] } else { *pixel })
                .collect();
            let layer = GpuCompositeLayer {
                source: GpuCompositeLayerSource::CpuFrame(&frame),
                opacity: 1.0,
                blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_plan: Some(&plan),
                frame_seed: 0,
            };
            let (actual, diagnostics) = readback_test_composite_with_diagnostics(
                &context,
                &[layer],
                width,
                height,
                WorkingColorSpace::LinearRec709,
            );
            assert_eq!(diagnostics.execution.fused_point_operations, 1);
            let report = compare_linear_rgba(
                &expected_pixels,
                &actual,
                LinearRgbaAccuracyBudget {
                    rgb: LinearAccuracyBudget::finite(2.0e-4, 2.0e-4, 2.0e-4),
                    alpha: LinearAccuracyBudget::finite(2.0e-4, 2.0e-4, 2.0e-4),
                },
            )
            .expect("comparable frames");
            assert!(
                report.within_budget,
                "GPU colour map budget exceeded for {name}: {report:#?}"
            );
        }
    }

    #[tokio::test]
    async fn gpu_test_signals_matches_the_cpu_kernel() {
        use crate::color_accuracy::{
            compare_linear_rgba, LinearAccuracyBudget, LinearRgbaAccuracyBudget,
        };
        use mondrian_effects::{
            apply_compiled_effect_graph_rgba_f32, compile_reference_render_graph,
            lower_effect_graph_to_gpu_plan, EffectGraphBuilderState, EffectRenderOp,
            TestSignalsGrade, TestSignalsPattern,
        };

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping GPU test signals parity test: no GPU adapter available");
            return;
        };
        // The cases avoid the bars pattern: its PLUGE strip is deliberately
        // negative (-0.04) and the composite target clamps a negative coverage
        // colour where the CPU reference keeps it. The remaining patterns are
        // integer-exact bar and cell selection, so the two kernels must agree.
        // Both extents are awkward multiples of the segment counts so the last
        // segment eats the remainder.
        for (width, height) in [(12_u32, 9_u32), (7, 5)] {
            let data = vec![[0.5_f32, 0.5, 0.5, 1.0]; (width * height) as usize];
            let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
                sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
                alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
                width,
                height,
                color_space: WorkingColorSpace::LinearRec709,
                data,
            });
            for (pattern, intensity, display_labels, safe_frames) in [
                (TestSignalsPattern::Grayscale, 100.0_f32, false, false),
                (TestSignalsPattern::Grayscale, 100.0, true, false),
                (TestSignalsPattern::HdrStaircase, 100.0, false, true),
                (TestSignalsPattern::Checkerboard, 50.0, false, false),
                (TestSignalsPattern::ZonePlate, 100.0, true, true),
            ] {
                let grade = TestSignalsGrade::new(pattern, intensity, display_labels, safe_frames)
                    .expect("valid test signals grade");
                let mut builder = EffectGraphBuilderState::new();
                builder.append_unary(EffectRenderOp::TestSignals { grade });
                let graph = compile_reference_render_graph(builder.finish()).expect("valid graph");
                let plan = lower_effect_graph_to_gpu_plan(&graph).expect("supported point effect");
                assert_eq!(plan.operations().len(), 1);
                let expected = apply_compiled_effect_graph_rgba_f32(
                    mondrian_effects::EffectFloatFrameView::new(
                        mondrian_effects::EffectFrameExtent::new(width, height),
                        mondrian_effects::EffectValueFormat::new(
                            mondrian_effects::EffectWorkingPrecision::Float32,
                            mondrian_effects::EffectColorDomain::SceneLinearRgb,
                            mondrian_core::ColorFrameAlpha::StraightCoverage,
                        ),
                        mondrian_core::ImageSampling::UNIT_SQUARE,
                        &frame.rgba_f32().data,
                    ),
                    &graph,
                    0,
                )
                .expect("CPU float reference");
                // The generator always emits alpha 1, so no expected pixel can be
                // remapped to zero coverage here.
                let expected_pixels: Vec<[f32; 4]> = expected.pixels().to_vec();
                let layer = GpuCompositeLayer {
                    source: GpuCompositeLayerSource::CpuFrame(&frame),
                    opacity: 1.0,
                    blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
                    transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                    effect_plan: Some(&plan),
                    frame_seed: 0,
                };
                let (actual, diagnostics) = readback_test_composite_with_diagnostics(
                    &context,
                    &[layer],
                    width,
                    height,
                    WorkingColorSpace::LinearRec709,
                );
                assert_eq!(diagnostics.execution.fused_point_operations, 1);
                let report = compare_linear_rgba(
                    &expected_pixels,
                    &actual,
                    LinearRgbaAccuracyBudget {
                        rgb: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
                        alpha: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
                    },
                )
                .expect("comparable frames");
                assert!(
                    report.within_budget,
                    "GPU test signals accuracy budget exceeded for {grade:?} at {width}x{height}: {report:#?}"
                );
            }
        }
    }

    /// Renders one four-color-gradient point op on the CPU reference and the
    /// fused GPU pass, and returns both frames.
    ///
    /// Every caller pins a zero-coverage input pixel: the generator has to
    /// paint it, which only happens because `effects_author_coverage` answers
    /// true for kind 35.
    fn four_color_gradient_parity_pixels(
        context: &crate::GpuContext,
        width: u32,
        height: u32,
        data: &[[f32; 4]],
        grade: mondrian_effects::FourColorGradientGrade,
    ) -> (Vec<[f32; 4]>, Vec<[f32; 4]>) {
        use mondrian_effects::{
            apply_compiled_effect_graph_rgba_f32, compile_reference_render_graph,
            lower_effect_graph_to_gpu_plan, EffectGraphBuilderState, EffectRenderOp,
        };

        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
            width,
            height,
            color_space: WorkingColorSpace::LinearRec709,
            data: data.to_vec(),
        });
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(EffectRenderOp::FourColorGradient { grade });
        let graph = compile_reference_render_graph(builder.finish()).expect("valid graph");
        let plan = lower_effect_graph_to_gpu_plan(&graph).expect("supported point effect");
        assert_eq!(plan.operations().len(), 1);
        let expected = apply_compiled_effect_graph_rgba_f32(
            mondrian_effects::EffectFloatFrameView::new(
                mondrian_effects::EffectFrameExtent::new(width, height),
                mondrian_effects::EffectValueFormat::new(
                    mondrian_effects::EffectWorkingPrecision::Float32,
                    mondrian_effects::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &frame.rgba_f32().data,
            ),
            &graph,
            0,
        )
        .expect("CPU float reference");
        let layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::CpuFrame(&frame),
            opacity: 1.0,
            blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: Some(&plan),
            frame_seed: 0,
        };
        let (actual, diagnostics) = readback_test_composite_with_diagnostics(
            context,
            &[layer],
            width,
            height,
            WorkingColorSpace::LinearRec709,
        );
        assert_eq!(diagnostics.execution.fused_point_operations, 1);
        (expected.pixels().to_vec(), actual)
    }

    #[tokio::test]
    async fn gpu_four_color_gradient_matches_the_cpu_kernel() {
        use crate::color_accuracy::{
            compare_linear_rgba, LinearAccuracyBudget, LinearRgbaAccuracyBudget,
        };
        use mondrian_effects::FourColorGradientGrade;

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping GPU four-color gradient parity test: no GPU adapter available");
            return;
        };
        let width = 8_u32;
        let height = 6_u32;
        let mut data = Vec::with_capacity((width * height) as usize);
        for index in 0..width * height {
            let channel = |offset: u32| ((index * (offset + 5)) % 23) as f32 / 23.0;
            data.push([channel(0), channel(1), channel(2), 1.0]);
        }
        // The pinned transparent pixel: (3, 2) at index 2 * 8 + 3 = 19.
        data[19] = [0.0; 4];
        assert_eq!(data[19], [0.0; 4]);

        let grade = FourColorGradientGrade::new(
            [[0.5, 0.5], [7.5, 0.5], [0.5, 5.5], [7.5, 5.5]],
            [
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, 1.0],
                [1.0, 1.0, 0.0],
            ],
            5.0,
            0.0,
            100.0,
            BlendMode::Normal,
        )
        .expect("valid four color gradient grade");
        let (expected_pixels, actual) =
            four_color_gradient_parity_pixels(&context, width, height, &data, grade);
        // The generator authors coverage, so the transparent input pixel must
        // come back painted rather than skipped.
        assert_ne!(actual[19], [0.0; 4]);
        for (index, pixel) in actual.iter().enumerate() {
            if index != 19 {
                assert_ne!(*pixel, [0.0; 4], "pixel {index} lost its coverage");
            }
        }
        let report = compare_linear_rgba(
            &expected_pixels,
            &actual,
            LinearRgbaAccuracyBudget {
                rgb: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
                alpha: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
            },
        )
        .expect("comparable frames");
        assert!(
            report.within_budget,
            "GPU four color gradient accuracy budget exceeded: {report:#?}"
        );
    }

    #[tokio::test]
    async fn gpu_four_color_gradient_dither_matches_the_cpu_kernel() {
        use crate::color_accuracy::{
            compare_linear_rgba, LinearAccuracyBudget, LinearRgbaAccuracyBudget,
        };
        use mondrian_effects::FourColorGradientGrade;

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping GPU four-color gradient dither test: no GPU adapter available");
            return;
        };
        let width = 17_u32;
        let height = 13_u32;
        let mut data = Vec::with_capacity((width * height) as usize);
        for index in 0..width * height {
            let channel = |offset: u32| ((index * (offset + 7)) % 29) as f32 / 29.0;
            data.push([channel(0), channel(1), channel(2), 1.0]);
        }
        let zero_coverage = 0_usize;
        data[zero_coverage] = [0.0; 4];
        assert_eq!(data[zero_coverage], [0.0; 4]);

        let grade = FourColorGradientGrade::new(
            [[0.0, 0.0], [16.0, 0.0], [0.0, 12.0], [16.0, 12.0]],
            [
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, 1.0],
                [1.0, 1.0, 0.0],
            ],
            40.0,
            200.0,
            65.0,
            BlendMode::Multiply,
        )
        .expect("valid four color gradient grade");
        let (expected_pixels, actual) =
            four_color_gradient_parity_pixels(&context, width, height, &data, grade);
        assert_ne!(actual[zero_coverage], [0.0; 4]);
        // At zero coverage the multiply mode is ignored and only the
        // gradient-plus-dither sum survives, so the two kernels have to agree
        // far inside the general budget: the dither hash is bit-identical.
        for channel in 0..3 {
            let difference =
                (expected_pixels[zero_coverage][channel] - actual[zero_coverage][channel]).abs();
            assert!(
                difference <= 1.0e-6,
                "dither channel {channel} diverged by {difference}"
            );
        }
        let report = compare_linear_rgba(
            &expected_pixels,
            &actual,
            LinearRgbaAccuracyBudget {
                rgb: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
                alpha: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
            },
        )
        .expect("comparable frames");
        assert!(
            report.within_budget,
            "GPU four color gradient dither accuracy budget exceeded: {report:#?}"
        );
    }

    #[tokio::test]
    async fn gpu_four_color_gradient_blend_modes_match_the_cpu_kernel() {
        use crate::color_accuracy::{
            compare_linear_rgba, LinearAccuracyBudget, LinearRgbaAccuracyBudget,
        };
        use mondrian_effects::FourColorGradientGrade;

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping GPU four-color gradient blend test: no GPU adapter available");
            return;
        };
        let width = 5_u32;
        let height = 5_u32;
        let mut data = Vec::with_capacity((width * height) as usize);
        for index in 0..width * height {
            let channel = |offset: u32| ((index * (offset + 3)) % 19) as f32 / 19.0;
            data.push([channel(0), channel(1), channel(2), 1.0]);
        }
        // The pinned transparent pixel: (2, 2) at index 2 * 5 + 2 = 12.
        let zero_coverage = 12_usize;
        data[zero_coverage] = [0.0; 4];
        assert_eq!(data[zero_coverage], [0.0; 4]);

        let mut seen = 0_usize;
        for option in mondrian_core::display_labels::blend_mode_options() {
            let Some(mode) = option.mode else { continue };
            if mode == BlendMode::Dissolve {
                continue;
            }
            let grade = FourColorGradientGrade::new(
                [[0.5, 0.5], [4.5, 0.5], [0.5, 4.5], [4.5, 4.5]],
                [
                    [0.9, 0.2, 0.2],
                    [0.2, 0.9, 0.2],
                    [0.2, 0.2, 0.9],
                    [0.8, 0.8, 0.1],
                ],
                8.0,
                0.0,
                100.0,
                mode,
            )
            .expect("valid four color gradient grade");
            let (expected_pixels, actual) =
                four_color_gradient_parity_pixels(&context, width, height, &data, grade);
            assert_ne!(actual[zero_coverage], [0.0; 4], "{mode:?} lost coverage");
            let report = compare_linear_rgba(
                &expected_pixels,
                &actual,
                LinearRgbaAccuracyBudget {
                    rgb: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
                    alpha: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
                },
            )
            .expect("comparable frames");
            assert!(
                report.within_budget,
                "GPU four color gradient {mode:?} accuracy budget exceeded: {report:#?}"
            );
            seen += 1;
        }
        assert_eq!(seen, 26, "the effect offers every legal blend mode");
    }

    /// Renders one checkerboard point op on the CPU reference and the fused GPU
    /// pass, and returns both frames.
    ///
    /// Every caller pins a zero-coverage input pixel: the generator has to paint
    /// it, which only happens because `effects_author_coverage` answers true for
    /// kind 36.
    fn checkerboard_parity_pixels(
        context: &crate::GpuContext,
        width: u32,
        height: u32,
        data: &[[f32; 4]],
        grade: mondrian_effects::CheckerboardGrade,
    ) -> (Vec<[f32; 4]>, Vec<[f32; 4]>) {
        use mondrian_effects::{
            apply_compiled_effect_graph_rgba_f32, compile_reference_render_graph,
            lower_effect_graph_to_gpu_plan, EffectGraphBuilderState, EffectRenderOp,
        };

        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
            width,
            height,
            color_space: WorkingColorSpace::LinearRec709,
            data: data.to_vec(),
        });
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(EffectRenderOp::Checkerboard { grade });
        let graph = compile_reference_render_graph(builder.finish()).expect("valid graph");
        let plan = lower_effect_graph_to_gpu_plan(&graph).expect("supported point effect");
        assert_eq!(plan.operations().len(), 1);
        let expected = apply_compiled_effect_graph_rgba_f32(
            mondrian_effects::EffectFloatFrameView::new(
                mondrian_effects::EffectFrameExtent::new(width, height),
                mondrian_effects::EffectValueFormat::new(
                    mondrian_effects::EffectWorkingPrecision::Float32,
                    mondrian_effects::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &frame.rgba_f32().data,
            ),
            &graph,
            0,
        )
        .expect("CPU float reference");
        let layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::CpuFrame(&frame),
            opacity: 1.0,
            blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: Some(&plan),
            frame_seed: 0,
        };
        let (actual, diagnostics) = readback_test_composite_with_diagnostics(
            context,
            &[layer],
            width,
            height,
            WorkingColorSpace::LinearRec709,
        );
        assert_eq!(diagnostics.execution.fused_point_operations, 1);
        (expected.pixels().to_vec(), actual)
    }

    #[tokio::test]
    async fn gpu_checkerboard_matches_the_cpu_kernel() {
        use crate::color_accuracy::{
            compare_linear_rgba, LinearAccuracyBudget, LinearRgbaAccuracyBudget,
        };
        use mondrian_effects::{CheckerboardGrade, CheckerboardSizeFrom};

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping GPU checkerboard parity test: no GPU adapter available");
            return;
        };
        let width = 8_u32;
        let height = 6_u32;
        let mut data = Vec::with_capacity((width * height) as usize);
        for index in 0..width * height {
            let channel = |offset: u32| ((index * (offset + 5)) % 23) as f32 / 23.0;
            data.push([channel(0), channel(1), channel(2), 1.0]);
        }
        // One painted and one empty cell at a one-pixel cell size: (0, 0) is
        // even/even and (1, 0) is odd/even. The empty pixel is transparent so a
        // wrongly painted cell would show up as extra coverage.
        data[1] = [0.0; 4];

        let grade = CheckerboardGrade::new(
            [0.0, 0.0],
            CheckerboardSizeFrom::WidthSlider,
            [0.0, 0.0],
            1.0,
            1.0,
            0.0,
            0.0,
            [1.0, 1.0, 1.0],
            100.0,
            BlendMode::Normal,
        )
        .expect("valid checkerboard grade");
        let (expected_pixels, actual) =
            checkerboard_parity_pixels(&context, width, height, &data, grade);
        // The generator authors coverage, so the transparent input pixel must
        // come back painted rather than skipped.
        assert_ne!(actual[0], [0.0; 4]);
        assert_ne!(expected_pixels[0], [0.0; 4]);
        // The odd cell keeps its empty input pixel untouched.
        assert_eq!(actual[1], [0.0; 4]);
        assert_eq!(expected_pixels[1], [0.0; 4]);

        let report = compare_linear_rgba(
            &expected_pixels,
            &actual,
            LinearRgbaAccuracyBudget {
                rgb: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
                alpha: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
            },
        )
        .expect("comparable frames");
        assert!(
            report.within_budget,
            "GPU checkerboard accuracy budget exceeded: {report:#?}"
        );
    }

    #[tokio::test]
    async fn gpu_checkerboard_feather_and_blend_match_the_cpu_kernel() {
        use crate::color_accuracy::{
            compare_linear_rgba, LinearAccuracyBudget, LinearRgbaAccuracyBudget,
        };
        use mondrian_effects::{CheckerboardGrade, CheckerboardSizeFrom};

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping GPU checkerboard feather test: no GPU adapter available");
            return;
        };
        let width = 17_u32;
        let height = 13_u32;
        // A half-transparent base exercises the straight-alpha blend path.
        let data = vec![[0.2, 0.3, 0.4, 0.5]; (width * height) as usize];

        let grade = CheckerboardGrade::new(
            [0.0, 0.0],
            CheckerboardSizeFrom::WidthHeightSliders,
            [0.0, 0.0],
            5.0,
            5.0,
            3.0,
            3.0,
            [1.0, 1.0, 1.0],
            60.0,
            BlendMode::Multiply,
        )
        .expect("valid checkerboard grade");
        let (expected_pixels, actual) =
            checkerboard_parity_pixels(&context, width, height, &data, grade);

        let report = compare_linear_rgba(
            &expected_pixels,
            &actual,
            LinearRgbaAccuracyBudget {
                rgb: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
                alpha: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
            },
        )
        .expect("comparable frames");
        assert!(
            report.within_budget,
            "GPU checkerboard feather accuracy budget exceeded: {report:#?}"
        );
    }

    #[tokio::test]
    async fn gpu_checkerboard_paints_transparent_layers() {
        use crate::color_accuracy::{
            compare_linear_rgba, LinearAccuracyBudget, LinearRgbaAccuracyBudget,
        };
        use mondrian_effects::{CheckerboardGrade, CheckerboardSizeFrom};

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping GPU checkerboard coverage test: no GPU adapter available");
            return;
        };
        let width = 5_u32;
        let height = 5_u32;
        // A fully transparent layer: without the author-coverage registration the
        // fused pass would skip every pixel and return transparent black.
        let data = vec![[0.0; 4]; (width * height) as usize];

        let grade = CheckerboardGrade::new(
            [128.0, 128.0],
            CheckerboardSizeFrom::WidthSlider,
            [148.48, 148.48],
            16.0,
            8.0,
            0.0,
            0.0,
            [1.0, 1.0, 1.0],
            100.0,
            BlendMode::Normal,
        )
        .expect("valid checkerboard grade");
        let (expected_pixels, actual) =
            checkerboard_parity_pixels(&context, width, height, &data, grade);

        for (index, pixel) in actual.iter().enumerate() {
            assert_ne!(*pixel, [0.0; 4], "pixel {index} was skipped as empty");
            assert!(
                (pixel[3] - 1.0).abs() <= 1.0e-4,
                "pixel {index} lost its coverage: {pixel:?}"
            );
        }
        let report = compare_linear_rgba(
            &expected_pixels,
            &actual,
            LinearRgbaAccuracyBudget {
                rgb: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
                alpha: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
            },
        )
        .expect("comparable frames");
        assert!(
            report.within_budget,
            "GPU checkerboard coverage budget exceeded: {report:#?}"
        );
    }

    /// Renders one grid point op on the CPU reference and the fused GPU pass,
    /// and returns both frames.
    ///
    /// Every caller pins a zero-coverage input pixel: the generator has to paint
    /// it, which only happens because `effects_author_coverage` answers true for
    /// kind 37.
    fn grid_parity_pixels(
        context: &crate::GpuContext,
        width: u32,
        height: u32,
        data: &[[f32; 4]],
        grade: mondrian_effects::GridGrade,
    ) -> (Vec<[f32; 4]>, Vec<[f32; 4]>) {
        use mondrian_effects::{
            apply_compiled_effect_graph_rgba_f32, compile_reference_render_graph,
            lower_effect_graph_to_gpu_plan, EffectGraphBuilderState, EffectRenderOp,
        };

        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
            width,
            height,
            color_space: WorkingColorSpace::LinearRec709,
            data: data.to_vec(),
        });
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(EffectRenderOp::Grid { grade });
        let graph = compile_reference_render_graph(builder.finish()).expect("valid graph");
        let plan = lower_effect_graph_to_gpu_plan(&graph).expect("supported point effect");
        assert_eq!(plan.operations().len(), 1);
        let expected = apply_compiled_effect_graph_rgba_f32(
            mondrian_effects::EffectFloatFrameView::new(
                mondrian_effects::EffectFrameExtent::new(width, height),
                mondrian_effects::EffectValueFormat::new(
                    mondrian_effects::EffectWorkingPrecision::Float32,
                    mondrian_effects::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &frame.rgba_f32().data,
            ),
            &graph,
            0,
        )
        .expect("CPU float reference");
        let layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::CpuFrame(&frame),
            opacity: 1.0,
            blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: Some(&plan),
            frame_seed: 0,
        };
        let (actual, diagnostics) = readback_test_composite_with_diagnostics(
            context,
            &[layer],
            width,
            height,
            WorkingColorSpace::LinearRec709,
        );
        assert_eq!(diagnostics.execution.fused_point_operations, 1);
        (expected.pixels().to_vec(), actual)
    }

    #[tokio::test]
    async fn gpu_grid_matches_the_cpu_kernel() {
        use crate::color_accuracy::{
            compare_linear_rgba, LinearAccuracyBudget, LinearRgbaAccuracyBudget,
        };
        use mondrian_effects::{GridGrade, GridSizeFrom};

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping GPU grid parity test: no GPU adapter available");
            return;
        };
        let width = 8_u32;
        let height = 6_u32;
        // A fully transparent layer: without the author-coverage registration the
        // fused pass would short-circuit every pixel and return transparent black.
        let data = vec![[0.0; 4]; (width * height) as usize];

        let grade = GridGrade::new(
            [128.0, 128.0],
            GridSizeFrom::Corner,
            [153.6, 153.6],
            32.0,
            24.0,
            5.0,
            0.0,
            0.0,
            false,
            [1.0, 1.0, 1.0],
            100.0,
            BlendMode::Normal,
        )
        .expect("valid grid grade");
        let (expected_pixels, actual) = grid_parity_pixels(&context, width, height, &data, grade);
        // Row 0 sits on a horizontal line, so every pixel of that row is
        // painted. Pixel (3, 3) is deep inside the cell on both axes: one
        // painted, one empty.
        assert_eq!(actual[0], [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(actual[3 * 8 + 3], [0.0; 4]);
        assert_eq!(expected_pixels[0], [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(expected_pixels[3 * 8 + 3], [0.0; 4]);

        let report = compare_linear_rgba(
            &expected_pixels,
            &actual,
            LinearRgbaAccuracyBudget {
                rgb: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
                alpha: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
            },
        )
        .expect("comparable frames");
        assert!(
            report.within_budget,
            "GPU grid accuracy budget exceeded: {report:#?}"
        );

        // A zero-width, feathered grid paints nothing. Without the shader-side
        // border guard a pixel centre on an exact cell boundary would match
        // `distance <= half_border` with `half_border == 0` and paint a line,
        // and the feather ramp would paint a half-intensity line everywhere.
        let empty = GridGrade::new(
            [0.0, 0.0],
            GridSizeFrom::WidthHeightSliders,
            [153.6, 153.6],
            4.0,
            4.0,
            0.0,
            6.0,
            6.0,
            false,
            [1.0, 1.0, 1.0],
            100.0,
            BlendMode::Normal,
        )
        .expect("valid zero-border grid grade");
        let (empty_expected, empty_actual) =
            grid_parity_pixels(&context, width, height, &data, empty);
        assert!(
            empty_expected.iter().all(|pixel| *pixel == [0.0; 4]),
            "the CPU reference must paint nothing for a zero-width border"
        );
        assert_eq!(empty_actual, empty_expected);
    }

    #[tokio::test]
    async fn gpu_grid_feather_and_blend_match_the_cpu_kernel() {
        use crate::color_accuracy::{
            compare_linear_rgba, LinearAccuracyBudget, LinearRgbaAccuracyBudget,
        };
        use mondrian_effects::{GridGrade, GridSizeFrom};

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping GPU grid feather test: no GPU adapter available");
            return;
        };
        let width = 17_u32;
        let height = 13_u32;
        // A half-transparent base exercises the straight-alpha blend path.
        let data = vec![[0.2, 0.3, 0.4, 0.5]; (width * height) as usize];

        let grade = GridGrade::new(
            [0.0, 0.0],
            GridSizeFrom::WidthHeightSliders,
            [0.0, 0.0],
            5.0,
            5.0,
            2.0,
            3.0,
            3.0,
            false,
            [1.0, 1.0, 1.0],
            60.0,
            BlendMode::Multiply,
        )
        .expect("valid grid grade");
        let (expected_pixels, actual) = grid_parity_pixels(&context, width, height, &data, grade);

        let report = compare_linear_rgba(
            &expected_pixels,
            &actual,
            LinearRgbaAccuracyBudget {
                rgb: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
                alpha: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
            },
        )
        .expect("comparable frames");
        assert!(
            report.within_budget,
            "GPU grid feather accuracy budget exceeded: {report:#?}"
        );
    }

    #[tokio::test]
    async fn gpu_grid_paints_transparent_layers() {
        use crate::color_accuracy::{
            compare_linear_rgba, LinearAccuracyBudget, LinearRgbaAccuracyBudget,
        };
        use mondrian_effects::{GridGrade, GridSizeFrom};

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping GPU grid coverage test: no GPU adapter available");
            return;
        };
        let width = 5_u32;
        let height = 5_u32;
        // A fully transparent layer: without the author-coverage registration the
        // fused pass would skip every pixel and return transparent black.
        let data = vec![[0.0; 4]; (width * height) as usize];

        let grade = GridGrade::new(
            [128.0, 128.0],
            GridSizeFrom::Corner,
            [153.6, 153.6],
            32.0,
            24.0,
            5.0,
            0.0,
            0.0,
            false,
            [1.0, 1.0, 1.0],
            100.0,
            BlendMode::Normal,
        )
        .expect("valid grid grade");
        let (expected_pixels, actual) = grid_parity_pixels(&context, width, height, &data, grade);
        // Pixel (0, 0) is on both lines and authors full coverage; pixel (3, 3)
        // is inside the cell and has to stay transparent.
        assert_eq!(actual[0], [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(actual[3 * 5 + 3], [0.0; 4]);
        assert!(
            actual.iter().any(|pixel| *pixel != [0.0; 4]),
            "the transparent frame came back empty"
        );

        let report = compare_linear_rgba(
            &expected_pixels,
            &actual,
            LinearRgbaAccuracyBudget {
                rgb: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
                alpha: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
            },
        )
        .expect("comparable frames");
        assert!(
            report.within_budget,
            "GPU grid coverage budget exceeded: {report:#?}"
        );
    }

    /// Renders one circle point op on the CPU reference and the fused GPU pass,
    /// and returns both frames.
    ///
    /// Every caller pins a zero-coverage input pixel: the generator has to paint
    /// it, which only happens because `effects_author_coverage` answers true for
    /// kind 38.
    fn circle_ellipse_parity_pixels(
        context: &crate::GpuContext,
        width: u32,
        height: u32,
        data: &[[f32; 4]],
        grade: mondrian_effects::CircleEllipseGrade,
    ) -> (Vec<[f32; 4]>, Vec<[f32; 4]>) {
        use mondrian_effects::{
            apply_compiled_effect_graph_rgba_f32, compile_reference_render_graph,
            lower_effect_graph_to_gpu_plan, EffectGraphBuilderState, EffectRenderOp,
        };

        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
            width,
            height,
            color_space: WorkingColorSpace::LinearRec709,
            data: data.to_vec(),
        });
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(EffectRenderOp::CircleEllipse { grade });
        let graph = compile_reference_render_graph(builder.finish()).expect("valid graph");
        let plan = lower_effect_graph_to_gpu_plan(&graph).expect("supported point effect");
        assert_eq!(plan.operations().len(), 1);
        let expected = apply_compiled_effect_graph_rgba_f32(
            mondrian_effects::EffectFloatFrameView::new(
                mondrian_effects::EffectFrameExtent::new(width, height),
                mondrian_effects::EffectValueFormat::new(
                    mondrian_effects::EffectWorkingPrecision::Float32,
                    mondrian_effects::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &frame.rgba_f32().data,
            ),
            &graph,
            0,
        )
        .expect("CPU float reference");
        let layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::CpuFrame(&frame),
            opacity: 1.0,
            blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: Some(&plan),
            frame_seed: 0,
        };
        let (actual, diagnostics) = readback_test_composite_with_diagnostics(
            context,
            &[layer],
            width,
            height,
            WorkingColorSpace::LinearRec709,
        );
        assert_eq!(diagnostics.execution.fused_point_operations, 1);
        (expected.pixels().to_vec(), actual)
    }

    #[tokio::test]
    async fn gpu_circle_matches_the_cpu_kernel() {
        use crate::color_accuracy::{
            compare_linear_rgba, LinearAccuracyBudget, LinearRgbaAccuracyBudget,
        };
        use mondrian_effects::CircleEllipseGrade;

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping GPU circle parity test: no GPU adapter available");
            return;
        };
        let width = 8_u32;
        let height = 6_u32;
        // A fully transparent layer: without the author-coverage registration the
        // fused pass would short-circuit every pixel and return transparent black.
        let data = vec![[0.0; 4]; (width * height) as usize];

        // The centre lands on the centre of pixel (3, 2): radius 2.5 with edge 1
        // gives a two-pixel ramp around the radius.
        let grade = CircleEllipseGrade::new(
            [3.5, 2.5],
            2.5,
            1.0,
            0.0,
            0.0,
            false,
            [1.0, 1.0, 1.0],
            100.0,
            BlendMode::Normal,
        )
        .expect("valid circle grade");
        let (expected_pixels, actual) =
            circle_ellipse_parity_pixels(&context, width, height, &data, grade);
        // The centre pixel is fully covered, and the pixel whose centre is three
        // units away sits two ramp widths outside the radius: fully transparent.
        assert_eq!(actual[2 * 8 + 3], [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(actual[2 * 8 + 6], [0.0; 4]);
        assert_eq!(expected_pixels[2 * 8 + 3], [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(expected_pixels[2 * 8 + 6], [0.0; 4]);
        // The ramp really is a ramp: the diagonal pixel is partially covered.
        assert!(
            actual[4 * 8 + 5][3] > 0.0 && actual[4 * 8 + 5][3] < 1.0,
            "the edge ramp must be partial, got {:?}",
            actual[4 * 8 + 5]
        );

        let report = compare_linear_rgba(
            &expected_pixels,
            &actual,
            LinearRgbaAccuracyBudget {
                rgb: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
                alpha: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
            },
        )
        .expect("comparable frames");
        assert!(
            report.within_budget,
            "GPU circle accuracy budget exceeded: {report:#?}"
        );
    }

    #[tokio::test]
    async fn gpu_circle_feather_and_blend_match_the_cpu_kernel() {
        use crate::color_accuracy::{
            compare_linear_rgba, LinearAccuracyBudget, LinearRgbaAccuracyBudget,
        };
        use mondrian_effects::CircleEllipseGrade;

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping GPU circle feather test: no GPU adapter available");
            return;
        };
        let width = 17_u32;
        let height = 13_u32;
        // A half-transparent base exercises the straight-alpha blend path.
        let data = vec![[0.2, 0.3, 0.4, 0.5]; (width * height) as usize];

        let grade = CircleEllipseGrade::new(
            [8.5, 6.5],
            4.0,
            1.0,
            3.0,
            2.0,
            false,
            [0.9, 0.6, 0.3],
            60.0,
            BlendMode::Multiply,
        )
        .expect("valid circle grade");
        let (expected_pixels, actual) =
            circle_ellipse_parity_pixels(&context, width, height, &data, grade);

        let report = compare_linear_rgba(
            &expected_pixels,
            &actual,
            LinearRgbaAccuracyBudget {
                rgb: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
                alpha: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
            },
        )
        .expect("comparable frames");
        assert!(
            report.within_budget,
            "GPU circle feather accuracy budget exceeded: {report:#?}"
        );
    }

    #[tokio::test]
    async fn gpu_circle_paints_transparent_layers() {
        use crate::color_accuracy::{
            compare_linear_rgba, LinearAccuracyBudget, LinearRgbaAccuracyBudget,
        };
        use mondrian_effects::CircleEllipseGrade;

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping GPU circle coverage test: no GPU adapter available");
            return;
        };
        let width = 5_u32;
        let height = 5_u32;
        // A fully transparent layer: without the author-coverage registration the
        // fused pass would skip every pixel and return transparent black.
        let data = vec![[0.0; 4]; (width * height) as usize];

        let grade = CircleEllipseGrade::new(
            [2.5, 2.5],
            2.0,
            1.0,
            0.0,
            0.0,
            true,
            [1.0, 1.0, 1.0],
            100.0,
            BlendMode::Normal,
        )
        .expect("valid inverted circle grade");
        let (expected_pixels, actual) =
            circle_ellipse_parity_pixels(&context, width, height, &data, grade);
        // Inverted, the centre is empty and the far corner is fully covered.
        assert_eq!(actual[2 * 5 + 2], [0.0; 4]);
        assert_eq!(actual[0], [1.0, 1.0, 1.0, 1.0]);
        assert!(
            actual.iter().any(|pixel| *pixel != [0.0; 4]),
            "the transparent frame came back empty"
        );

        let report = compare_linear_rgba(
            &expected_pixels,
            &actual,
            LinearRgbaAccuracyBudget {
                rgb: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
                alpha: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
            },
        )
        .expect("comparable frames");
        assert!(
            report.within_budget,
            "GPU circle coverage budget exceeded: {report:#?}"
        );
    }

    fn fill_parity_pixels(
        context: &crate::GpuContext,
        width: u32,
        height: u32,
        data: &[[f32; 4]],
        grade: mondrian_effects::FillGrade,
    ) -> (Vec<[f32; 4]>, Vec<[f32; 4]>) {
        use mondrian_effects::{
            apply_compiled_effect_graph_rgba_f32, compile_reference_render_graph,
            lower_effect_graph_to_gpu_plan, EffectGraphBuilderState, EffectRenderOp,
        };

        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
            width,
            height,
            color_space: WorkingColorSpace::LinearRec709,
            data: data.to_vec(),
        });
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(EffectRenderOp::Fill { grade });
        let graph = compile_reference_render_graph(builder.finish()).expect("valid graph");
        let plan = lower_effect_graph_to_gpu_plan(&graph).expect("supported point effect");
        assert_eq!(plan.operations().len(), 1);
        // The fused point stage reuses the seven-cell uniform: kind 39 in
        // `header.x`, the inverted flag in `header.w`, the feather in `params`
        // and the paint in `color`. `header.y` stays free because the fill has no
        // blend-mode parameter.
        let uniforms =
            effect_uniforms_for_ops(plan.operations(), None).expect("the fill packs a uniform");
        let (expected_params, expected_color) = grade.gpu_controls();
        assert_eq!(uniforms[0].header, [39, 0, 0, u32::from(grade.invert())]);
        assert_eq!(uniforms[0].params, expected_params);
        assert_eq!(uniforms[0].color, expected_color);
        for cell in [
            uniforms[0].extra0,
            uniforms[0].extra1,
            uniforms[0].extra2,
            uniforms[0].extra3,
        ] {
            assert_eq!(cell, [0.0; 4]);
        }

        let expected = apply_compiled_effect_graph_rgba_f32(
            mondrian_effects::EffectFloatFrameView::new(
                mondrian_effects::EffectFrameExtent::new(width, height),
                mondrian_effects::EffectValueFormat::new(
                    mondrian_effects::EffectWorkingPrecision::Float32,
                    mondrian_effects::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &frame.rgba_f32().data,
            ),
            &graph,
            0,
        )
        .expect("CPU float reference");
        let layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::CpuFrame(&frame),
            opacity: 1.0,
            blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: Some(&plan),
            frame_seed: 0,
        };
        let (actual, diagnostics) = readback_test_composite_with_diagnostics(
            context,
            &[layer],
            width,
            height,
            WorkingColorSpace::LinearRec709,
        );
        assert_eq!(diagnostics.execution.fused_point_operations, 1);
        (expected.pixels().to_vec(), actual)
    }

    #[tokio::test]
    async fn gpu_fill_matches_the_cpu_kernel() {
        use crate::color_accuracy::{
            compare_linear_rgba, LinearAccuracyBudget, LinearRgbaAccuracyBudget,
        };
        use mondrian_effects::FillGrade;

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping GPU fill parity test: no GPU adapter available");
            return;
        };
        let width = 8_u32;
        let height = 6_u32;
        // A fully transparent layer: without the author-coverage registration the
        // fused pass would short-circuit every pixel and return transparent black.
        let data = vec![[0.0; 4]; (width * height) as usize];
        let grade =
            FillGrade::new(3.0, 1.5, false, [1.0, 0.0, 0.0], 0.8).expect("valid fill grade");
        let (expected_pixels, actual) = fill_parity_pixels(&context, width, height, &data, grade);

        // The corner centre sits half a pixel in, so its ramp is 1/6 on x and 1/3
        // on y; the middle is inside both ramps and carries the full opacity.
        let painted = actual[2 * 8 + 4];
        assert_eq!(painted[0], 1.0);
        assert!(
            (painted[3] - 0.8).abs() <= 1.0e-4,
            "the middle must reach the authored opacity, got {painted:?}"
        );
        assert!(
            actual[0][3] > 0.0 && actual[0][3] < 0.1,
            "the corner must sit on the ramp, got {:?}",
            actual[0]
        );

        let report = compare_linear_rgba(
            &expected_pixels,
            &actual,
            LinearRgbaAccuracyBudget {
                rgb: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
                alpha: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
            },
        )
        .expect("comparable frames");
        assert!(
            report.within_budget,
            "GPU fill accuracy budget exceeded: {report:#?}"
        );
    }

    fn cell_pattern_parity_pixels(
        context: &crate::GpuContext,
        width: u32,
        height: u32,
        data: &[[f32; 4]],
        grade: mondrian_effects::CellPatternGrade,
    ) -> (Vec<[f32; 4]>, Vec<[f32; 4]>) {
        use mondrian_effects::{
            apply_compiled_effect_graph_rgba_f32, compile_reference_render_graph,
            lower_effect_graph_to_gpu_plan, EffectGraphBuilderState, EffectRenderOp,
        };

        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
            width,
            height,
            color_space: WorkingColorSpace::LinearRec709,
            data: data.to_vec(),
        });
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(EffectRenderOp::CellPattern { grade });
        let graph = compile_reference_render_graph(builder.finish()).expect("valid graph");
        let plan = lower_effect_graph_to_gpu_plan(&graph).expect("supported point effect");
        assert_eq!(plan.operations().len(), 1);
        // The cell pattern shares the seven-cell point uniform: kind 41 in
        // `header.x`, the cell type and the overflow mode beside it, and the
        // invert/tiling/cycle flags in `header.w`. Only four cells are used, so
        // the other twelve scalars stay zero.
        let uniforms =
            effect_uniforms_for_ops(plan.operations(), None).expect("the pattern packs a uniform");
        let (expected_params, expected_color, expected_extra0, _extra1) = grade.gpu_controls();
        assert_eq!(
            uniforms[0].header,
            [
                41,
                grade.pattern().index(),
                grade.overflow().index(),
                grade.header_flags(),
            ]
        );
        assert_eq!(uniforms[0].params, expected_params);
        assert_eq!(uniforms[0].color, expected_color);
        assert_eq!(uniforms[0].extra0, expected_extra0);
        for cell in [uniforms[0].extra1, uniforms[0].extra2, uniforms[0].extra3] {
            assert_eq!(cell, [0.0; 4]);
        }

        let expected = apply_compiled_effect_graph_rgba_f32(
            mondrian_effects::EffectFloatFrameView::new(
                mondrian_effects::EffectFrameExtent::new(width, height),
                mondrian_effects::EffectValueFormat::new(
                    mondrian_effects::EffectWorkingPrecision::Float32,
                    mondrian_effects::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &frame.rgba_f32().data,
            ),
            &graph,
            0,
        )
        .expect("CPU float reference");
        let layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::CpuFrame(&frame),
            opacity: 1.0,
            blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: Some(&plan),
            frame_seed: 0,
        };
        let (actual, diagnostics) = readback_test_composite_with_diagnostics(
            context,
            &[layer],
            width,
            height,
            WorkingColorSpace::LinearRec709,
        );
        assert_eq!(diagnostics.execution.fused_point_operations, 1);
        (expected.pixels().to_vec(), actual)
    }

    #[tokio::test]
    async fn gpu_cell_pattern_matches_the_cpu_kernel() {
        use crate::color_accuracy::{
            compare_linear_rgba, LinearAccuracyBudget, LinearRgbaAccuracyBudget,
        };
        use mondrian_effects::{CellPatternGrade, CellPatternOverflow, CellPatternType};

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping GPU cell pattern parity test: no GPU adapter available");
            return;
        };
        let width = 8_u32;
        let height = 6_u32;
        // The first pixel is fully transparent: without the author-coverage
        // registration the fused pass would short-circuit it and leave black.
        let mut data = vec![[0.25, 0.25, 0.25, 1.0]; (width * height) as usize];
        data[0] = [0.0; 4];
        let grade = CellPatternGrade::new(
            CellPatternType::Bubbles,
            false,
            100.0,
            CellPatternOverflow::Clip,
            1.0,
            60.0,
            [128.0, 128.0],
            true,
            2.0,
            2.0,
            0.0,
            false,
            1.0,
            0.0,
        )
        .expect("valid cell pattern grade");
        let (expected_pixels, actual) =
            cell_pattern_parity_pixels(&context, width, height, &data, grade);

        assert_eq!(
            actual[0][3], 1.0,
            "the generator must author full coverage over a transparent pixel"
        );
        assert!(
            actual[0][0] > 0.0,
            "the transparent pixel must be painted, got {:?}",
            actual[0]
        );
        assert!(expected_pixels[0][0] > 0.0);

        let report = compare_linear_rgba(
            &expected_pixels,
            &actual,
            LinearRgbaAccuracyBudget {
                rgb: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
                alpha: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
            },
        )
        .expect("comparable frames");
        assert!(
            report.within_budget,
            "GPU cell pattern accuracy budget exceeded: {report:#?}"
        );
    }

    fn fractal_parity_pixels(
        context: &crate::GpuContext,
        width: u32,
        height: u32,
        data: &[[f32; 4]],
        grade: mondrian_effects::FractalGrade,
    ) -> (Vec<[f32; 4]>, Vec<[f32; 4]>, mondrian_effects::FractalGrade) {
        use mondrian_effects::{
            apply_compiled_effect_graph_rgba_f32, compile_reference_render_graph,
            lower_effect_graph_to_gpu_plan, EffectGraphBuilderState, EffectRenderOp,
        };

        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
            width,
            height,
            color_space: WorkingColorSpace::LinearRec709,
            data: data.to_vec(),
        });
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(EffectRenderOp::Fractal { grade });
        let graph = compile_reference_render_graph(builder.finish()).expect("valid graph");
        let plan = lower_effect_graph_to_gpu_plan(&graph).expect("supported point effect");
        assert_eq!(plan.operations().len(), 1);
        let uniforms =
            effect_uniforms_for_ops(plan.operations(), None).expect("the fractal packs a uniform");
        let (params, color, extra0, extra1, extra2) = grade.gpu_controls();
        assert_eq!(
            uniforms[0].header,
            [
                42,
                grade.set_choice().index(),
                grade.equation().index(),
                grade.effective_iterations(),
            ]
        );
        assert_eq!(uniforms[0].params, params);
        assert_eq!(uniforms[0].color, color);
        assert_eq!(uniforms[0].extra0, extra0);
        assert_eq!(uniforms[0].extra1, extra1);
        assert_eq!(uniforms[0].extra2, [extra2[0], extra2[1], 0.0, 0.0]);
        assert_eq!(uniforms[0].extra3, [0.0; 4]);

        let expected = apply_compiled_effect_graph_rgba_f32(
            mondrian_effects::EffectFloatFrameView::new(
                mondrian_effects::EffectFrameExtent::new(width, height),
                mondrian_effects::EffectValueFormat::new(
                    mondrian_effects::EffectWorkingPrecision::Float32,
                    mondrian_effects::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &frame.rgba_f32().data,
            ),
            &graph,
            0,
        )
        .expect("CPU float reference");
        let layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::CpuFrame(&frame),
            opacity: 1.0,
            blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: Some(&plan),
            frame_seed: 0,
        };
        let (actual, diagnostics) = readback_test_composite_with_diagnostics(
            context,
            &[layer],
            width,
            height,
            WorkingColorSpace::LinearRec709,
        );
        assert_eq!(diagnostics.execution.fused_point_operations, 1);
        (expected.pixels().to_vec(), actual, grade)
    }

    fn default_fractal_grade() -> mondrian_effects::FractalGrade {
        mondrian_effects::FractalGrade::new(
            1,
            1,
            [-0.703, 0.0],
            0.0,
            300.0,
            [0.0, 0.0],
            0.0,
            300.0,
            [0.0, 0.0],
            0.0,
            0.0,
            1,
            0.0,
            29.0,
            0.0,
            0.0,
            1,
            4.0,
        )
        .expect("valid default fractal grade")
    }

    /// True when nudging the sample by one floating-point step leaves the CPU
    /// colour alone, so a GPU comparison is meaningful there.
    fn fractal_pixel_is_stable(
        grade: mondrian_effects::FractalGrade,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    ) -> bool {
        grade.sample_is_stable(x, y, width, height)
    }

    #[tokio::test]
    async fn gpu_fractal_probe_pixels_match_the_cpu_kernel_bit_for_bit() {
        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping GPU fractal probe test: no GPU adapter available");
            return;
        };
        let width = 96_u32;
        let height = 96_u32;
        let data = vec![[0.0, 0.0, 0.0, 1.0]; (width * height) as usize];
        let grade = mondrian_effects::FractalGrade::new(
            1,
            1,
            [-0.703, 0.0],
            0.0,
            300.0,
            [0.0, 0.0],
            0.0,
            300.0,
            [0.0, 0.0],
            0.0,
            0.0,
            1,
            0.0,
            29.0,
            0.0,
            0.0,
            1,
            4.0,
        )
        .expect("valid default fractal grade");
        let (expected, actual, grade) =
            fractal_parity_pixels(&context, width, height, &data, grade);

        let stable = |x: u32, y: u32| fractal_pixel_is_stable(grade, x, y, width, height);
        let anchors = [
            (48_u32, 48_u32),
            (48, 0),
            (48, 95),
            (0, 48),
            (0, 0),
            (95, 0),
            (0, 95),
            (95, 95),
        ];
        for (anchor_x, anchor_y) in anchors {
            let mut probe = None;
            for radius in 0..=8_u32 {
                let left = anchor_x.saturating_sub(radius);
                let right = (anchor_x + radius).min(width - 1);
                let top = anchor_y.saturating_sub(radius);
                let bottom = (anchor_y + radius).min(height - 1);
                for y in top..=bottom {
                    for x in left..=right {
                        let on_ring = x == left || x == right || y == top || y == bottom;
                        if on_ring && stable(x, y) {
                            probe = Some((x, y));
                            break;
                        }
                    }
                    if probe.is_some() {
                        break;
                    }
                }
                if probe.is_some() {
                    break;
                }
            }
            let Some((x, y)) = probe else {
                panic!("no numerically stable pixel within 8 of anchor ({anchor_x},{anchor_y})");
            };
            let index = (y * width + x) as usize;
            let (cpu_escape, _) = grade.escape_band(x, y, width, height);
            let difference = (0..4)
                .map(|channel| (actual[index][channel] - expected[index][channel]).abs())
                .fold(0.0_f32, f32::max);
            assert!(
                difference <= 1.0e-6,
                "probe ({x},{y}) near anchor ({anchor_x},{anchor_y}) with escape {cpu_escape} \
                 differs by {difference:e}: cpu {expected:?} vs gpu {actual:?}",
                expected = expected[index],
                actual = actual[index]
            );
        }
    }

    #[tokio::test]
    async fn gpu_fractal_paints_a_fully_transparent_source() {
        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping GPU fractal coverage test: no GPU adapter available");
            return;
        };
        let width = 8_u32;
        let height = 6_u32;
        let data = vec![[0.0; 4]; (width * height) as usize];
        let grade = default_fractal_grade();
        let (_, actual, _) = fractal_parity_pixels(&context, width, height, &data, grade);

        for (index, pixel) in actual.iter().enumerate() {
            assert_eq!(
                pixel[3], 1.0,
                "the generator must author full coverage at pixel {index}, got {pixel:?}"
            );
        }
        assert!(
            actual.iter().any(|pixel| pixel[0] > 0.0),
            "the fully transparent source must still receive fractal colour"
        );
    }

    #[tokio::test]
    async fn gpu_fractal_band_tolerance_holds_outside_escape_boundaries() {
        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping GPU fractal band tolerance test: no GPU adapter available");
            return;
        };
        let width = 96_u32;
        let height = 96_u32;
        let data = vec![[0.0, 0.0, 0.0, 1.0]; (width * height) as usize];
        let grade = default_fractal_grade();
        let (expected, actual, grade) =
            fractal_parity_pixels(&context, width, height, &data, grade);

        // The iteration is chaotic: a sub-ulp difference in the view can flip an
        // escape count and with it a whole colour band. So the claim is not a
        // flat tolerance, it is this: every disagreement must be a colour the
        // CPU itself produces for the same pixel a few hundred ulp away. A
        // genuinely different formula paints colours no nearby orbit can reach
        // and fails here, while chaos is accounted for instead of hidden.
        const TOLERANCE: f32 = 1.0e-4;
        const NEIGHBOURHOOD: [i32; 7] = [0, -512, 512, -2048, 2048, -8192, 8192];
        let frame_pixels = f64::from(width * height);
        let mut compared = 0_usize;
        let mut masked = 0_usize;
        let mut beyond_tolerance = 0_usize;
        let mut unexplained: Vec<(u32, u32, [f32; 4], [f32; 4])> = Vec::new();
        for y in 0..height {
            for x in 0..width {
                let index = (y * width + x) as usize;
                if !fractal_pixel_is_stable(grade, x, y, width, height) {
                    masked += 1;
                    continue;
                }
                compared += 1;
                let difference = (0..4)
                    .map(|channel| (actual[index][channel] - expected[index][channel]).abs())
                    .fold(0.0_f32, f32::max);
                if difference <= TOLERANCE {
                    continue;
                }
                beyond_tolerance += 1;
                let explained = NEIGHBOURHOOD.iter().any(|&step_x| {
                    NEIGHBOURHOOD.iter().any(|&step_y| {
                        let candidate = grade.sample_nudged(x, y, width, height, step_x, step_y);
                        (0..4).all(|channel| {
                            (actual[index][channel] - candidate[channel]).abs() <= TOLERANCE
                        })
                    })
                });
                if !explained {
                    unexplained.push((x, y, expected[index], actual[index]));
                }
            }
        }

        let masked_fraction = masked as f64 / frame_pixels;
        assert!(
            masked_fraction <= 0.25,
            "chaotic samples cover {:.2}% of the frame, which makes this test vacuous",
            masked_fraction * 100.0
        );
        let compared_fraction = compared as f64 / frame_pixels;
        assert!(
            compared_fraction >= 0.5,
            "only {:.2}% of the frame can be compared; the test needs a majority",
            compared_fraction * 100.0
        );
        assert!(
            unexplained.is_empty(),
            "{} of {compared} comparable pixels painted a colour no nearby orbit produces, \
             first offenders: {:#?}",
            unexplained.len(),
            &unexplained[..unexplained.len().min(6)]
        );
        let beyond_fraction = beyond_tolerance as f64 / compared as f64;
        assert!(
            beyond_fraction <= 0.01,
            "{:.2}% of comparable pixels differ by more than {TOLERANCE:e}",
            beyond_fraction * 100.0
        );
    }

    #[tokio::test]
    async fn gpu_fill_feather_and_invert_match_the_cpu_kernel() {
        use crate::color_accuracy::{
            compare_linear_rgba, LinearAccuracyBudget, LinearRgbaAccuracyBudget,
        };
        use mondrian_effects::FillGrade;

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping GPU fill feather test: no GPU adapter available");
            return;
        };
        let width = 17_u32;
        let height = 13_u32;
        // A half-transparent base exercises the straight-alpha blend path.
        let data = vec![[0.2, 0.3, 0.4, 0.5]; (width * height) as usize];
        let grade = FillGrade::new(2.5, 4.0, true, [0.9, 0.6, 0.3], 1.0)
            .expect("valid inverted fill grade");
        let (expected_pixels, actual) = fill_parity_pixels(&context, width, height, &data, grade);

        // Inverted, the border is nearly opaque while the middle keeps the base.
        // The corner keeps 2.5% of its own coverage, so the 0.5 base alpha leaks
        // through and the composite lands just under one.
        assert!(
            (actual[0][3] - 0.9875).abs() <= 1.0e-4,
            "the inverted border must nearly reach opaque, got {:?}",
            actual[0]
        );
        assert!(
            (actual[6 * 17 + 8][3] - 0.5).abs() <= 1.0e-4,
            "the inverted middle must keep the base alpha, got {:?}",
            actual[6 * 17 + 8]
        );

        let report = compare_linear_rgba(
            &expected_pixels,
            &actual,
            LinearRgbaAccuracyBudget {
                rgb: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
                alpha: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
            },
        )
        .expect("comparable frames");
        assert!(
            report.within_budget,
            "GPU fill feather accuracy budget exceeded: {report:#?}"
        );
    }

    #[tokio::test]
    async fn gpu_fill_paints_transparent_layers() {
        use crate::color_accuracy::{
            compare_linear_rgba, LinearAccuracyBudget, LinearRgbaAccuracyBudget,
        };
        use mondrian_effects::FillGrade;

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping GPU fill coverage test: no GPU adapter available");
            return;
        };
        let width = 5_u32;
        let height = 5_u32;
        // A fully transparent layer: without the author-coverage registration the
        // fused pass would skip every pixel and return transparent black.
        let data = vec![[0.0; 4]; (width * height) as usize];
        let grade =
            FillGrade::new(0.0, 0.0, false, [0.0, 1.0, 0.0], 1.0).expect("valid fill grade");
        let (expected_pixels, actual) = fill_parity_pixels(&context, width, height, &data, grade);

        // A hard-edged fill covers everything, so the transparent frame must come
        // back fully painted instead of empty.
        for (index, pixel) in actual.iter().enumerate() {
            assert_eq!(*pixel, [0.0, 1.0, 0.0, 1.0], "pixel {index}");
        }

        let report = compare_linear_rgba(
            &expected_pixels,
            &actual,
            LinearRgbaAccuracyBudget {
                rgb: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
                alpha: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
            },
        )
        .expect("comparable frames");
        assert!(
            report.within_budget,
            "GPU fill coverage budget exceeded: {report:#?}"
        );
    }

    #[tokio::test]
    async fn gpu_color_balance_matches_the_cpu_kernel() {
        use crate::color_accuracy::{
            compare_linear_rgba, LinearAccuracyBudget, LinearRgbaAccuracyBudget,
        };
        use mondrian_effects::{
            apply_compiled_effect_graph_rgba_f32, compile_reference_render_graph,
            lower_effect_graph_to_gpu_plan, ColorBalanceGrade, EffectGraphBuilderState,
            EffectRenderOp,
        };

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping GPU color balance parity test: no GPU adapter available");
            return;
        };
        let width = 8_u32;
        let height = 6_u32;
        // One transparent, one semi-transparent and the rest opaque: the alpha
        // passthrough and the zero-coverage early return both have to match. The
        // palette stops at 16/17 so a positive amount can still clamp, while the
        // preserve-luminosity case keeps every expected channel inside the unit
        // range the composite target can carry.
        let mut data = Vec::with_capacity((width * height) as usize);
        for index in 0..width * height {
            let channel = |offset: u32| ((index * (offset + 3)) % 17) as f32 / 17.0;
            data.push([channel(0), channel(1), channel(2), 1.0]);
        }
        data[0] = [0.0, 0.0, 0.0, 0.0];
        data[5][3] = 0.25;
        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
            width,
            height,
            color_space: WorkingColorSpace::LinearRec709,
            data,
        });
        for (shadow, midtone, highlight, preserve_luminosity) in [
            ([100.0_f32, 0.0, 0.0], [0.0_f32; 3], [0.0_f32; 3], false),
            ([0.0_f32; 3], [0.0_f32; 3], [0.0_f32, 0.0, -100.0], false),
            (
                [30.0_f32, -10.0, 5.0],
                [0.0_f32, 25.0, -15.0],
                [-20.0_f32, 0.0, 40.0],
                true,
            ),
        ] {
            let grade = ColorBalanceGrade::new(shadow, midtone, highlight, preserve_luminosity)
                .expect("valid color balance grade");
            let mut builder = EffectGraphBuilderState::new();
            builder.append_unary(EffectRenderOp::ColorBalance { grade });
            let graph = compile_reference_render_graph(builder.finish()).expect("valid graph");
            let plan = lower_effect_graph_to_gpu_plan(&graph).expect("supported point effect");
            assert_eq!(plan.operations().len(), 1);
            let expected = apply_compiled_effect_graph_rgba_f32(
                mondrian_effects::EffectFloatFrameView::new(
                    mondrian_effects::EffectFrameExtent::new(width, height),
                    mondrian_effects::EffectValueFormat::new(
                        mondrian_effects::EffectWorkingPrecision::Float32,
                        mondrian_effects::EffectColorDomain::SceneLinearRgb,
                        mondrian_core::ColorFrameAlpha::StraightCoverage,
                    ),
                    mondrian_core::ImageSampling::UNIT_SQUARE,
                    &frame.rgba_f32().data,
                ),
                &graph,
                0,
            )
            .expect("CPU float reference");
            let expected_pixels: Vec<[f32; 4]> = expected.pixels().to_vec();
            let layer = GpuCompositeLayer {
                source: GpuCompositeLayerSource::CpuFrame(&frame),
                opacity: 1.0,
                blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_plan: Some(&plan),
                frame_seed: 0,
            };
            let (actual, diagnostics) = readback_test_composite_with_diagnostics(
                &context,
                &[layer],
                width,
                height,
                WorkingColorSpace::LinearRec709,
            );
            assert_eq!(diagnostics.execution.fused_point_operations, 1);
            let report = compare_linear_rgba(
                &expected_pixels,
                &actual,
                LinearRgbaAccuracyBudget {
                    rgb: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
                    alpha: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
                },
            )
            .expect("comparable frames");
            assert!(
                report.within_budget,
                "GPU color balance accuracy budget exceeded for {grade:?}: {report:#?}"
            );
        }
    }

    #[tokio::test]
    async fn gpu_spill_suppressor_matches_the_cpu_kernel() {
        use crate::color_accuracy::{
            compare_linear_rgba, LinearAccuracyBudget, LinearRgbaAccuracyBudget,
        };
        use mondrian_effects::{
            apply_compiled_effect_graph_rgba_f32, compile_reference_render_graph,
            lower_effect_graph_to_gpu_plan, EffectGraphBuilderState, EffectRenderOp,
            SpillSuppressorGrade, SpillSuppressorMethod,
        };

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping GPU spill suppressor parity test: no GPU adapter available");
            return;
        };
        let width = 11_u32;
        let height = 9_u32;
        // A three-colour frame: every pixel is dominated by green, blue or red,
        // so each key finds spill to remove, plus the three primary colours at
        // full strength and one transparent and one semi-transparent pixel. The
        // alpha passthrough and the zero-coverage early return both have to
        // match the CPU kernel.
        let mut data = Vec::with_capacity((width * height) as usize);
        for index in 0..width * height {
            let t = (index % 7) as f32 / 6.0;
            let ramp = |base: f32| (base + 0.3 * t).min(0.85);
            let pixel = match index % 4 {
                0 => [ramp(0.1), ramp(0.55), ramp(0.1)],
                1 => [ramp(0.1), ramp(0.1), ramp(0.55)],
                2 => [ramp(0.55), ramp(0.1), ramp(0.1)],
                _ => [ramp(0.4), ramp(0.45), ramp(0.4)],
            };
            data.push([pixel[0], pixel[1], pixel[2], 1.0]);
        }
        data[0] = [0.0, 0.0, 0.0, 0.0];
        data[1] = [1.0, 0.0, 0.0, 1.0];
        data[2] = [0.0, 1.0, 0.0, 1.0];
        data[3] = [0.0, 0.0, 1.0, 1.0];
        data[4] = [1.0, 0.2, 0.2, 1.0];
        data[5] = [0.2, 1.0, 0.2, 1.0];
        data[6] = [0.2, 0.2, 1.0, 1.0];
        data[7][3] = 0.25;
        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
            width,
            height,
            color_space: WorkingColorSpace::LinearRec709,
            data,
        });
        for grade in [
            // Green key, final result at full suppression: despill, then
            // desaturate, with the luminance correction left at zero.
            SpillSuppressorGrade::new(
                SpillSuppressorMethod::FinalResult,
                100.0,
                [0.0, 1.0, 0.0],
                50.0,
                50.0,
                50.0,
                50.0,
                0.0,
            )
            .expect("valid green key grade"),
            // Blue key, colour only at 60 percent: the shader has to skip the
            // desaturation and the luminance correction as well.
            SpillSuppressorGrade::new(
                SpillSuppressorMethod::ColorOnly,
                60.0,
                [0.0, 0.0, 1.0],
                50.0,
                50.0,
                50.0,
                50.0,
                0.0,
            )
            .expect("valid blue key grade"),
            // Red key with a zero tolerance and a full luminance correction:
            // only pixels that sit on the key's chroma may move, and the source
            // luma has to come back afterwards.
            SpillSuppressorGrade::new(
                SpillSuppressorMethod::FinalResult,
                100.0,
                [1.0, 0.0, 0.0],
                0.0,
                50.0,
                50.0,
                50.0,
                100.0,
            )
            .expect("valid red key grade"),
        ] {
            let mut builder = EffectGraphBuilderState::new();
            builder.append_unary(EffectRenderOp::SpillSuppressor { grade });
            let graph = compile_reference_render_graph(builder.finish()).expect("valid graph");
            let plan = lower_effect_graph_to_gpu_plan(&graph).expect("supported point effect");
            assert_eq!(plan.operations().len(), 1);
            let expected = apply_compiled_effect_graph_rgba_f32(
                mondrian_effects::EffectFloatFrameView::new(
                    mondrian_effects::EffectFrameExtent::new(width, height),
                    mondrian_effects::EffectValueFormat::new(
                        mondrian_effects::EffectWorkingPrecision::Float32,
                        mondrian_effects::EffectColorDomain::SceneLinearRgb,
                        mondrian_core::ColorFrameAlpha::StraightCoverage,
                    ),
                    mondrian_core::ImageSampling::UNIT_SQUARE,
                    &frame.rgba_f32().data,
                ),
                &graph,
                0,
            )
            .expect("CPU float reference");
            let expected_pixels: Vec<[f32; 4]> = expected.pixels().to_vec();
            let layer = GpuCompositeLayer {
                source: GpuCompositeLayerSource::CpuFrame(&frame),
                opacity: 1.0,
                blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_plan: Some(&plan),
                frame_seed: 0,
            };
            let (actual, diagnostics) = readback_test_composite_with_diagnostics(
                &context,
                &[layer],
                width,
                height,
                WorkingColorSpace::LinearRec709,
            );
            assert_eq!(diagnostics.execution.fused_point_operations, 1);
            let report = compare_linear_rgba(
                &expected_pixels,
                &actual,
                LinearRgbaAccuracyBudget {
                    rgb: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
                    alpha: LinearAccuracyBudget::finite(1.0e-4, 1.0e-4, 1.0e-4),
                },
            )
            .expect("comparable frames");
            assert!(
                report.within_budget,
                "GPU spill suppressor accuracy budget exceeded for {grade:?}: {report:#?}"
            );
        }
    }

    #[tokio::test]
    async fn gpu_noise_matches_the_cpu_reference_on_real_wgpu_device() {
        use crate::color_accuracy::{
            compare_linear_rgba, LinearAccuracyBudget, LinearRgbaAccuracyBudget,
        };
        use mondrian_effects::{
            apply_compiled_effect_graph_rgba_f32, compile_reference_render_graph,
            lower_effect_graph_to_gpu_plan, EffectGraphBuilderState, EffectRenderOp, NoiseGrade,
            NoiseType,
        };

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping GPU noise parity test: no GPU adapter available");
            return;
        };
        // Every pixel starts from the same value, so any difference between the
        // two frames is produced by the positional grain itself.
        let mut data = vec![[0.35_f32, 0.5, 0.65, 1.0]; 40];
        data[7] = [2.0, -0.5, 1.75, 0.5];
        data[23] = [0.0, 0.0, 0.0, 0.0];
        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
            width: 8,
            height: 5,
            color_space: WorkingColorSpace::LinearRec709,
            data,
        });
        for (amount, noise_type, clip_output) in [
            (0.55_f32, NoiseType::Rgb, false),
            (0.9, NoiseType::Hls, false),
            (1.0, NoiseType::Rgb, true),
        ] {
            let grade =
                NoiseGrade::new(amount, noise_type, clip_output).expect("valid noise grade");
            let mut builder = EffectGraphBuilderState::new();
            builder.append_unary(EffectRenderOp::Noise { grade });
            let graph = compile_reference_render_graph(builder.finish()).expect("valid graph");
            let plan = lower_effect_graph_to_gpu_plan(&graph).expect("supported point effect");
            assert_eq!(plan.operations().len(), 1);
            let expected = apply_compiled_effect_graph_rgba_f32(
                mondrian_effects::EffectFloatFrameView::new(
                    mondrian_effects::EffectFrameExtent::new(8, 5),
                    mondrian_effects::EffectValueFormat::new(
                        mondrian_effects::EffectWorkingPrecision::Float32,
                        mondrian_effects::EffectColorDomain::SceneLinearRgb,
                        mondrian_core::ColorFrameAlpha::StraightCoverage,
                    ),
                    mondrian_core::ImageSampling::UNIT_SQUARE,
                    &frame.rgba_f32().data,
                ),
                &graph,
                0,
            )
            .expect("CPU float reference");
            let layer = GpuCompositeLayer {
                source: GpuCompositeLayerSource::CpuFrame(&frame),
                opacity: 1.0,
                blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_plan: Some(&plan),
                frame_seed: 0,
            };
            let (actual, diagnostics) = readback_test_composite_with_diagnostics(
                &context,
                &[layer],
                8,
                5,
                WorkingColorSpace::LinearRec709,
            );
            assert_eq!(diagnostics.execution.fused_point_operations, 1);
            let report = compare_linear_rgba(
                expected.pixels(),
                &actual,
                LinearRgbaAccuracyBudget {
                    rgb: LinearAccuracyBudget::finite(1.0e-6, 1.0e-7, 1.0e-6),
                    alpha: LinearAccuracyBudget::finite(1.0e-6, 1.0e-7, 1.0e-6),
                },
            )
            .expect("comparable frames");
            assert!(
                report.within_budget,
                "GPU noise accuracy budget exceeded for {noise_type:?}: {report:#?}"
            );
        }
    }
    #[tokio::test]
    async fn gpu_tint_matches_the_cpu_reference_on_real_wgpu_device() {
        use crate::color_accuracy::{
            compare_linear_rgba, LinearAccuracyBudget, LinearRgbaAccuracyBudget,
        };
        use mondrian_effects::{
            apply_compiled_effect_graph_rgba_f32, compile_reference_render_graph,
            lower_effect_graph_to_gpu_plan, EffectGraphBuilderState, EffectRenderOp, TintGrade,
        };

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping GPU tint parity test: no GPU adapter available");
            return;
        };
        // Neutral, saturated, extended-range, and negative scene-linear values
        // plus one partially transparent pixel: the map must not clamp.
        let data = vec![
            [0.18_f32, 0.18, 0.18, 1.0],
            [0.25, 0.05, 0.02, 1.0],
            [2.0, -0.5, 1.75, 1.0],
            [0.0, 0.0, 0.0, 1.0],
            [0.5, 0.9, 0.1, 1.0],
            [0.35, 0.35, 0.35, 0.25],
        ];
        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
            width: 3,
            height: 2,
            color_space: WorkingColorSpace::LinearRec709,
            data,
        });
        let grade = TintGrade::for_space(
            [0.05, 0.10, 0.02],
            [0.9, 0.85, 0.6],
            0.65,
            WorkingColorSpace::LinearRec709,
        )
        .expect("valid tint");
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(EffectRenderOp::Tint { grade });
        let graph = compile_reference_render_graph(builder.finish()).expect("valid graph");
        let plan = lower_effect_graph_to_gpu_plan(&graph).expect("supported point effect");
        assert_eq!(plan.operations().len(), 1);
        let expected = apply_compiled_effect_graph_rgba_f32(
            mondrian_effects::EffectFloatFrameView::new(
                mondrian_effects::EffectFrameExtent::new(3, 2),
                mondrian_effects::EffectValueFormat::new(
                    mondrian_effects::EffectWorkingPrecision::Float32,
                    mondrian_effects::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &frame.rgba_f32().data,
            ),
            &graph,
            0,
        )
        .expect("CPU float reference");
        let layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::CpuFrame(&frame),
            opacity: 1.0,
            blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: Some(&plan),
            frame_seed: 0,
        };
        let (actual, diagnostics) = readback_test_composite_with_diagnostics(
            &context,
            &[layer],
            3,
            2,
            WorkingColorSpace::LinearRec709,
        );
        assert_eq!(diagnostics.execution.fused_point_operations, 1);
        let report = compare_linear_rgba(
            expected.pixels(),
            &actual,
            LinearRgbaAccuracyBudget {
                rgb: LinearAccuracyBudget::finite(1.0e-6, 1.0e-7, 1.0e-7),
                alpha: LinearAccuracyBudget::finite(1.0e-6, 1.0e-7, 1.0e-6),
            },
        )
        .expect("comparable frames");
        assert!(
            report.within_budget,
            "GPU tint accuracy budget exceeded: {report:#?}"
        );
    }

    #[tokio::test]
    async fn gpu_vibrance_matches_the_cpu_reference_on_real_wgpu_device() {
        use crate::color_accuracy::{
            compare_linear_rgba, LinearAccuracyBudget, LinearRgbaAccuracyBudget,
        };
        use mondrian_effects::{
            apply_compiled_effect_graph_rgba_f32, compile_reference_render_graph,
            lower_effect_graph_to_gpu_plan, EffectGraphBuilderState, EffectRenderOp, VibranceGrade,
        };

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping GPU vibrance parity test: no GPU adapter available");
            return;
        };
        // A neutral pixel must be untouched, a muted colour must move more than a
        // saturated one, and negative / extended-range values keep their sign.
        let data = vec![
            [0.5_f32, 0.5, 0.5, 1.0],
            [0.55, 0.45, 0.42, 1.0],
            [0.9, 0.02, 0.01, 1.0],
            [-0.4, -0.2, -0.1, 1.0],
            [2.0, 0.5, 0.25, 1.0],
            [0.3, 0.3, 0.34, 0.25],
        ];
        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
            width: 3,
            height: 2,
            color_space: WorkingColorSpace::LinearRec709,
            data,
        });
        let grade = VibranceGrade::for_space(0.5, 0.2, WorkingColorSpace::LinearRec709)
            .expect("valid vibrance");
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(EffectRenderOp::Vibrance { grade });
        let graph = compile_reference_render_graph(builder.finish()).expect("valid graph");
        let plan = lower_effect_graph_to_gpu_plan(&graph).expect("supported point effect");
        assert_eq!(plan.operations().len(), 1);
        let expected = apply_compiled_effect_graph_rgba_f32(
            mondrian_effects::EffectFloatFrameView::new(
                mondrian_effects::EffectFrameExtent::new(3, 2),
                mondrian_effects::EffectValueFormat::new(
                    mondrian_effects::EffectWorkingPrecision::Float32,
                    mondrian_effects::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &frame.rgba_f32().data,
            ),
            &graph,
            0,
        )
        .expect("CPU float reference");
        let layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::CpuFrame(&frame),
            opacity: 1.0,
            blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: Some(&plan),
            frame_seed: 0,
        };
        let (actual, diagnostics) = readback_test_composite_with_diagnostics(
            &context,
            &[layer],
            3,
            2,
            WorkingColorSpace::LinearRec709,
        );
        assert_eq!(diagnostics.execution.fused_point_operations, 1);
        let report = compare_linear_rgba(
            expected.pixels(),
            &actual,
            LinearRgbaAccuracyBudget {
                rgb: LinearAccuracyBudget::finite(1.0e-6, 1.0e-7, 1.0e-7),
                alpha: LinearAccuracyBudget::finite(1.0e-6, 1.0e-7, 1.0e-6),
            },
        )
        .expect("comparable frames");
        assert!(
            report.within_budget,
            "GPU vibrance accuracy budget exceeded: {report:#?}"
        );
    }
    #[tokio::test]
    async fn gpu_tritone_matches_the_cpu_reference_on_real_wgpu_device() {
        use crate::color_accuracy::{
            compare_linear_rgba, LinearAccuracyBudget, LinearRgbaAccuracyBudget,
        };
        use mondrian_effects::{
            apply_compiled_effect_graph_rgba_f32, compile_reference_render_graph,
            lower_effect_graph_to_gpu_plan, EffectGraphBuilderState, EffectRenderOp, TritoneGrade,
        };

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping GPU tritone parity test: no GPU adapter available");
            return;
        };
        // Both ramp halves, an extended-range pixel and one partially
        // transparent pixel: the ramp must clamp the position, not the source.
        let data = vec![
            [0.0_f32, 0.0, 0.0, 1.0],
            [0.18, 0.18, 0.18, 1.0],
            [0.5, 0.5, 0.5, 1.0],
            [0.9, 0.9, 0.9, 1.0],
            [2.0, -0.5, 1.75, 1.0],
            [0.35, 0.35, 0.35, 0.25],
        ];
        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
            width: 3,
            height: 2,
            color_space: WorkingColorSpace::LinearRec709,
            data,
        });
        let grade = TritoneGrade::for_space(
            [0.05, 0.10, 0.30],
            [0.50, 0.39, 0.27],
            [1.0, 0.95, 0.80],
            0.35,
            WorkingColorSpace::LinearRec709,
        )
        .expect("valid tritone");
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(EffectRenderOp::Tritone { grade });
        let graph = compile_reference_render_graph(builder.finish()).expect("valid graph");
        let plan = lower_effect_graph_to_gpu_plan(&graph).expect("supported point effect");
        assert_eq!(plan.operations().len(), 1);
        let expected = apply_compiled_effect_graph_rgba_f32(
            mondrian_effects::EffectFloatFrameView::new(
                mondrian_effects::EffectFrameExtent::new(3, 2),
                mondrian_effects::EffectValueFormat::new(
                    mondrian_effects::EffectWorkingPrecision::Float32,
                    mondrian_effects::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &frame.rgba_f32().data,
            ),
            &graph,
            0,
        )
        .expect("CPU float reference");
        let layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::CpuFrame(&frame),
            opacity: 1.0,
            blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: Some(&plan),
            frame_seed: 0,
        };
        let (actual, diagnostics) = readback_test_composite_with_diagnostics(
            &context,
            &[layer],
            3,
            2,
            WorkingColorSpace::LinearRec709,
        );
        assert_eq!(diagnostics.execution.fused_point_operations, 1);
        let report = compare_linear_rgba(
            expected.pixels(),
            &actual,
            LinearRgbaAccuracyBudget {
                rgb: LinearAccuracyBudget::finite(1.0e-6, 1.0e-7, 1.0e-7),
                alpha: LinearAccuracyBudget::finite(1.0e-6, 1.0e-7, 1.0e-6),
            },
        )
        .expect("comparable frames");
        assert!(
            report.within_budget,
            "GPU tritone accuracy budget exceeded: {report:#?}"
        );
    }
    #[tokio::test]
    async fn gpu_threshold_matches_the_cpu_reference_on_real_wgpu_device() {
        use crate::color_accuracy::{
            compare_linear_rgba, LinearAccuracyBudget, LinearRgbaAccuracyBudget,
        };
        use mondrian_effects::{
            apply_compiled_effect_graph_rgba_f32, compile_reference_render_graph,
            lower_effect_graph_to_gpu_plan, EffectGraphBuilderState, EffectRenderOp,
            ThresholdGrade,
        };

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping GPU threshold parity test: no GPU adapter available");
            return;
        };
        let data = vec![
            [0.2_f32, 0.2, 0.2, 1.0],
            [0.8, 0.8, 0.8, 1.0],
            [0.0, 0.9, 0.0, 1.0],
            [0.0, 0.0, 1.0, 1.0],
            [-0.4, 1.6, 0.5, 0.5],
            [0.95, 0.05, 0.05, 0.25],
        ];
        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
            width: 3,
            height: 2,
            color_space: WorkingColorSpace::LinearRec709,
            data,
        });
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(EffectRenderOp::Threshold {
            grade: ThresholdGrade::new(
                0.5,
                WorkingColorSpace::LinearRec709.luminance_coefficients(),
            )
            .expect("valid threshold"),
        });
        let graph = compile_reference_render_graph(builder.finish()).expect("valid graph");
        let plan = lower_effect_graph_to_gpu_plan(&graph).expect("supported point effect");
        assert_eq!(plan.operations().len(), 1);
        let expected = apply_compiled_effect_graph_rgba_f32(
            mondrian_effects::EffectFloatFrameView::new(
                mondrian_effects::EffectFrameExtent::new(3, 2),
                mondrian_effects::EffectValueFormat::new(
                    mondrian_effects::EffectWorkingPrecision::Float32,
                    mondrian_effects::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &frame.rgba_f32().data,
            ),
            &graph,
            0,
        )
        .expect("CPU float reference");
        let layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::CpuFrame(&frame),
            opacity: 1.0,
            blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: Some(&plan),
            frame_seed: 0,
        };
        let (actual, diagnostics) = readback_test_composite_with_diagnostics(
            &context,
            &[layer],
            3,
            2,
            WorkingColorSpace::LinearRec709,
        );
        assert_eq!(diagnostics.execution.fused_point_operations, 1);
        let report = compare_linear_rgba(
            expected.pixels(),
            &actual,
            LinearRgbaAccuracyBudget {
                rgb: LinearAccuracyBudget::finite(1.0e-6, 1.0e-7, 1.0e-7),
                alpha: LinearAccuracyBudget::finite(1.0e-6, 1.0e-7, 1.0e-6),
            },
        )
        .expect("comparable frames");
        assert!(
            report.within_budget,
            "GPU threshold accuracy budget exceeded: {report:#?}"
        );
    }

    #[tokio::test]
    async fn gpu_channel_combiner_matches_the_cpu_reference_on_real_wgpu_device() {
        use crate::color_accuracy::{
            compare_linear_rgba, LinearAccuracyBudget, LinearRgbaAccuracyBudget,
        };
        use mondrian_effects::{
            apply_compiled_effect_graph_rgba_f32, compile_reference_render_graph,
            lower_effect_graph_to_gpu_plan, ChannelCombinerChannel, ChannelCombinerGrade,
            EffectGraphBuilderState, EffectRenderOp,
        };

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping GPU channel combiner parity test: no GPU adapter available");
            return;
        };
        // The fixture carries an empty pixel (0.9, 0.1, 0.2, 0.0), which only
        // the on-see-coverage path can reveal, and a negative-luminance HDR
        // pixel, which exercises the unbounded arithmetic without clamping.
        let data = vec![
            [0.25_f32, 0.5, 0.75, 1.0],
            [0.9, 0.1, 0.2, 0.0],
            [0.5, 0.5, 0.5, 1.0],
            [-0.2, 1.4, 0.35, 1.0],
            [0.05, 0.07, 0.9, 0.6],
            [1.6, 0.0, 0.0, 0.25],
        ];
        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
            width: 3,
            height: 2,
            color_space: WorkingColorSpace::LinearRec709,
            data,
        });
        let coefficients = WorkingColorSpace::LinearRec709.luminance_coefficients();
        let mut builder = EffectGraphBuilderState::new();
        // Per-slot rewrite with a broadcast: one source fills two destinations.
        builder.append_unary(EffectRenderOp::ChannelCombiner {
            grade: ChannelCombinerGrade::new(
                ChannelCombinerChannel::Alpha,
                ChannelCombinerChannel::RedGreen,
                false,
                0.0,
                coefficients,
            )
            .expect("valid default grade"),
        });
        // Derived source that writes coverage.
        builder.append_unary(EffectRenderOp::ChannelCombiner {
            grade: ChannelCombinerGrade::new(
                ChannelCombinerChannel::Hue,
                ChannelCombinerChannel::AlphaRed,
                false,
                0.0,
                coefficients,
            )
            .expect("valid derived-source grade"),
        });
        // Derived target: the luminance ratio rescales the whole RGB triple.
        builder.append_unary(EffectRenderOp::ChannelCombiner {
            grade: ChannelCombinerGrade::new(
                ChannelCombinerChannel::Alpha,
                ChannelCombinerChannel::Luminance,
                false,
                0.0,
                coefficients,
            )
            .expect("valid derived-target grade"),
        });
        let graph = compile_reference_render_graph(builder.finish()).expect("valid graph");
        let plan = lower_effect_graph_to_gpu_plan(&graph).expect("supported point effect");
        assert_eq!(plan.operations().len(), 3);
        let expected = apply_compiled_effect_graph_rgba_f32(
            mondrian_effects::EffectFloatFrameView::new(
                mondrian_effects::EffectFrameExtent::new(3, 2),
                mondrian_effects::EffectValueFormat::new(
                    mondrian_effects::EffectWorkingPrecision::Float32,
                    mondrian_effects::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &frame.rgba_f32().data,
            ),
            &graph,
            0,
        )
        .expect("CPU float reference");
        let layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::CpuFrame(&frame),
            opacity: 1.0,
            blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: Some(&plan),
            frame_seed: 0,
        };
        let (actual, diagnostics) = readback_test_composite_with_diagnostics(
            &context,
            &[layer],
            3,
            2,
            WorkingColorSpace::LinearRec709,
        );
        assert_eq!(diagnostics.execution.fused_point_operations, 3);
        let report = compare_linear_rgba(
            expected.pixels(),
            &actual,
            LinearRgbaAccuracyBudget {
                rgb: LinearAccuracyBudget::finite(3.0e-5, 1.0e-5, 2.0e-5),
                alpha: LinearAccuracyBudget::finite(1.0e-6, 1.0e-7, 1.0e-6),
            },
        )
        .expect("comparable frames");
        assert!(
            report.within_budget,
            "GPU channel combiner accuracy budget exceeded: {report:#?}"
        );
    }

    #[tokio::test]
    async fn gpu_builtin_point_effects_match_cpu_float_reference_on_real_wgpu_device() {
        use crate::color_accuracy::{
            compare_linear_rgba, LinearAccuracyBudget, LinearRgbaAccuracyBudget,
        };
        use mondrian_effects::{
            apply_compiled_effect_graph_rgba_f32, compile_reference_render_graph,
            lower_effect_graph_to_gpu_plan, ArithmeticGrade, ArithmeticOperator,
            BlackAndWhiteGrade, BroadcastLocale, BroadcastSafeGrade, BroadcastSafeMode,
            ChangeToColorBy, ChangeToColorChange, ChangeToColorGrade, ChannelMixerGrade,
            ChannelSource, ColorBalanceGrade, EffectGraphBuilderState, EffectRenderOp, HueBand,
            HueSaturationGrade, InvertChannels, LeaveColorGrade, LeaveColorMatch,
            ShiftChannelsGrade, SpillSuppressorGrade, SpillSuppressorMethod, TestSignalsGrade,
            TestSignalsPattern,
        };

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping GPU built-in point-effect parity test: no GPU adapter available");
            return;
        };
        let data = vec![
            [0.25_f32, 0.5, 0.75, 1.0],
            [0.9, 0.1, 0.2, 1.0],
            [0.5, 0.5, 0.5, 1.0],
            [-0.2, 1.4, 0.35, 1.0],
            [0.05, 0.07, 0.9, 0.6],
            [1.6, 0.0, 0.0, 0.25],
        ];
        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
            width: 3,
            height: 2,
            color_space: WorkingColorSpace::LinearRec709,
            data,
        });
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(EffectRenderOp::Invert {
            channels: InvertChannels::RedGreen,
            blend_with_original: 0.0,
        });
        builder.append_unary(EffectRenderOp::Invert {
            channels: InvertChannels::Hls,
            blend_with_original: 0.35,
        });
        builder.append_unary(EffectRenderOp::HueSaturation {
            grade: HueSaturationGrade::new(
                HueBand::Reds,
                45.0,
                30.0,
                -10.0,
                0.25,
                150.0,
                40.0,
                5.0,
            )
            .expect("valid red band grade"),
        });
        builder.append_unary(EffectRenderOp::HueSaturation {
            grade: HueSaturationGrade::new(
                HueBand::Master,
                -30.0,
                -20.0,
                15.0,
                0.0,
                0.0,
                25.0,
                0.0,
            )
            .expect("valid master band grade"),
        });
        builder.append_unary(EffectRenderOp::BlackAndWhite {
            grade: BlackAndWhiteGrade::new(
                [40.0, 60.0, 40.0, 60.0, 20.0, 80.0],
                0.35,
                [0.9, 0.8, 0.6],
            )
            .expect("valid black and white grade"),
        });
        builder.append_unary(EffectRenderOp::ShiftChannels {
            grade: ShiftChannelsGrade::new(
                ChannelSource::Green,
                ChannelSource::Luminance,
                ChannelSource::Blue,
                ChannelSource::Alpha,
                [0.2126, 0.7152, 0.0722],
            ),
        });
        builder.append_unary(EffectRenderOp::ChannelMixer {
            grade: ChannelMixerGrade::new(
                [
                    90.0, 20.0, 0.0, 0.0, 0.0, 110.0, 0.0, -10.0, 0.0, 0.0, 100.0, 5.0,
                ],
                false,
            )
            .expect("valid channel mixer"),
        });
        builder.append_unary(EffectRenderOp::Arithmetic {
            grade: ArithmeticGrade::new(ArithmeticOperator::Screen, 128.0, 64.0, 200.0, false)
                .expect("valid arithmetic grade"),
        });
        builder.append_unary(EffectRenderOp::LeaveColor {
            grade: LeaveColorGrade::new(100.0, [0.25, 0.5, 0.75], 25.0, 20.0, LeaveColorMatch::Hue)
                .expect("valid leave color grade"),
        });
        builder.append_unary(EffectRenderOp::ChangeToColor {
            grade: ChangeToColorGrade::new(
                [0.25, 0.5, 0.75],
                [0.75, 0.5, 0.25],
                ChangeToColorChange::HueSaturation,
                ChangeToColorBy::Relative,
                1.0,
                1.0,
                1.0,
                0.5,
                0.0,
            )
            .expect("valid change to color grade"),
        });
        builder.append_unary(EffectRenderOp::BroadcastSafe {
            grade: BroadcastSafeGrade::new(
                BroadcastLocale::Ntsc,
                BroadcastSafeMode::ClampRgb,
                100.0,
            )
            .expect("valid broadcast safe grade"),
        });
        builder.append_unary(EffectRenderOp::ColorBalance {
            grade: ColorBalanceGrade::new(
                [40.0, -20.0, 0.0],
                [0.0, 30.0, -25.0],
                [-15.0, 0.0, 35.0],
                true,
            )
            .expect("valid color balance grade"),
        });
        builder.append_unary(EffectRenderOp::SpillSuppressor {
            grade: SpillSuppressorGrade::new(
                SpillSuppressorMethod::FinalResult,
                100.0,
                [0.0, 1.0, 0.0],
                50.0,
                50.0,
                50.0,
                50.0,
                0.0,
            )
            .expect("valid spill suppressor grade"),
        });
        // The generator runs last, so its frame replaces whatever the thirteen
        // colour ops produced; the chain still has to fuse all fourteen ops.
        builder.append_unary(EffectRenderOp::TestSignals {
            grade: TestSignalsGrade::new(TestSignalsPattern::Grayscale, 100.0, true, true)
                .expect("valid test signals grade"),
        });
        let graph = compile_reference_render_graph(builder.finish()).expect("valid built-in graph");
        let plan =
            lower_effect_graph_to_gpu_plan(&graph).expect("supported built-in point effects");
        assert_eq!(plan.operations().len(), 14);

        let expected = apply_compiled_effect_graph_rgba_f32(
            mondrian_effects::EffectFloatFrameView::new(
                mondrian_effects::EffectFrameExtent::new(3, 2),
                mondrian_effects::EffectValueFormat::new(
                    mondrian_effects::EffectWorkingPrecision::Float32,
                    mondrian_effects::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &frame.rgba_f32().data,
            ),
            &graph,
            0,
        )
        .expect("CPU float reference");

        let layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::CpuFrame(&frame),
            opacity: 1.0,
            blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: Some(&plan),
            frame_seed: 0,
        };
        let (actual, diagnostics) = readback_test_composite_with_diagnostics(
            &context,
            &[layer],
            3,
            2,
            WorkingColorSpace::LinearRec709,
        );
        assert_eq!(diagnostics.execution.fused_layer_passes, 1);
        assert_eq!(diagnostics.execution.fused_point_operations, 14);
        let report = compare_linear_rgba(
            expected.pixels(),
            &actual,
            LinearRgbaAccuracyBudget {
                rgb: LinearAccuracyBudget::finite(1.0e-4, 1.0e-5, 2.0e-5),
                alpha: LinearAccuracyBudget::finite(1.0e-6, 1.0e-7, 1.0e-6),
            },
        )
        .expect("matching GPU and CPU frame shapes");
        assert!(
            report.within_budget,
            "GPU built-in point-effect accuracy budget exceeded: {report:#?}"
        );
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
            blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
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
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
            width: 4,
            height: 4,
            color_space: WorkingColorSpace::LinearRec709,
            data: base_data.clone(),
        });
        let overlay = CpuColorFrame::working(WorkingRgbaF32Frame {
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
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
                blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_plan: None,
                frame_seed: 0,
            };
            let overlay_layer = GpuCompositeLayer {
                source: GpuCompositeLayerSource::CpuFrame(&overlay),
                opacity: OPACITY,
                blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(mode),
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
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
            width: 4,
            height: 4,
            color_space: WorkingColorSpace::LinearRec709,
            data: vec![[1.25, -0.25, 0.5, edge]; 16],
        });
        let low_coverage_layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::CpuFrame(&low_coverage),
            opacity: 1.0,
            blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
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
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
            width: 4,
            height: 4,
            color_space: WorkingColorSpace::LinearRec709,
            data: vec![base_pixel; 16],
        });
        let source = CpuColorFrame::working(WorkingRgbaF32Frame {
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
            width: 4,
            height: 4,
            color_space: WorkingColorSpace::LinearRec709,
            data: vec![source_pixel; 16],
        });
        let layers = [
            GpuCompositeLayer {
                source: GpuCompositeLayerSource::CpuFrame(&base),
                opacity: 1.0,
                blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_plan: None,
                frame_seed: 0,
            },
            GpuCompositeLayer {
                source: GpuCompositeLayerSource::CpuFrame(&source),
                opacity: edge,
                blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
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
            mondrian_effects::EffectFloatFrameView::new(
                mondrian_effects::EffectFrameExtent::new(4, 4),
                mondrian_effects::EffectValueFormat::new(
                    mondrian_effects::EffectWorkingPrecision::Float32,
                    mondrian_effects::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &vec![[color.r, color.g, color.b, color.a]; 16],
            ),
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
            blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
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
    async fn bounded_overlay_preserves_accumulator_and_reports_actual_gpu_work() {
        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping bounded-overlay GPU test: no GPU adapter available");
            return;
        };
        let base = CpuColorFrame::working(WorkingRgbaF32Frame {
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
            width: 4,
            height: 4,
            color_space: WorkingColorSpace::LinearRec709,
            data: vec![[0.0, 0.0, 1.0, 1.0]; 16],
        });
        let overlay = CpuColorFrame::working(WorkingRgbaF32Frame {
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
            width: 2,
            height: 2,
            color_space: WorkingColorSpace::LinearRec709,
            data: vec![[1.0, 0.0, 0.0, 1.0]; 4],
        });
        let layers = [
            GpuCompositeLayer {
                source: GpuCompositeLayerSource::CpuFrame(&base),
                opacity: 1.0,
                blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_plan: None,
                frame_seed: 0,
            },
            GpuCompositeLayer {
                source: GpuCompositeLayerSource::CpuFrame(&overlay),
                opacity: 1.0,
                blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
                transform: [1.0, 0.0, 1.0, 0.0, 1.0, 1.0],
                effect_plan: None,
                frame_seed: 0,
            },
        ];

        let (actual, diagnostics) = readback_test_composite_with_diagnostics(
            &context,
            &layers,
            4,
            4,
            WorkingColorSpace::LinearRec709,
        );
        for y in 0..4 {
            for x in 0..4 {
                let expected = if (1..3).contains(&x) && (1..3).contains(&y) {
                    [1.0, 0.0, 0.0, 1.0]
                } else {
                    [0.0, 0.0, 1.0, 1.0]
                };
                assert_eq!(actual[y * 4 + x], expected, "pixel ({x}, {y})");
            }
        }
        assert_eq!(diagnostics.execution.render_passes, 2);
        assert_eq!(diagnostics.execution.tile_draws, 2);
        assert_eq!(diagnostics.execution.logical_full_frame_shader_pixels, 32);
        assert_eq!(diagnostics.execution.shaded_pixels, 20);
        assert_eq!(diagnostics.execution.avoided_shader_pixels, 12);
        assert_eq!(diagnostics.execution.preserved_copy_pixels, 12);
        assert_eq!(diagnostics.execution.preserved_copy_regions, 4);
        assert_eq!(diagnostics.gpu_composited_pixels, 16);
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
                blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
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
            1,
            "only the accumulator read by the second layer needs a new binding"
        );
        assert_eq!(
            after_second.cache_hits - after_first.cache_hits,
            3,
            "both pooled inputs and the immutable zero texture reuse their bindings"
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

    #[tokio::test]
    #[ignore = "requires physical GPU; corner-space image coverage and Nearest readback"]
    async fn physical_gpu_image_borders_use_corner_space_coverage() {
        let context = crate::GpuContext::new().await.expect("actual GPU adapter required");
        let adapter = context.adapter.get_info();
        eprintln!("GPU_IMAGE_BORDER_NATIVE_PRIMARY: {adapter:?}");
        assert!(
            matches!(
                adapter.device_type,
                wgpu::DeviceType::DiscreteGpu | wgpu::DeviceType::IntegratedGpu
            ),
            "image-border qualification requires physical hardware: {adapter:?}"
        );
        assert_eq!(
            product_gpu_working_texture_format(),
            GpuColorFrameTextureFormat::Rgba32Float
        );
        let working = WorkingColorSpace::LinearRec709;
        let mut failures = Vec::new();
        let run = |label: &str,
                   source: &CpuColorFrame,
                   width: u32,
                   height: u32,
                   transform: [f32; 6],
                   opaque_background: bool,
                   expected: &[[f32; 4]],
                   shaded_pixels: u64,
                   failures: &mut Vec<String>| {
            let mut layers = Vec::new();
            if opaque_background {
                // This exact opaque identity base selects fs_opaque_normal for
                // the subsequent transformed source, rather than guessing from alpha.
                layers.push(GpuCompositeLayer {
                    source: GpuCompositeLayerSource::SolidColor(Color {
                        r: 0.,
                        g: 0.,
                        b: 0.,
                        a: 1.,
                    }),
                    opacity: 1.0,
                    blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
                    transform: [1., 0., 0., 0., 1., 0.],
                    effect_plan: None,
                    frame_seed: 0,
                });
            }
            layers.push(GpuCompositeLayer {
                source: GpuCompositeLayerSource::CpuFrame(source),
                opacity: 1.0,
                blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
                transform,
                effect_plan: None,
                frame_seed: 0,
            });
            let (actual, diagnostics) =
                readback_test_composite_with_diagnostics(&context, &layers, width, height, working);
            assert_eq!(actual.len(), expected.len(), "{label}");
            assert_eq!(diagnostics.gpu_passthrough_frames, 0, "{label}");
            assert_eq!(diagnostics.gpu_with_upload_composites, 1, "{label}");
            assert_eq!(diagnostics.cpu_fallback_composites, 0, "{label}");
            assert_eq!(
                diagnostics.gpu_composited_pixels,
                u64::from(width) * u64::from(height),
                "{label}"
            );
            assert_eq!(
                diagnostics.opaque_normal_single_accumulator_composites,
                u64::from(opaque_background),
                "{label}: actual pipeline evidence"
            );
            assert_eq!(
                diagnostics.execution.render_passes,
                if opaque_background { 2 } else { 1 },
                "{label}"
            );
            assert_eq!(
                diagnostics.execution.tile_draws, diagnostics.execution.render_passes,
                "{label}"
            );
            if diagnostics.execution.shaded_pixels != shaded_pixels {
                failures.push(format!(
                    "{label}: shaded {} expected {shaded_pixels}",
                    diagnostics.execution.shaded_pixels
                ));
            }
            for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
                for channel in 0..4 {
                    if !actual[channel].is_finite()
                        || (actual[channel] - expected[channel]).abs() > 1.0e-6
                    {
                        failures.push(format!(
                            "{label}: pixel {index} channel {channel}: {} expected {}",
                            actual[channel], expected[channel]
                        ));
                    }
                }
            }
            eprintln!("GPU_IMAGE_BORDER_CASE: {label} {width}x{height} {diagnostics:?}");
        };
        // A 1x1 image occupies corner-space [0,1)^2. Inverse-mapped
        // destination centers .25/.75 (2x) or .125/.375/.625/.875 (4x)
        // all lie inside. Nearest samples the same literal texel everywhere.
        for alpha in [0.5_f32, 1.0] {
            let color = [0.25, 0.5, 0.75, alpha];
            let source = CpuColorFrame::working(WorkingRgbaF32Frame {
                width: 1,
                height: 1,
                color_space: working,
                alpha: crate::ColorFrameAlpha::StraightCoverage,
                sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
                data: vec![color],
            });
            for size in [2_u32, 4] {
                for opaque in [false, true] {
                    let expected = if opaque {
                        [color[0] * alpha, color[1] * alpha, color[2] * alpha, 1.0]
                    } else {
                        color
                    };
                    run(
                        &format!("scale{size}-alpha{alpha}-opaque{opaque}"),
                        &source,
                        size,
                        size,
                        [size as f32, 0., 0., 0., size as f32, 0.],
                        opaque,
                        &vec![expected; (size * size) as usize],
                        u64::from(size * size) * if opaque { 2 } else { 1 },
                        &mut failures,
                    );
                }
            }
        }
        let color = [0.25, 0.5, 0.75, 0.5];
        let source = CpuColorFrame::working(WorkingRgbaF32Frame {
            width: 1,
            height: 1,
            color_space: working,
            alpha: crate::ColorFrameAlpha::StraightCoverage,
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            data: vec![color],
        });
        for opaque in [false, true] {
            let clear = if opaque { [0., 0., 0., 1.] } else { [0.; 4] };
            let foreground = if opaque {
                [0.125, 0.25, 0.375, 1.]
            } else {
                color
            };
            // The transformed source is [1,3)^2: exactly the central four
            // pixel centers are covered, and the other twelve remain clear.
            let expected = [
                clear, clear, clear, clear, clear, foreground, foreground, clear, clear,
                foreground, foreground, clear, clear, clear, clear, clear,
            ];
            run(
                &format!("translated-alpha.5-opaque{opaque}"),
                &source,
                4,
                4,
                [2., 0., 1., 0., 2., 1.],
                opaque,
                &expected,
                if opaque { 20 } else { 4 },
                &mut failures,
            );
        }
        // This diamond's AABB includes uncovered centers. Exact inverse
        // coordinates are ((x+y-2)/4,(-x+y+2)/4): zero is inside and one is
        // outside. ClampToEdge must not smear the texel over those exterior points.
        let clear = [0.; 4];
        let expected = [
            clear, color, color, clear, color, color, color, color, clear, color, color, clear,
            clear, clear, clear, clear,
        ];
        run(
            "diamond-half-open-support",
            &source,
            4,
            4,
            [2., -2., 2., 2., 2., 0.],
            false,
            &expected,
            16,
            &mut failures,
        );
        let red = [1., 0., 0., 1.];
        let green = [0., 1., 0., 1.];
        let source = CpuColorFrame::working(WorkingRgbaF32Frame {
            width: 2,
            height: 1,
            color_space: working,
            alpha: crate::ColorFrameAlpha::StraightCoverage,
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            data: vec![red, green],
        });
        // Preserve the existing Nearest policy: no interpolated middle color.
        run(
            "nearest-two-texel-control",
            &source,
            4,
            2,
            [2., 0., 0., 0., 2., 0.],
            false,
            &[red, red, green, green, red, red, green, green],
            8,
            &mut failures,
        );
        assert!(
            failures.is_empty(),
            "physical GPU image-border mismatches:\n{}",
            failures.join("\n")
        );
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
        readback_test_composite_with_diagnostics(
            context,
            layers,
            width,
            height,
            working_color_space,
        )
        .0
    }

    fn readback_test_composite_with_diagnostics(
        context: &crate::GpuContext,
        layers: &[GpuCompositeLayer<'_>],
        width: u32,
        height: u32,
        working_color_space: WorkingColorSpace,
    ) -> (Vec<[f32; 4]>, GpuCompositingDiagnostics) {
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
        let diagnostics = record.diagnostics;
        let actual =
            readback_composite_output(context, encoder, &mut table, &record.output, width, height);
        (actual, diagnostics)
    }

    fn readback_spatial_test_composite_with_diagnostics(
        context: &crate::GpuContext,
        layers: &[GpuCompositeSpatialLayer<'_>],
        width: u32,
        height: u32,
        working_color_space: WorkingColorSpace,
    ) -> (Vec<[f32; 4]>, GpuCompositingDiagnostics) {
        readback_spatial_test_composite_with_compositor(
            context,
            layers,
            width,
            height,
            working_color_space,
        )
        .0
    }

    fn readback_spatial_test_composite_with_compositor(
        context: &crate::GpuContext,
        layers: &[GpuCompositeSpatialLayer<'_>],
        width: u32,
        height: u32,
        working_color_space: WorkingColorSpace,
    ) -> (
        (Vec<[f32; 4]>, GpuCompositingDiagnostics),
        GpuFrameCompositor,
    ) {
        let compositor = GpuFrameCompositor::new(&context.device).expect("GPU compositor");
        let mut ids = GpuColorFrameIdAllocator::new(1).expect("frame id allocator");
        let mut table = GpuColorFrameResourceTable::new();
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian-test-gpu-spatial-chain-parity"),
        });
        let record = compositor
            .record_with_spatial_effect_chains(
                &context.device,
                &context.queue,
                &mut encoder,
                &mut ids,
                &mut table,
                None,
                GpuCompositeSpatialRequest { width, height, working_color_space, layers },
            )
            .expect("record GPU spatial chain composite");
        let diagnostics = record.diagnostics;
        let actual =
            readback_composite_output(context, encoder, &mut table, &record.output, width, height);
        ((actual, diagnostics), compositor)
    }

    fn readback_composite_output(
        context: &crate::GpuContext,
        mut encoder: wgpu::CommandEncoder,
        table: &mut GpuColorFrameResourceTable<GpuColorFrameWgpuResource>,
        output: &GpuColorFrameHandle,
        width: u32,
        height: u32,
    ) -> Vec<[f32; 4]> {
        // `copy_texture_to_buffer` demands a 256-byte row stride, so a frame
        // wider than sixteen RGBA32F pixels has to be read back padded.
        let row_bytes = width * 16;
        let stride = row_bytes.div_ceil(256) * 256;
        let readback = context.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mondrian-test-gpu-point-effect-readback"),
            size: u64::from(stride) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let output = table.get(output).expect("composite output resource");
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
                    bytes_per_row: Some(stride),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        context.queue.submit(std::iter::once(encoder.finish()));
        let mapped = map_test_readback(&context.device, &readback);
        let mut actual = Vec::with_capacity((width * height) as usize);
        for row in mapped.chunks_exact(stride as usize).take(height as usize) {
            actual.extend(
                bytemuck::cast_slice::<u8, f32>(&row[..row_bytes as usize])
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
            blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Multiply),
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
    fn opaque_full_canvas_solid_makes_composite_output_opaque() {
        let translucent_foreground = GpuCompositeLayer {
            source: GpuCompositeLayerSource::SolidColor(Color { r: 4.0, g: -2.0, b: 1.0, a: 0.5 }),
            opacity: 1.0,
            blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: None,
            frame_seed: 0,
        };
        let opaque_base = GpuCompositeLayer {
            source: GpuCompositeLayerSource::SolidColor(Color::BLACK),
            ..translucent_foreground
        };
        let layers = [opaque_base, translucent_foreground];
        let request = GpuCompositeRequest {
            width: 16,
            height: 16,
            working_color_space: WorkingColorSpace::LinearRec709,
            layers: &layers,
        };

        assert_eq!(
            composite_output_alpha(&request, |_| false),
            crate::ColorFrameAlpha::Opaque
        );
        assert_eq!(
            composite_output_alpha(&request, |index| index == 1),
            crate::ColorFrameAlpha::Opaque
        );
        assert_eq!(
            composite_output_alpha(&request, |index| index == 0),
            crate::ColorFrameAlpha::StraightCoverage
        );
    }

    #[test]
    fn partial_opaque_solid_does_not_claim_opaque_output() {
        let partial_base = GpuCompositeLayer {
            source: GpuCompositeLayerSource::SolidColor(Color::BLACK),
            opacity: 1.0,
            blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
            transform: [0.5, 0.0, 0.0, 0.0, 0.5, 0.0],
            effect_plan: None,
            frame_seed: 0,
        };
        let layers = [partial_base];
        let request = GpuCompositeRequest {
            width: 16,
            height: 16,
            working_color_space: WorkingColorSpace::LinearRec709,
            layers: &layers,
        };

        assert_eq!(
            composite_output_alpha(&request, |_| false),
            crate::ColorFrameAlpha::StraightCoverage
        );
    }

    #[test]
    fn gpu_composite_request_accepts_media_extent_mismatch_with_affine_transform() {
        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
            width: 8,
            height: 8,
            color_space: WorkingColorSpace::LinearRec709,
            data: vec![[0.0, 0.0, 0.0, 1.0]; 64],
        });
        let layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::CpuFrame(&frame),
            opacity: 1.0,
            blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
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
            blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
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
            blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
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
            blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
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
            blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
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
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
            width: 8,
            height: 8,
            color_space: WorkingColorSpace::LinearRec709,
            data: vec![[0.0, 0.0, 0.0, 1.0]; 64],
        });
        let layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::CpuFrame(&frame),
            opacity: 1.0,
            blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
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
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
            width: 8,
            height: 8,
            color_space: WorkingColorSpace::LinearP3D65,
            data: vec![[0.0, 0.0, 0.0, 1.0]; 64],
        });
        let layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::CpuFrame(&frame),
            opacity: 1.0,
            blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
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

    #[tokio::test]
    #[ignore = "actual hardware GPU; bounded 16/64 cost report, not realtime acceptance"]
    async fn gpu_adjustment_mix_small_cost_qualification() {
        use mondrian_effects::{
            compile_reference_render_graph, lower_effect_graph_to_gpu_plan,
            EffectGraphBuilderState, EffectPassBlendPolicy, EffectRenderOp,
        };
        use std::time::{Duration, Instant};

        // Separate invocations are intentional: inspect the 16x16 report before
        // explicitly requesting 64x64. This gate never expands to HD or UHD.
        let size: u32 = std::env::var("MONDRIAN_GPU_MIX_COST_SIZE")
            .expect("explicit bounded size required")
            .parse()
            .expect("integer size");
        assert!(
            matches!(size, 16 | 64),
            "only the reviewed 16/64 cohorts are admitted"
        );
        let output_path = std::env::var_os("MONDRIAN_GPU_MIX_COST_OUTPUT")
            .expect("fresh diagnostic output path required");
        let context = crate::GpuContext::new().await.expect("actual default GPU context");
        let adapter = context.adapter.get_info();
        eprintln!("GPU_ADJUSTMENT_NATIVE_PRIMARY: {adapter:?}");
        assert!(
            matches!(
                adapter.device_type,
                wgpu::DeviceType::DiscreteGpu | wgpu::DeviceType::IntegratedGpu
            ),
            "software adapters are not this hardware cost gate: {adapter:?}"
        );
        assert_eq!(
            product_gpu_working_texture_format(),
            GpuColorFrameTextureFormat::Rgba32Float
        );
        let compositor_started = Instant::now();
        let compositor = GpuFrameCompositor::new(&context.device).expect("ordinary compositor");
        let compositor_create_ns = compositor_started.elapsed().as_nanos();
        assert!(
            compositor.adjustment_mix_pipeline.get().is_none(),
            "Mix starts lazy"
        );
        let mut graph_builder = EffectGraphBuilderState::new();
        graph_builder.append_unary(EffectRenderOp::ColorAdjust {
            exposure: 1.0,
            contrast: 1.0,
            saturation: 1.0,
            working_color_space: WorkingColorSpace::LinearRec709,
        });
        let compiled =
            compile_reference_render_graph(graph_builder.finish()).expect("sole Exposure graph");
        let point_plan = lower_effect_graph_to_gpu_plan(&compiled).expect("actual point plan");

        #[derive(Clone, Copy)]
        enum Cohort {
            Over,
            CanonicalMix,
            AdversarialMix,
        }
        struct Sample {
            ns: u128,
            texture_bytes: u64,
            textures: usize,
        }
        fn wait(context: &crate::GpuContext, submission: wgpu::SubmissionIndex) {
            context
                .device
                .poll(wgpu::PollType::Wait {
                    submission_index: Some(submission),
                    timeout: Some(Duration::from_secs(10)),
                })
                .expect("proved bounded GPU completion; do not dispatch again after failure");
        }
        fn readback(
            context: &crate::GpuContext,
            table: &GpuColorFrameResourceTable<GpuColorFrameWgpuResource>,
            output: &GpuColorFrameHandle,
            size: u32,
        ) -> Vec<[f32; 4]> {
            let row_bytes = size * 16;
            let stride = row_bytes.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
                * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
            let buffer = context.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("small-mix-cost-independent-readback"),
                size: u64::from(stride) * u64::from(size),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("small-mix-cost-readback-outside-timer"),
                });
            let actual = table.get(output).expect("same retained output owner");
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: &actual.resource().texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(stride),
                        rows_per_image: Some(size),
                    },
                },
                wgpu::Extent3d {
                    width: size,
                    height: size,
                    depth_or_array_layers: 1,
                },
            );
            let submitted = context.queue.submit(std::iter::once(encoder.finish()));
            let slice = buffer.slice(..);
            let (sender, receiver) = std::sync::mpsc::channel();
            slice.map_async(wgpu::MapMode::Read, move |mapped| {
                let _ = sender.send(mapped);
            });
            context
                .device
                .poll(wgpu::PollType::Wait {
                    submission_index: Some(submitted),
                    timeout: Some(Duration::from_secs(10)),
                })
                .expect("bounded readback submission completion");
            // Mapping is a separate asynchronous operation from submission.
            // Drive all pending mapping callbacks, with its own finite bound.
            context
                .device
                .poll(wgpu::PollType::Wait {
                    submission_index: None,
                    timeout: Some(Duration::from_secs(10)),
                })
                .expect("bounded mapping progress");
            receiver
                .recv_timeout(Duration::from_secs(2))
                .expect("readback callback")
                .expect("actual mapped output");
            let mapped = slice.get_mapped_range().expect("mapped output bytes");
            let pixels = mapped
                .chunks_exact(stride as usize)
                .take(size as usize)
                .flat_map(|row| {
                    bytemuck::cast_slice::<u8, f32>(&row[..row_bytes as usize])
                        .chunks_exact(4)
                        .map(|p| [p[0], p[1], p[2], p[3]])
                })
                .collect();
            drop(mapped);
            buffer.unmap();
            pixels
        }
        fn execute(
            context: &crate::GpuContext,
            compositor: &GpuFrameCompositor,
            plan: &mondrian_effects::CompiledEffectGpuPlan,
            size: u32,
            cohort: Cohort,
            seed: u32,
        ) -> Sample {
            // Real changing textures, rather than a cached output or repeated
            // empty submission. Source materialization finishes before timing.
            let sign = if seed & 1 == 0 { 1.0 } else { -1.0 };
            let scale = sign * if seed & 2 == 0 { 1.0 } else { 2.0 };
            let (left_sample, right_sample, amount) = match cohort {
                Cohort::Over | Cohort::CanonicalMix => (
                    [2.0 * scale, -4.0 * scale, 8.0 * scale, 0.5],
                    [4.0 * scale, -8.0 * scale, 16.0 * scale, 0.5],
                    0.5,
                ),
                Cohort::AdversarialMix => {
                    let rgb = sign * f32::from_bits(0x6480_0000);
                    let tiny = f32::from_bits(0x1a00_0000);
                    ([rgb, rgb, rgb, tiny], [-rgb, -rgb, -rgb, 1.0], tiny)
                }
            };
            let mut ids = GpuColorFrameIdAllocator::new(1).expect("actual frame IDs");
            let mut table = GpuColorFrameResourceTable::new();
            let mut prepared =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("small-mix-cost-changing-inputs-excluded"),
                });
            let mut solid = |sample: [f32; 4]| {
                compositor
                    .record_solid_source_pass(
                        &context.device,
                        &context.queue,
                        &mut prepared,
                        &mut ids,
                        &mut table,
                        None,
                        size,
                        size,
                        WorkingColorSpace::LinearRec709,
                        Color {
                            r: sample[0],
                            g: sample[1],
                            b: sample[2],
                            a: sample[3],
                        },
                    )
                    .expect("real source pixels")
                    .output
            };
            let original = solid(left_sample);
            let processed = match cohort {
                Cohort::Over | Cohort::AdversarialMix => Some(solid(right_sample)),
                Cohort::CanonicalMix => None,
            };
            wait(
                context,
                context.queue.submit(std::iter::once(prepared.finish())),
            );
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("small-mix-cost-public-record-submit-completion"),
                });
            let started = Instant::now();
            let record = match cohort {
                Cohort::Over => {
                    let layers = [
                        GpuCompositeLayer {
                            source: GpuCompositeLayerSource::GpuFrame(&original),
                            opacity: 1.0,
                            blend_policy: EffectPassBlendPolicy::Blend(BlendMode::Normal),
                            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                            effect_plan: None,
                            frame_seed: 0,
                        },
                        GpuCompositeLayer {
                            source: GpuCompositeLayerSource::GpuFrame(
                                processed.as_ref().expect("Over input"),
                            ),
                            opacity: 1.0,
                            blend_policy: EffectPassBlendPolicy::Blend(BlendMode::Normal),
                            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                            effect_plan: None,
                            frame_seed: 0,
                        },
                    ];
                    compositor.record(
                        &context.device,
                        &context.queue,
                        &mut encoder,
                        &mut ids,
                        &mut table,
                        None,
                        GpuCompositeRequest {
                            width: size,
                            height: size,
                            working_color_space: WorkingColorSpace::LinearRec709,
                            layers: &layers,
                        },
                    )
                }
                Cohort::CanonicalMix => {
                    let layers = [
                        GpuCompositeLayer {
                            source: GpuCompositeLayerSource::GpuFrame(&original),
                            opacity: 1.0,
                            blend_policy: EffectPassBlendPolicy::Blend(BlendMode::Normal),
                            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                            effect_plan: None,
                            frame_seed: 0,
                        },
                        GpuCompositeLayer {
                            source: GpuCompositeLayerSource::Adjustment,
                            opacity: amount,
                            blend_policy: EffectPassBlendPolicy::Mix,
                            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                            effect_plan: Some(plan),
                            frame_seed: 0,
                        },
                    ];
                    compositor.record(
                        &context.device,
                        &context.queue,
                        &mut encoder,
                        &mut ids,
                        &mut table,
                        None,
                        GpuCompositeRequest {
                            width: size,
                            height: size,
                            working_color_space: WorkingColorSpace::LinearRec709,
                            layers: &layers,
                        },
                    )
                }
                Cohort::AdversarialMix => compositor.record_adjustment_blend_pass(
                    &context.device,
                    &context.queue,
                    &mut encoder,
                    &mut ids,
                    &mut table,
                    None,
                    &original,
                    processed.as_ref().expect("independent processed texture"),
                    amount,
                    EffectPassBlendPolicy::Mix,
                    0,
                    WorkingColorSpace::LinearRec709,
                ),
            }
            .expect("actual public production record");
            let submission = context.queue.submit(std::iter::once(encoder.finish()));
            wait(context, submission);
            let ns = started.elapsed().as_nanos();
            // Readback storage/mapping, independent pixel comparisons and
            // completed owner destruction are outside this timer. Output
            // allocation remains inside the public production record call.
            assert_eq!(
                record.output.descriptor().alpha,
                crate::ColorFrameAlpha::StraightCoverage
            );
            let textures = table.len();
            let texture_bytes = table.logical_texture_bytes().expect("known texture charge");
            let pixels = readback(context, &table, &record.output, size);
            assert_eq!(pixels.len(), (size * size) as usize);
            for pixel in pixels {
                match cohort {
                    Cohort::AdversarialMix => {
                        // Fixed exact rational oracle, not a CPU Mix result.
                        let rgb_bits = if sign > 0.0 { 0xbe80_0000 } else { 0x3e80_0000 };
                        assert_eq!(
                            pixel.map(f32::to_bits),
                            [rgb_bits, rgb_bits, rgb_bits, 0x1a80_0000]
                        );
                    }
                    Cohort::CanonicalMix => {
                        let expected = [3.0 * scale, -6.0 * scale, 12.0 * scale, 0.5];
                        for (actual, wanted) in pixel.into_iter().zip(expected) {
                            assert!(actual.is_finite());
                            assert!(
                                (actual - wanted).abs() <= 4.0e-6 * scale.abs(),
                                "independent exposure/Mix oracle: {pixel:?} vs {expected:?}"
                            );
                        }
                    }
                    Cohort::Over => {
                        let expected = [
                            10.0 / 3.0 * scale,
                            -20.0 / 3.0 * scale,
                            40.0 / 3.0 * scale,
                            0.75,
                        ];
                        for (actual, expected) in pixel.into_iter().zip(expected) {
                            assert!(actual.is_finite());
                            assert!(
                                (actual - expected).abs() <= 4.0e-6 * scale.abs(),
                                "independent ordinary Over oracle: {pixel:?} vs {expected}"
                            );
                        }
                    }
                }
            }
            Sample { ns, texture_bytes, textures }
        }
        fn cohort_report(
            context: &crate::GpuContext,
            compositor: &GpuFrameCompositor,
            plan: &mondrian_effects::CompiledEffectGpuPlan,
            size: u32,
            cohort: Cohort,
            label: &str,
        ) -> serde_json::Value {
            for seed in 0..2 {
                let _ = execute(context, compositor, plan, size, cohort, seed);
            }
            let samples = (2..10)
                .map(|seed| execute(context, compositor, plan, size, cohort, seed))
                .collect::<Vec<_>>();
            let ns = samples.iter().map(|sample| sample.ns).collect::<Vec<_>>();
            let mut sorted = ns.clone();
            sorted.sort_unstable();
            let quantile = |numerator: usize, denominator: usize| {
                sorted[(numerator * sorted.len()).div_ceil(denominator).saturating_sub(1)]
            };
            serde_json::json!({"cohort": label, "warmup": 2, "sample_count": samples.len(),
                "record_submit_completion_ns": ns, "p50_ns": quantile(50, 100),
                "p95_ns": quantile(95, 100), "max_ns": sorted[sorted.len() - 1],
                "known_texture_bytes": samples.iter().map(|sample| sample.texture_bytes).collect::<Vec<_>>(),
                "known_texture_owners": samples.iter().map(|sample| sample.textures).collect::<Vec<_>>()})
        }

        let before = cohort_report(
            &context,
            &compositor,
            &point_plan,
            size,
            Cohort::Over,
            "ordinary_over_before_mix",
        );
        assert!(
            compositor.adjustment_mix_pipeline.get().is_none(),
            "ordinary Over never builds exact Mix"
        );
        // This is the same production lazy initializer, timed independently.
        // 'Cold' means first use for this compositor, not a cleared driver cache.
        let pipeline_started = Instant::now();
        let _ = compositor.adjustment_mix_pipeline(&context.device);
        let first_mix_pipeline_create_ns = pipeline_started.elapsed().as_nanos();
        let first = execute(
            &context,
            &compositor,
            &point_plan,
            size,
            Cohort::CanonicalMix,
            0,
        );
        let common = cohort_report(
            &context,
            &compositor,
            &point_plan,
            size,
            Cohort::CanonicalMix,
            "canonical_inline_mix_common",
        );
        let adversarial = cohort_report(
            &context,
            &compositor,
            &point_plan,
            size,
            Cohort::AdversarialMix,
            "direct_exact_mix_adversarial",
        );
        let after = cohort_report(
            &context,
            &compositor,
            &point_plan,
            size,
            Cohort::Over,
            "ordinary_over_after_mix",
        );
        let report = serde_json::json!({"schema": 1, "qualification": "small GPU diagnostic; no realtime or release throughput acceptance",
            "host_profile": if cfg!(debug_assertions) { "test-debug" } else { "test-release" },
            "size": [size, size], "adapter": format!("{adapter:?}"),
            "production_context": "GpuContext::new/default compiler; no developer SDK override",
            "native_static_compiler": "pinned mach-dxcompiler ReleaseFast archive; host profile does not change its build",
            "compositor_create_ns": compositor_create_ns,
            "first_mix_pipeline_create_ns": first_mix_pipeline_create_ns,
            "first_canonical_mix_record_submit_completion_ns": first.ns,
            "timer_scope": "public record + output allocation + queue submission + proved GPU completion",
            "exclusions": ["changing source texture materialization and completion", "command encoder creation",
                "compiled plan construction", "readback/map/oracles", "completed resource owner destruction", "App/IPC/full NLE"],
            "known_texture_charge_is_not_driver_rss": true,
            "reports": [before, common, adversarial, after]});
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(output_path)
            .expect("new diagnostic report file");
        serde_json::to_writer_pretty(&mut file, &report).expect("diagnostic JSON");
        eprintln!("GPU_MIX_SMALL_COST_REPORT: {report}");
    }

    fn spatial_test_pixels(width: u32, height: u32) -> Vec<[f32; 4]> {
        let mut pixels = Vec::with_capacity((width * height) as usize);
        for y in 0..height {
            for x in 0..width {
                let u = (x as f32 + 0.5) / width as f32;
                let v = (y as f32 + 0.5) / height as f32;
                pixels.push([
                    0.05 + 0.9 * u,
                    0.08 + 0.7 * v,
                    0.2 + 0.6 * (u * 0.4 + v * 0.6),
                    1.0,
                ]);
            }
        }
        pixels
    }

    fn spatial_test_frame(pixels: &[[f32; 4]], width: u32, height: u32) -> CpuColorFrame {
        CpuColorFrame::working(WorkingRgbaF32Frame {
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            alpha: mondrian_core::ColorFrameAlpha::Opaque,
            width,
            height,
            color_space: WorkingColorSpace::LinearRec709,
            data: pixels.to_vec(),
        })
    }

    fn spatial_test_layer<'a>(frame: &'a CpuColorFrame) -> GpuCompositeLayer<'a> {
        GpuCompositeLayer {
            source: GpuCompositeLayerSource::CpuFrame(frame),
            opacity: 1.0,
            blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: None,
            frame_seed: 0,
        }
    }

    fn spatial_test_pipeline(op: mondrian_effects::EffectRenderOp) -> CompiledEffectGpuPipeline {
        use mondrian_effects::{compile_reference_render_graph, EffectGraphBuilderState};
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(op);
        let graph = compile_reference_render_graph(builder.finish()).expect("valid effect graph");
        mondrian_effects::lower_effect_graph_to_gpu_pipeline(&graph)
            .expect("lowered GPU effect pipeline")
    }

    #[tokio::test]
    async fn an_auxiliary_pass_binds_a_second_texture_and_matches_the_cpu_reference() {
        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping Fill matte parity: no GPU adapter available");
            return;
        };
        assert_fill_matte_texture_parity(&context);
    }

    #[tokio::test]
    async fn auxiliary_sampling_is_bilinear_without_float32_filterable() {
        let Ok(context) = crate::GpuContext::new_without_float32_filterable_for_test().await else {
            eprintln!("skipping unfilterable Fill matte parity: no GPU adapter available");
            return;
        };
        assert!(!context.device.features().contains(wgpu::Features::FLOAT32_FILTERABLE));
        assert_fill_matte_texture_parity(&context);
    }

    fn assert_fill_matte_texture_parity(context: &crate::GpuContext) {
        use mondrian_effects::{AuxiliaryRole, EffectRenderOp, FillGrade};
        assert_auxiliary_texture_parity(context, AuxiliaryRole::Matte, |case| {
            let grade = FillGrade::new(2.0, 1.3, case == 1, [0.9, 0.1, 0.6], 0.7).unwrap();
            EffectRenderOp::Fill { grade }
        });
    }

    #[tokio::test]
    async fn spatial_chain_channel_combiner_layer_matches_float32_cpu() {
        use mondrian_effects::{
            AuxiliaryRole, ChannelCombinerChannel as Channel, ChannelCombinerGrade, EffectRenderOp,
        };
        let context = crate::GpuContext::new_without_float32_filterable_for_test()
            .await
            .expect("real GPU adapter for second-layer parity");
        assert_auxiliary_texture_parity(&context, AuxiliaryRole::Layer, |case| {
            let (source, target, invert, solid_alpha) = [
                (Channel::Red, Channel::Red, false, 0.0),
                (Channel::Hue, Channel::Lightness, true, 0.75),
                (Channel::Alpha, Channel::Luminance, false, 0.0),
                (Channel::Red, Channel::Alpha, false, 0.0),
            ][case];
            let grade = ChannelCombinerGrade::new(
                source,
                target,
                invert,
                solid_alpha,
                WorkingColorSpace::LinearRec709.luminance_coefficients(),
            )
            .unwrap();
            EffectRenderOp::ChannelCombiner { grade }
        });
    }

    fn assert_auxiliary_texture_parity(
        context: &crate::GpuContext,
        role: mondrian_effects::AuxiliaryRole,
        operation: impl Fn(usize) -> mondrian_effects::EffectRenderOp,
    ) {
        use mondrian_core::{ImageSampling, WorkingRgbaF32Frame};
        use mondrian_effects::{
            compile_reference_render_graph, lower_effect_graph_to_gpu_pipeline,
            AuxiliaryCoordinateMapping, AuxiliaryFilter, AuxiliaryFloatFrame, AuxiliaryFloatInput,
            AuxiliaryOutOfBounds, AuxiliarySampling, EffectColorDomainContract,
            EffectExecutionSession, EffectFloatFrameView, EffectFrameExtent,
            EffectGraphBuilderState, EffectProcessorControl, EffectValueFormat,
            EffectWorkingPrecision,
        };

        let (width, height) = (7, 5);
        let (matte_width, matte_height) = (3, 2);
        let working_color_space = WorkingColorSpace::LinearRec709;
        let pixels = (0..width * height)
            .map(|index| [index as f32 * 0.1 - 1.0, 0.4, 2.1, (index % 4) as f32 / 3.0])
            .collect::<Vec<_>>();
        let matte = [
            [7.0, -1.0, 0.2, 0.0],
            [-1.5, 0.0, 0.0, 0.2],
            [0.25, 0.0, 0.0, 0.8],
            [0.0, 0.0, 0.0, 1.0],
            [1.75, 0.0, 0.0, 0.6],
            [0.0, 0.0, 0.0, 0.0],
        ];
        let frame = |width, height, data| {
            CpuColorFrame::working(WorkingRgbaF32Frame {
                width,
                height,
                sampling: ImageSampling::UNIT_SQUARE,
                alpha: ColorFrameAlpha::StraightCoverage,
                color_space: working_color_space,
                data,
            })
        };
        let primary_cpu = frame(width, height, pixels.clone());
        let matte_cpu = frame(matte_width, matte_height, matte.to_vec());
        let compositor = GpuFrameCompositor::new(&context.device).expect("GPU compositor");
        let mut ids = GpuColorFrameIdAllocator::new(1).unwrap();
        let mut table = GpuColorFrameResourceTable::new();
        let upload = GpuColorFrameUploadPlan::from_cpu_color_frame(
            ids.allocate().unwrap(),
            &matte_cpu,
            product_gpu_working_texture_format(),
            "fill-matte-test",
        )
        .unwrap();
        let uploaded = GpuColorFrameUploader::upload(&context.device, &context.queue, &upload);
        let matte_handle = uploaded.handle().clone();
        table.insert(uploaded).unwrap();
        let format = EffectValueFormat::new(
            EffectWorkingPrecision::Float32,
            EffectColorDomain::SceneLinearRgb,
            ColorFrameAlpha::StraightCoverage,
        );
        let identity = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
        for (case, (sampling, transform)) in [
            (AuxiliarySampling::PRIMARY_GRID_LINEAR_CLAMP, identity),
            (
                AuxiliarySampling::new(
                    AuxiliaryCoordinateMapping::FrameTransform,
                    AuxiliaryFilter::Linear,
                    AuxiliaryOutOfBounds::Transparent,
                ),
                [1.0, 0.0, -0.3, 0.0, 1.0, 0.15],
            ),
            (AuxiliarySampling::PRIMARY_GRID_NEAREST_CLAMP, identity),
            (AuxiliarySampling::PRIMARY_GRID_LINEAR_CLAMP, identity),
        ]
        .into_iter()
        .enumerate()
        {
            let mut builder = EffectGraphBuilderState::new();
            let aux =
                builder.append_external_input(9, format, EffectColorDomainContract::SCENE_LINEAR);
            let output = builder.add_auxiliary_effect(
                builder.source(),
                aux,
                operation(case),
                role,
                sampling,
            );
            builder.set_current_output(output);
            let graph = compile_reference_render_graph(builder.finish()).unwrap();
            let pipeline = lower_effect_graph_to_gpu_pipeline(&graph).unwrap();
            assert!(matches!(
                pipeline.stages(),
                [EffectGpuStage::Spatial {
                    pass,
                    ..
                }] if pass.auxiliary_slot() == Some(Some(9))
            ));
            let policy = AuxiliaryPolicy::new(AuxiliaryResolution::Authored);
            let cpu_binding = AuxiliaryFloatInput {
                slot: 9,
                raster: AuxiliaryFloatFrame {
                    frame: EffectFloatFrameView::new(
                        EffectFrameExtent::new(matte_width, matte_height),
                        format,
                        ImageSampling::UNIT_SQUARE,
                        &matte,
                    ),
                    policy,
                    transform,
                },
            };
            let cpu = EffectExecutionSession::default()
                .apply_compiled_rgba_f32_with_auxiliary(
                    EffectFloatFrameView::new(
                        EffectFrameExtent::new(width, height),
                        format,
                        ImageSampling::UNIT_SQUARE,
                        &pixels,
                    ),
                    &graph,
                    0,
                    &[cpu_binding],
                    &mut EffectProcessorControl::uncancelled(),
                )
                .unwrap();
            assert!(cpu
                .pixels()
                .iter()
                .zip(&pixels)
                .any(|(out, input)| out.iter().zip(input).any(|(a, b)| (a - b).abs() > 0.1)));
            let expected = cpu
                .pixels()
                .iter()
                .map(|pixel| if pixel[3] <= 0.0 { [0.0; 4] } else { *pixel })
                .collect::<Vec<_>>();
            let layers = [GpuCompositeSpatialLayer {
                layer: spatial_test_layer(&primary_cpu),
                spatial_plan: Some(&pipeline),
            }];
            let request = || GpuCompositeSpatialRequest {
                width,
                height,
                working_color_space,
                layers: &layers,
            };
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("fill-matte-test"),
                });
            let error = compositor
                .record_with_spatial_effect_chains(
                    &context.device,
                    &context.queue,
                    &mut encoder,
                    &mut ids,
                    &mut table,
                    None,
                    request(),
                )
                .err()
                .expect("a missing auxiliary raster must reject");
            assert!(matches!(
                error,
                GpuCompositeError::SpatialChainFailed { pass: "spatial_auxiliary_missing", .. }
            ));
            let gpu_binding =
                GpuSpatialAuxiliaryRaster { slot: 9, frame: &matte_handle, transform, policy };
            let record = compositor
                .record_with_auxiliary_spatial_effect_chains(
                    &context.device,
                    &context.queue,
                    &mut encoder,
                    &mut ids,
                    &mut table,
                    None,
                    request(),
                    &[&[gpu_binding]],
                )
                .expect("record a dual-texture spatial pass");
            assert_eq!(record.diagnostics.spatial_effect_layers, 1);
            assert_eq!(record.diagnostics.spatial_effect_passes, 1);
            assert_eq!(record.diagnostics.cpu_fallback_composites, 0);
            let actual = readback_composite_output(
                context,
                encoder,
                &mut table,
                &record.output,
                width,
                height,
            );
            assert_spatial_chain_matches(&expected, &actual);
            let max_error = expected
                .iter()
                .flatten()
                .zip(actual.iter().flatten())
                .map(|(cpu, gpu)| (cpu - gpu).abs())
                .fold(0.0f32, f32::max);
            eprintln!("{role:?} parity {sampling:?}, max absolute error = {max_error:.9e}");
        }
    }

    fn assert_spatial_chain_matches(expected: &[[f32; 4]], actual: &[[f32; 4]]) {
        assert_eq!(expected.len(), actual.len(), "spatial chain pixel count");
        for (index, (cpu_pixel, gpu_pixel)) in expected.iter().zip(actual.iter()).enumerate() {
            for channel in 0..4 {
                let difference = (cpu_pixel[channel] - gpu_pixel[channel]).abs();
                assert!(
                    difference <= 1.0e-4,
                    "pixel {index} channel {channel}: cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                );
            }
        }
    }

    /// The compositor's `fs_main` source-over branch for a fully opaque base.
    fn spatial_test_normal_over_opaque(base: [f32; 4], blend: [f32; 4]) -> [f32; 4] {
        let base_alpha = base[3].clamp(0.0, 1.0);
        let blend_alpha = blend[3].clamp(0.0, 1.0);
        if blend_alpha <= 0.0 {
            return base;
        }
        if base_alpha <= 0.0 {
            return [blend[0], blend[1], blend[2], blend_alpha];
        }
        let out_alpha = blend_alpha + base_alpha * (1.0 - blend_alpha);
        if out_alpha <= 0.0 {
            return [0.0; 4];
        }
        let mut out = [0.0; 4];
        for channel in 0..3 {
            let premultiplied =
                blend[channel] * blend_alpha + base[channel] * base_alpha * (1.0 - blend_alpha);
            out[channel] = premultiplied / out_alpha;
        }
        out[3] = out_alpha;
        out
    }

    /// Uploads a straight-alpha fixture for the direct spatial-pass parity tests.
    ///
    /// These helpers live here rather than being shared with
    /// `spatial_pass_gpu`'s own test module: the key cleaner parity test drives
    /// [`crate::spatial_pass_gpu::EffectSpatialGpu::record`] directly, because the
    /// compositor's source-over path premultiplies and would not preserve the
    /// colour channels bit-for-bit.
    fn key_cleaner_upload(
        context: &crate::GpuContext,
        pixels: &[[f32; 4]],
        width: u32,
        height: u32,
    ) -> wgpu::Texture {
        let bytes: Vec<u8> = pixels
            .iter()
            .flat_map(|pixel| pixel.iter().flat_map(|value| value.to_le_bytes()))
            .collect();
        let texture = context.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("mondrian.key-cleaner.source"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        context.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 16),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        texture
    }

    fn key_cleaner_scratch(
        context: &crate::GpuContext,
        width: u32,
        height: u32,
        count: usize,
    ) -> Vec<wgpu::Texture> {
        (0..count)
            .map(|_| {
                context.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("mondrian.key-cleaner.scratch"),
                    size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba32Float,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                })
            })
            .collect()
    }

    fn key_cleaner_readback(
        context: &crate::GpuContext,
        texture: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Vec<[f32; 4]> {
        let unpadded = width * 16;
        let padded = unpadded.div_ceil(256) * 256;
        let buffer = context.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mondrian.key-cleaner.readback"),
            size: u64::from(padded) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian.key-cleaner.readback-encoder"),
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        context.queue.submit(Some(encoder.finish()));
        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        let _ = context
            .device
            .poll(wgpu::PollType::Wait { submission_index: None, timeout: None });
        let data = slice.get_mapped_range().expect("mapped readback range");
        let mut pixels = Vec::with_capacity((width * height) as usize);
        for row in 0..height as usize {
            let start = row * padded as usize;
            for column in 0..width as usize {
                let base = start + column * 16;
                let mut pixel = [0.0_f32; 4];
                for (component, value) in pixel.iter_mut().enumerate() {
                    let offset = base + component * 4;
                    *value = f32::from_le_bytes([
                        data[offset],
                        data[offset + 1],
                        data[offset + 2],
                        data[offset + 3],
                    ]);
                }
                pixels.push(pixel);
            }
        }
        drop(data);
        buffer.unmap();
        pixels
    }

    /// A straight-alpha fixture with a soft key edge, a stepped matte and one
    /// isolated hole, so the erosion, the chatter box and the curve all have
    /// something to act on.
    fn key_cleaner_test_pixels(width: u32, height: u32) -> Vec<[f32; 4]> {
        let mut pixels = Vec::with_capacity((width * height) as usize);
        for y in 0..height {
            for x in 0..width {
                let u = (x as f32 + 0.5) / width as f32;
                let v = (y as f32 + 0.5) / height as f32;
                let alpha = if u < 0.35 {
                    0.0
                } else if u < 0.5 {
                    0.4
                } else if u < 0.75 {
                    0.85
                } else {
                    1.0
                };
                let alpha = if x == width / 2 && y == height / 2 {
                    0.0
                } else {
                    alpha
                };
                pixels.push([0.1 + 0.8 * u, 0.2 + 0.6 * v, 0.3 + 0.5 * u * v, alpha]);
            }
        }
        pixels
    }

    #[tokio::test]
    async fn gpu_key_cleaner_matches_the_cpu_kernel() {
        use mondrian_effects::{key_cleaner_frame_f32, EffectGpuSpatialPass, KeyCleanerGrade};

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping key cleaner parity test: no GPU adapter available");
            return;
        };
        let width = 11;
        let height = 9;
        let pixels = key_cleaner_test_pixels(width, height);
        let executor = crate::spatial_pass_gpu::EffectSpatialGpu::new(&context.device);

        let cases: [(&str, [f32; 4]); 7] = [
            ("default", [10.0, 0.0, 0.0, 100.0]),
            ("erode", [3.0, 0.0, 0.0, 100.0]),
            ("soften", [4.0, 0.5, 0.0, 100.0]),
            ("contrast", [3.0, 0.0, 80.0, 100.0]),
            ("half", [3.0, 0.5, 40.0, 50.0]),
            ("strength_zero", [10.0, 0.5, 80.0, 0.0]),
            ("radius_zero", [0.0, 0.0, 0.0, 100.0]),
        ];
        for (index, (label, values)) in cases.iter().enumerate() {
            let grade = KeyCleanerGrade::new(values[0], values[1], values[2], values[3])
                .unwrap_or_else(|error| panic!("{label} grade: {error}"));
            let mut cpu = pixels.clone();
            assert!(
                key_cleaner_frame_f32(&mut cpu, width as usize, height as usize, &grade),
                "{label}: the CPU kernel must accept the frame"
            );

            let pass = EffectGpuSpatialPass::KeyCleaner { grade };
            let passes = crate::spatial_pass_gpu::EffectSpatialGpu::texture_pass_count(&pass)
                .unwrap_or_else(|error| panic!("{label} pass count: {error}"));
            assert_eq!(passes, 3, "{label}: two matte planes and the composite");
            let source = key_cleaner_upload(&context, &pixels, width, height);
            let scratch = key_cleaner_scratch(&context, width, height, passes as usize);
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("mondrian.key-cleaner.encoder"),
                });
            let output = executor
                .record(
                    &context.device,
                    &mut encoder,
                    &pass,
                    &source,
                    &scratch,
                    width,
                    height,
                )
                .unwrap_or_else(|error| panic!("{label} record: {error}"));
            context.queue.submit(Some(encoder.finish()));
            let gpu = key_cleaner_readback(&context, &output, width, height);

            assert_eq!(gpu.len(), cpu.len(), "{label}: pixel count");
            for (pixel_index, (cpu_pixel, gpu_pixel)) in cpu.iter().zip(gpu.iter()).enumerate() {
                for channel in 0..4 {
                    let difference = (cpu_pixel[channel] - gpu_pixel[channel]).abs();
                    assert!(
                        difference <= 1.0e-4,
                        "key cleaner case {index} ({label}) pixel {pixel_index} channel \
                         {channel}: cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                    );
                }
                // The effect never reads the colour channels, so they have to
                // survive the whole chain bit for bit.
                for channel in 0..3 {
                    assert_eq!(
                        cpu_pixel[channel].to_bits(),
                        gpu_pixel[channel].to_bits(),
                        "key cleaner case {index} ({label}) pixel {pixel_index} colour channel \
                         {channel} must be bit-identical"
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn key_cleaner_refuses_a_two_scratch_pool() {
        use crate::spatial_pass_gpu::EffectSpatialGpuError;
        use mondrian_effects::{EffectGpuSpatialPass, KeyCleanerGrade};

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping key cleaner geometry test: no GPU adapter available");
            return;
        };
        let width = 5;
        let height = 4;
        let pixels = key_cleaner_test_pixels(width, height);
        let executor = crate::spatial_pass_gpu::EffectSpatialGpu::new(&context.device);
        let grade = KeyCleanerGrade::new(3.0, 0.5, 40.0, 100.0).expect("valid grade");
        let pass = EffectGpuSpatialPass::KeyCleaner { grade };
        let source = key_cleaner_upload(&context, &pixels, width, height);
        let scratch = key_cleaner_scratch(&context, width, height, 2);
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian.key-cleaner.geometry"),
        });
        let error = executor
            .record(
                &context.device,
                &mut encoder,
                &pass,
                &source,
                &scratch,
                width,
                height,
            )
            .expect_err("two scratch textures cannot hold the softening plane");
        assert!(
            matches!(
                error,
                EffectSpatialGpuError::UnsupportedPass("key_cleaner_scratch_geometry")
            ),
            "unexpected error: {error:?}"
        );
    }

    #[test]
    fn key_cleaner_passes_over_budget_only_above_uhd() {
        use mondrian_effects::{EffectRenderOp, KeyCleanerGrade};

        let grade = KeyCleanerGrade::new(10.0, 0.5, 40.0, 100.0).expect("valid grade");
        let pipeline = spatial_test_pipeline(EffectRenderOp::KeyCleaner { grade });
        // UHD fits: 3840 * 2160 * 16 bytes * 4 textures = 530,841,600 bytes.
        crate::spatial_driver::gpu_spatial_chain_plan(&pipeline, 3840, 2160)
            .expect("UHD fits the 512 MiB spatial scratch budget");
        // DCI 4K does not: 4096 * 2160 * 16 * 4 = 566,231,040 bytes, which is
        // over the same budget. That is the documented consequence of declaring
        // three scratch textures; the same limit already applies to Deband.
        let error = crate::spatial_driver::gpu_spatial_chain_plan(&pipeline, 4096, 2160)
            .expect_err("DCI 4K exceeds the 512 MiB spatial scratch budget");
        let crate::spatial_driver::GpuSpatialChainBlocker::ScratchBudgetExceeded {
            required_bytes,
            budget_bytes,
        } = error
        else {
            panic!("unexpected blocker: {error:?}");
        };
        assert_eq!(required_bytes, 566_231_040);
        assert_eq!(budget_bytes, 512 * 1024 * 1024);
    }

    /// A straight-alpha fixture with a bright disc on a ramp, a hard step, a
    /// soft-alpha band and one fully transparent column, so the two passes, the
    /// gate and all three output modes have something to act on.
    fn smart_blur_test_pixels(width: u32, height: u32) -> Vec<[f32; 4]> {
        let mut pixels = Vec::with_capacity((width * height) as usize);
        for y in 0..height {
            for x in 0..width {
                let u = (x as f32 + 0.5) / width as f32;
                let v = (y as f32 + 0.5) / height as f32;
                let mut colour = [0.15 + 0.7 * u, 0.2 + 0.55 * v, 0.35 + 0.4 * (1.0 - u)];
                let disc = (u - 0.62) * (u - 0.62) + (v - 0.4) * (v - 0.4);
                if disc < 0.05 {
                    colour = [
                        (colour[0] + 0.35).min(1.0),
                        (colour[1] + 0.35).min(1.0),
                        (colour[2] + 0.35).min(1.0),
                    ];
                }
                if u < 0.3 {
                    colour = [colour[0] * 0.25, colour[1] * 0.25, colour[2] * 0.25];
                }
                let alpha = if x == 1 {
                    0.0
                } else if u > 0.8 {
                    0.4
                } else {
                    1.0
                };
                pixels.push([colour[0], colour[1], colour[2], alpha]);
            }
        }
        pixels
    }

    #[tokio::test]
    async fn gpu_smart_blur_matches_the_cpu_kernel() {
        use mondrian_effects::{
            smart_blur_frame_f32, EffectGpuSpatialPass, SmartBlurGrade, SmartBlurMode,
        };

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping smart blur parity test: no GPU adapter available");
            return;
        };
        let width = 16;
        let height = 10;
        let pixels = smart_blur_test_pixels(width, height);
        let executor = crate::spatial_pass_gpu::EffectSpatialGpu::new(&context.device);

        let cases: [(&str, f32, f32, SmartBlurMode); 6] = [
            ("default", 3.0, 25.0, SmartBlurMode::Normal),
            ("wide", 12.0, 60.0, SmartBlurMode::Normal),
            ("edge_only", 4.0, 30.0, SmartBlurMode::EdgeOnly),
            ("overlay_edge", 4.0, 30.0, SmartBlurMode::OverlayEdge),
            ("gate_open", 3.0, 100.0, SmartBlurMode::Normal),
            ("floor", 0.1, 50.0, SmartBlurMode::Normal),
        ];
        for (label, radius, threshold, mode) in cases {
            let grade = SmartBlurGrade::new(radius, threshold, mode)
                .unwrap_or_else(|error| panic!("{label} grade: {error}"));
            let mut cpu = pixels.clone();
            assert!(
                smart_blur_frame_f32(&mut cpu, width as usize, height as usize, &grade),
                "{label}: the CPU kernel must accept the frame"
            );

            let pass = EffectGpuSpatialPass::SmartBlur { grade };
            let passes = crate::spatial_pass_gpu::EffectSpatialGpu::texture_pass_count(&pass)
                .unwrap_or_else(|error| panic!("{label} pass count: {error}"));
            assert_eq!(passes, 2, "{label}: horizontal then vertical");
            let source = key_cleaner_upload(&context, &pixels, width, height);
            let scratch = key_cleaner_scratch(&context, width, height, passes as usize);
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("mondrian.smart-blur.encoder"),
                });
            let output = executor
                .record(
                    &context.device,
                    &mut encoder,
                    &pass,
                    &source,
                    &scratch,
                    width,
                    height,
                )
                .unwrap_or_else(|error| panic!("{label} record: {error}"));
            context.queue.submit(Some(encoder.finish()));
            let gpu = key_cleaner_readback(&context, &output, width, height);

            assert_eq!(gpu.len(), cpu.len(), "{label}: pixel count");
            for (pixel_index, (cpu_pixel, gpu_pixel)) in cpu.iter().zip(gpu.iter()).enumerate() {
                for channel in 0..4 {
                    let difference = (cpu_pixel[channel] - gpu_pixel[channel]).abs();
                    assert!(
                        difference <= 2.0e-4,
                        "smart blur case {label} pixel {pixel_index} channel {channel}: \
                         cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                    );
                }
                // The source alpha is zero on that column, so both sides must
                // stay exactly cleared rather than dividing by zero.
                if cpu_pixel[3].abs() <= f32::EPSILON && gpu_pixel[3].abs() <= f32::EPSILON {
                    for channel in 0..3 {
                        assert_eq!(
                            cpu_pixel[channel], 0.0,
                            "smart blur case {label} pixel {pixel_index}: cleared colour"
                        );
                        assert_eq!(
                            gpu_pixel[channel], 0.0,
                            "smart blur case {label} pixel {pixel_index}: cleared GPU colour"
                        );
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn gpu_chromatic_aberration_removal_matches_the_cpu_layer() {
        use mondrian_effects::{
            chromatic_aberration_removal_frame_f32, ChromaticAberrationRemovalGrade, EffectRenderOp,
        };

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping chromatic aberration removal parity: no GPU adapter available");
            return;
        };
        let (width, height) = (9, 9);
        let pixels = spatial_test_pixels(width, height);
        let frame = spatial_test_frame(&pixels, width, height);
        for (lateral, longitudinal, edge, centre, tolerance) in [
            (1.0, 0.0, 0.0, [0.5, 0.5], 2.0e-4),
            (-2.0, 0.5, 0.0, [0.25, 0.75], 2.0e-4),
            (0.0, 3.0, 0.0, [0.5, 0.5], 2.0e-4),
            (2.0, -2.0, 10.0, [0.5, 0.5], 2.0e-4),
            (1.0, 0.0, -10.0, [0.5, 0.5], 2.0e-4),
            (1.0e-3, 0.0, 0.0, [0.5, 0.5], 1.0e-5),
        ] {
            let grade =
                ChromaticAberrationRemovalGrade::new(lateral, longitudinal, edge, centre).unwrap();
            let pipeline =
                spatial_test_pipeline(EffectRenderOp::ChromaticAberrationRemoval { grade });
            let entries = [GpuCompositeSpatialLayer {
                layer: spatial_test_layer(&frame),
                spatial_plan: Some(&pipeline),
            }];
            let (actual, diagnostics) = readback_spatial_test_composite_with_diagnostics(
                &context,
                &entries,
                width,
                height,
                WorkingColorSpace::LinearRec709,
            );
            let mut expected = pixels.clone();
            assert!(chromatic_aberration_removal_frame_f32(
                &mut expected,
                width as usize,
                height as usize,
                grade
            ));
            assert_eq!(actual.len(), expected.len());
            assert!(expected
                .iter()
                .zip(&pixels)
                .any(|(after, before)| (after[0] - before[0]).abs() > 1.0e-6
                    || (after[2] - before[2]).abs() > 1.0e-6));
            assert_eq!(diagnostics.spatial_effect_layers, 1);
            assert_eq!(diagnostics.spatial_effect_passes, 1);
            assert_eq!(diagnostics.spatial_scratch_textures_peak, 2);
            assert_eq!(diagnostics.spatial_point_stages, 0);
            assert_eq!(diagnostics.cpu_fallback_composites, 0);
            let max_error = max_spatial_float_error(&expected, &actual);
            eprintln!("RES-08 compositor {grade:?}, max absolute error = {max_error:.9e}");
            assert!(
                max_error <= tolerance,
                "{grade:?}: {max_error} > {tolerance}"
            );
        }
    }

    fn max_spatial_float_error(expected: &[[f32; 4]], actual: &[[f32; 4]]) -> f32 {
        assert_eq!(expected.len(), actual.len());
        expected
            .iter()
            .flatten()
            .zip(actual.iter().flatten())
            .map(|(cpu, gpu)| {
                let error = (cpu - gpu).abs();
                assert!(error.is_finite(), "CPU {cpu}, GPU {gpu}");
                error
            })
            .fold(0.0_f32, f32::max)
    }

    fn readback_spatial_stage(
        context: &crate::GpuContext,
        executor: &crate::spatial_pass_gpu::EffectSpatialGpu,
        pixels: &[[f32; 4]],
        width: u32,
        height: u32,
        pass: mondrian_effects::EffectGpuSpatialPass,
        passes: u32,
    ) -> Vec<[f32; 4]> {
        assert_eq!(
            crate::spatial_pass_gpu::EffectSpatialGpu::texture_pass_count(&pass).unwrap(),
            passes
        );
        let source = key_cleaner_upload(context, pixels, width, height);
        let scratch = key_cleaner_scratch(context, width, height, passes as usize);
        let mut encoder = context
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        let output = executor
            .record(
                &context.device,
                &mut encoder,
                &pass,
                &source,
                &scratch,
                width,
                height,
            )
            .unwrap();
        context.queue.submit(Some(encoder.finish()));
        key_cleaner_readback(context, &output, width, height)
    }

    #[tokio::test]
    async fn gpu_chromatic_aberration_removal_leaves_green_and_alpha_untouched() {
        use mondrian_effects::{
            chromatic_aberration_removal_frame_f32, ChromaticAberrationRemovalGrade,
        };

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!(
                "skipping chromatic aberration removal alpha parity: no GPU adapter available"
            );
            return;
        };
        let executor = crate::spatial_pass_gpu::EffectSpatialGpu::new(&context.device);
        let grade = ChromaticAberrationRemovalGrade::new(3.0, -2.0, 7.0, [0.4, 0.7]).unwrap();
        for (width, height) in [(33, 17), (1, 9), (9, 1)] {
            let mut pixels = spatial_test_pixels(width, height);
            for (index, pixel) in pixels.iter_mut().enumerate() {
                pixel[3] = (index as u32 % width) as f32 / width as f32;
            }
            let mut expected = pixels.clone();
            assert!(chromatic_aberration_removal_frame_f32(
                &mut expected,
                width as usize,
                height as usize,
                grade
            ));
            let actual = readback_spatial_stage(
                &context,
                &executor,
                &pixels,
                width,
                height,
                mondrian_effects::EffectGpuSpatialPass::ChromaticAberrationRemoval { grade },
                1,
            );
            for (input, output) in pixels.iter().zip(&actual) {
                assert_eq!(input[1].to_bits(), output[1].to_bits());
                assert_eq!(input[3].to_bits(), output[3].to_bits());
                if input[3] == 0.0 || width == 1 || height == 1 {
                    assert_eq!(input.map(f32::to_bits), output.map(f32::to_bits));
                }
            }
            let max_error = max_spatial_float_error(&expected, &actual);
            eprintln!("RES-08 alpha {width}x{height}, max absolute error = {max_error:.9e}");
            assert!(max_error <= 2.0e-4, "{width}x{height}: {max_error}");
        }
    }

    #[tokio::test]
    async fn gpu_chromatic_aberration_removal_clamps_at_the_frame_edge() {
        use mondrian_effects::{
            chromatic_aberration_removal_frame_f32, ChromaticAberrationRemovalGrade,
        };

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!(
                "skipping chromatic aberration removal edge parity: no GPU adapter available"
            );
            return;
        };
        let (width, height) = (9, 9);
        let pixels: Vec<_> = (0..width * height)
            .map(|index| {
                [
                    ((index * 23) % 61) as f32 / 61.0,
                    ((index * 17) % 67) as f32 / 67.0,
                    ((index * 31) % 73) as f32 / 73.0,
                    1.0,
                ]
            })
            .collect();
        let grade = ChromaticAberrationRemovalGrade::new(5.0, 5.0, 10.0, [0.5, 0.5]).unwrap();
        let executor = crate::spatial_pass_gpu::EffectSpatialGpu::new(&context.device);
        let actual = readback_spatial_stage(
            &context,
            &executor,
            &pixels,
            width,
            height,
            mondrian_effects::EffectGpuSpatialPass::ChromaticAberrationRemoval { grade },
            1,
        );
        let mut expected = pixels.clone();
        assert!(chromatic_aberration_removal_frame_f32(
            &mut expected,
            width as usize,
            height as usize,
            grade
        ));
        for index in [0, 4, 8, 4 * 9, 4 * 9 + 8, 8 * 9, 8 * 9 + 4, 8 * 9 + 8] {
            assert_eq!(actual[index][0].to_bits(), pixels[index][0].to_bits());
        }
        let max_error = max_spatial_float_error(&expected, &actual);
        eprintln!("RES-08 clamp, max absolute error = {max_error:.9e}");
        assert!(max_error <= 2.0e-4, "{max_error}");
    }

    #[tokio::test]
    async fn gpu_basic_3d_runs_in_the_layer_compositor() {
        use mondrian_effects::{basic_3d_frame_f32, Basic3DGrade, EffectRenderOp};

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping Basic 3D compositor parity: no GPU adapter available");
            return;
        };
        let (width, height) = (35, 21);
        let pixels = spatial_test_pixels(width, height);
        let frame = spatial_test_frame(&pixels, width, height);
        for (swivel, tilt, distance, specular) in [
            (32.0, -27.0, 8.0, 0.0),
            (-20.0, 26.0, -7.0, 0.75),
            (158.0, -15.0, 0.0, 1.0),
        ] {
            let grade = Basic3DGrade::new(swivel, tilt, distance, specular).unwrap();
            let pipeline = spatial_test_pipeline(EffectRenderOp::Basic3D { grade });
            let entries = [GpuCompositeSpatialLayer {
                layer: spatial_test_layer(&frame),
                spatial_plan: Some(&pipeline),
            }];
            let (actual, diagnostics) = readback_spatial_test_composite_with_diagnostics(
                &context,
                &entries,
                width,
                height,
                WorkingColorSpace::LinearRec709,
            );
            let mut expected = pixels.clone();
            assert!(basic_3d_frame_f32(
                &mut expected,
                width as usize,
                height as usize,
                grade
            ));
            assert!(max_spatial_float_error(&pixels, &actual) > 0.1);
            assert!(actual.iter().any(|pixel| pixel[3] == 0.0));
            assert!(actual.iter().any(|pixel| pixel[3] > 0.0));
            assert_eq!(diagnostics.spatial_effect_layers, 1);
            assert_eq!(diagnostics.spatial_effect_passes, 1);
            assert_eq!(diagnostics.spatial_scratch_textures_peak, 2);
            assert_eq!(diagnostics.spatial_point_stages, 0);
            assert_eq!(diagnostics.cpu_fallback_composites, 0);
            let max_error = max_spatial_float_error(&expected, &actual);
            eprintln!("TRF-07 compositor {grade:?}, max absolute error = {max_error:.9e}");
            assert!(max_error <= 2.0e-4, "{grade:?}: {max_error}");
        }
    }

    #[tokio::test]
    async fn gpu_basic_3d_preserves_soft_coverage_and_clips_depth() {
        use mondrian_effects::{basic_3d_frame_f32, Basic3DGrade, EffectGpuSpatialPass};

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping Basic 3D Float32 parity: no GPU adapter available");
            return;
        };
        let executor = crate::spatial_pass_gpu::EffectSpatialGpu::new(&context.device);
        for (width, height, swivel, tilt, distance, specular, passthrough) in [
            (31, 17, 0.0, 0.0, 0.0, 0.0, true),
            (31, 17, 90.0, 0.0, 3.0, 1.0, true),
            (31, 17, 12.0, -9.0, -31.0, 1.0, true),
            (1, 17, 12.0, -9.0, 1.0, 1.0, true),
            (31, 17, -32.0, 29.0, 4.0, 0.9, false),
            (31, 17, 0.0, 0.0, -30.9999, 1.0, false),
            (31, 17, 0.0, 0.0, -32.0, 1.0, false),
        ] {
            let grade = Basic3DGrade::new(swivel, tilt, distance, specular).unwrap();
            let mut pixels = spatial_test_pixels(width, height);
            for (index, pixel) in pixels.iter_mut().enumerate() {
                pixel[0] = pixel[0] * 1.5 + 0.2;
                pixel[3] = [0.0, 1.0 / 65_535.0, 0.2, 0.65, 1.0][index % 5];
            }
            let mut expected = pixels.clone();
            assert!(basic_3d_frame_f32(
                &mut expected,
                width as usize,
                height as usize,
                grade
            ));
            let actual = readback_spatial_stage(
                &context,
                &executor,
                &pixels,
                width,
                height,
                EffectGpuSpatialPass::Basic3D { grade },
                1,
            );
            let max_error = max_spatial_float_error(&expected, &actual);
            eprintln!(
                "TRF-07 raw {width}x{height} {grade:?}, max absolute error = {max_error:.9e}"
            );
            if passthrough {
                for (before, after) in pixels.iter().zip(&actual) {
                    assert_eq!(before.map(f32::to_bits), after.map(f32::to_bits));
                }
            } else if distance < -30.0 {
                assert!(actual.iter().all(|pixel| *pixel == [0.0; 4]));
            } else {
                assert!(actual
                    .iter()
                    .any(|pixel| (0.0..1.0).contains(&pixel[3]) && pixel[3] > 0.0));
                assert!(actual.iter().any(|pixel| pixel[0] > 1.0));
                assert!(actual.contains(&[0.0; 4]));
            }
            assert!(max_error <= 2.0e-4, "{grade:?}: {max_error}");
        }
    }

    #[tokio::test]
    async fn gpu_camera_view_fills_only_missed_rays_on_a_transparent_layer() {
        use mondrian_core::{PropertyHost, PropertyMutation, PropertyValue, TimelineTime};
        use mondrian_effects::{
            compile_clip_effect_graph, instantiate_effect_node, lower_effect_graph_to_gpu_pipeline,
            EffectType,
        };

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping Camera View compositor parity: no GPU adapter available");
            return;
        };
        let (width, height) = (31, 19);
        let pixels = vec![[-0.25, 0.75, 1.25, 0.0]; (width * height) as usize];
        let working_space = WorkingColorSpace::LinearRec709;
        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            alpha: ColorFrameAlpha::StraightCoverage,
            width,
            height,
            color_space: working_space,
            data: pixels.clone(),
        });
        let kind = EffectType::CameraView;
        let mut effect = instantiate_effect_node(kind.clone()).unwrap();
        let fill_color = Color { r: 0.25, g: 0.5, b: 0.75, a: 0.5 };
        for (name, value) in [
            ("longitude", PropertyValue::Float(27.0)),
            ("latitude", PropertyValue::Float(-18.0)),
            ("roll", PropertyValue::Float(14.0)),
            ("distance", PropertyValue::Float(2.0)),
            ("fill_color", PropertyValue::Color(fill_color)),
            ("fill_alpha", PropertyValue::Float(0.5)),
        ] {
            effect
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: kind.property_path(name),
                    value,
                })
                .unwrap();
        }
        let compiled =
            compile_clip_effect_graph(&[effect], &[], TimelineTime::ZERO, working_space).unwrap();
        let pipeline = lower_effect_graph_to_gpu_pipeline(&compiled).unwrap();
        let entries = [GpuCompositeSpatialLayer {
            layer: spatial_test_layer(&frame),
            spatial_plan: Some(&pipeline),
        }];
        let (actual, diagnostics) = readback_spatial_test_composite_with_diagnostics(
            &context,
            &entries,
            width,
            height,
            working_space,
        );
        let expected = mondrian_effects::apply_compiled_effect_graph_rgba_f32(
            mondrian_effects::EffectFloatFrameView::new(
                mondrian_effects::EffectFrameExtent::new(width, height),
                mondrian_effects::EffectValueFormat::new(
                    mondrian_effects::EffectWorkingPrecision::Float32,
                    EffectColorDomain::SceneLinearRgb,
                    ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &pixels,
            ),
            &compiled,
            0,
        )
        .unwrap();
        assert_eq!(actual[0], [0.25, 0.5, 0.75, 0.25]);
        assert_eq!(actual[(height / 2 * width + width / 2) as usize], [0.0; 4]);
        assert_eq!(diagnostics.spatial_effect_layers, 1);
        assert_eq!(diagnostics.spatial_effect_passes, 1);
        assert_eq!(diagnostics.spatial_scratch_textures_peak, 2);
        assert_eq!(diagnostics.spatial_point_stages, 0);
        assert_eq!(diagnostics.cpu_fallback_composites, 0);
        let max_error = max_spatial_float_error(&expected, &actual);
        eprintln!("TRF-08 transparent compositor, max absolute error = {max_error:.9e}");
        assert!(max_error <= 1.0e-4, "{max_error}");
    }

    #[tokio::test]
    async fn gpu_camera_view_matches_soft_alpha_and_depth_on_the_raw_pass() {
        use mondrian_effects::{camera_view_frame_f32, CameraViewGrade, EffectGpuSpatialPass};

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping Camera View Float32 parity: no GPU adapter available");
            return;
        };
        let executor = crate::spatial_pass_gpu::EffectSpatialGpu::new(&context.device);
        let fill = [0.25, 0.5, 0.75, 0.5];
        for (width, height, angles, focal, distance, zoom, alpha) in [
            (31, 17, [0.0; 3], 500.0, 1.0, 100.0, 0.5),
            (31, 17, [29.0, -16.0, 13.0], 470.0, 1.3, 114.0, 0.5),
            (31, 17, [-16.0, 23.0, -32.0], 740.0, 1.1, 89.0, 0.0),
            (31, 17, [90.0, 0.0, 0.0], 500.0, 1.0, 100.0, 0.5),
            (31, 17, [145.0, 16.0, -23.0], 500.0, 1.0, 100.0, 0.0),
            (31, 17, [60.0, 0.0, 0.0], 250.0, 1.0, 100.0, 0.5),
            (9, 5, [0.0; 3], 500.0, 1.125, 100.0, 0.5),
            (1, 17, [0.0; 3], 500.0, 0.5, 100.0, 0.5),
        ] {
            let grade = CameraViewGrade::new(angles, focal, distance, zoom, fill, alpha).unwrap();
            let mut pixels = spatial_test_pixels(width, height);
            for (index, pixel) in pixels.iter_mut().enumerate() {
                pixel[0] = pixel[0] * 1.5 + 0.2;
                pixel[2] -= 0.75;
                pixel[3] = [0.0, 1.0 / 65_535.0, 0.2, 0.65, 1.0][index % 5];
            }
            let mut expected = pixels.clone();
            assert!(camera_view_frame_f32(
                &mut expected,
                width as usize,
                height as usize,
                grade
            ));
            let actual = readback_spatial_stage(
                &context,
                &executor,
                &pixels,
                width,
                height,
                EffectGpuSpatialPass::CameraView { grade },
                1,
            );
            if grade.is_identity() {
                for (before, after) in pixels.iter().zip(&actual) {
                    assert_eq!(before.map(f32::to_bits), after.map(f32::to_bits));
                }
            }
            if angles[0] == 90.0 {
                assert!(actual.iter().all(|&pixel| pixel == [0.25, 0.5, 0.75, 0.25]));
            }
            if angles[0] > 90.0 {
                assert!(actual.iter().all(|&pixel| pixel == [0.0; 4]));
            }
            let max_error = max_spatial_float_error(&expected, &actual);
            eprintln!(
                "TRF-08 raw {width}x{height} {grade:?}, max absolute error = {max_error:.9e}"
            );
            if max_error > 1.0e-4 {
                for (index, (cpu, gpu)) in expected.iter().zip(&actual).enumerate() {
                    if cpu.iter().zip(gpu).any(|(a, b)| (a - b).abs() > 1.0e-4) {
                        eprintln!(
                            "Camera View mismatch at ({}, {}): CPU {cpu:?}, GPU {gpu:?}",
                            index % width as usize,
                            index / width as usize
                        );
                    }
                }
            }
            assert!(max_error <= 1.0e-4, "{grade:?}: {max_error}");
        }
    }

    #[tokio::test]
    async fn gpu_blanking_fill_matches_all_modes_on_the_raw_stage() {
        use mondrian_effects::{
            blanking_fill_frame_f32, BlankingFillGrade, BlankingFillMode, BlankingFillParameters,
        };

        let context = crate::GpuContext::new()
            .await
            .expect("Blanking Fill needs an actual GPU adapter");
        let executor = crate::spatial_pass_gpu::EffectSpatialGpu::new(&context.device);
        let (width, height) = (65, 47);
        let mut pixels = spatial_test_pixels(width, height);
        for (index, pixel) in pixels.iter_mut().enumerate() {
            pixel[0] += 0.8;
            pixel[2] -= 0.7;
            pixel[3] = [0.0, 0.015625, 0.25, 0.5, 0.875, 1.0][index % 6];
        }
        for (mode, blur, fade) in [
            (BlankingFillMode::Stretch, 1.0, 0.25),
            (BlankingFillMode::Zoom, 0.65, 0.125),
            (BlankingFillMode::Manual, 0.0, 0.0),
            (BlankingFillMode::Mirror, 0.8, 0.35),
            (BlankingFillMode::Warp, 1.0, 1.0),
        ] {
            let grade = BlankingFillGrade::new(BlankingFillParameters {
                mode,
                crop: [0.23, 0.12, 0.15, 0.18],
                crop_split: [true; 2],
                center: true,
                source_zoom: 1.125,
                expand: 1.5,
                aspect: 1.3,
                blend_edges: 0.75,
                background_blur: blur,
                fade,
                fade_color: [0.5, 0.75, 0.25, 0.5],
                shadow_strength: 0.6,
                shadow_angle: -23.0,
                shadow_distance: 0.12,
                shadow_blur: 0.55,
                shadow_color: [0.1, 0.2, 0.3, 0.5],
                stretch: [-0.08, -0.12, -0.15, -0.06],
                stretch_split: [true; 2],
            })
            .unwrap();
            let mut expected = pixels.clone();
            assert!(blanking_fill_frame_f32(
                &mut expected,
                width as usize,
                height as usize,
                grade
            ));
            let actual = readback_spatial_stage(
                &context,
                &executor,
                &pixels,
                width,
                height,
                EffectGpuSpatialPass::BlankingFill { grade },
                if mode == BlankingFillMode::Warp { 1 } else { 2 },
            );
            assert!(max_spatial_float_error(&pixels, &actual) > 0.1);
            assert!(actual.iter().any(|pixel| pixel[0] > 1.0));
            let max_error = max_spatial_float_error(&expected, &actual);
            eprintln!("TRF-11 raw {mode:?}, max absolute error = {max_error:.9e}");
            assert!(max_error <= 1.0e-4, "{mode:?}: {max_error}");
        }
    }

    #[tokio::test]
    async fn gpu_blanking_fill_composites_the_authored_crop_on_the_original_grid() {
        use mondrian_core::{PropertyHost, PropertyMutation, PropertyValue, TimelineTime};
        use mondrian_effects::{
            compile_clip_effect_graph, instantiate_effect_node, lower_effect_graph_to_gpu_pipeline,
            EffectType,
        };

        let context = crate::GpuContext::new()
            .await
            .expect("Blanking Fill needs an actual GPU adapter");
        let (width, height) = (41, 27);
        let working_space = WorkingColorSpace::LinearRec709;
        let pixels = spatial_test_pixels(width, height);
        let frame = spatial_test_frame(&pixels, width, height);
        for (mode, passes) in [("stretch", 2_u32), ("warp", 1)] {
            let kind = EffectType::BlankingFill;
            let mut effect = instantiate_effect_node(kind.clone()).unwrap();
            for (field, value) in [
                ("crop_left", PropertyValue::Float(0.25)),
                ("crop_top", PropertyValue::Float(0.125)),
                ("zoom_mode", PropertyValue::Enum(mode.into())),
                ("fade", PropertyValue::Float(0.5)),
                ("stretch_left", PropertyValue::Float(-0.25)),
            ] {
                effect
                    .apply_property_mutation(PropertyMutation::SetStaticValue {
                        path: kind.property_path(field),
                        value,
                    })
                    .unwrap();
            }
            let compiled =
                compile_clip_effect_graph(&[effect], &[], TimelineTime::ZERO, working_space)
                    .unwrap();
            let pipeline = lower_effect_graph_to_gpu_pipeline(&compiled).unwrap();
            let entries = [GpuCompositeSpatialLayer {
                layer: spatial_test_layer(&frame),
                spatial_plan: Some(&pipeline),
            }];
            let (actual, diagnostics) = readback_spatial_test_composite_with_diagnostics(
                &context,
                &entries,
                width,
                height,
                working_space,
            );
            let expected = mondrian_effects::apply_compiled_effect_graph_rgba_f32(
                mondrian_effects::EffectFloatFrameView::new(
                    mondrian_effects::EffectFrameExtent::new(width, height),
                    mondrian_effects::EffectValueFormat::new(
                        mondrian_effects::EffectWorkingPrecision::Float32,
                        EffectColorDomain::SceneLinearRgb,
                        ColorFrameAlpha::StraightCoverage,
                    ),
                    mondrian_core::ImageSampling::UNIT_SQUARE,
                    &pixels,
                ),
                &compiled,
                0,
            )
            .unwrap();
            assert_eq!(actual.len(), pixels.len());
            assert!(max_spatial_float_error(&pixels, &actual) > 0.1);
            let centre = (height / 2 * width + width / 2) as usize;
            assert_eq!(actual[centre], pixels[centre]);
            assert_eq!(diagnostics.spatial_effect_layers, 1);
            assert_eq!(diagnostics.spatial_effect_passes, u64::from(passes));
            assert_eq!(diagnostics.spatial_scratch_textures_peak, passes + 1);
            assert_eq!(diagnostics.spatial_point_stages, 0);
            assert_eq!(diagnostics.cpu_fallback_composites, 0);
            let max_error = max_spatial_float_error(&expected, &actual);
            eprintln!("TRF-11 compositor {mode}, max absolute error = {max_error:.9e}");
            assert!(max_error <= 1.0e-4, "{mode}: {max_error}");
        }
    }

    #[tokio::test]
    async fn spatial_chain_runs_on_a_single_layer_before_compositing() {
        use mondrian_effects::{EffectRenderOp, TwirlGrade};

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping spatial chain single layer test: no GPU adapter available");
            return;
        };
        let width = 9;
        let height = 7;
        let pixels = spatial_test_pixels(width, height);
        let frame = spatial_test_frame(&pixels, width, height);
        let grade = TwirlGrade::new(45.0, 80.0, [width as f32 * 0.5, height as f32 * 0.5])
            .expect("valid twirl grade");
        let pipeline = spatial_test_pipeline(EffectRenderOp::Twirl { grade });
        let layer = spatial_test_layer(&frame);
        let entries = [GpuCompositeSpatialLayer { layer, spatial_plan: Some(&pipeline) }];
        let (actual, diagnostics) = readback_spatial_test_composite_with_diagnostics(
            &context,
            &entries,
            width,
            height,
            WorkingColorSpace::LinearRec709,
        );

        let mut expected = pixels;
        assert!(mondrian_effects::twirl_frame_f32(
            &mut expected,
            width as usize,
            height as usize,
            grade
        ));
        assert_spatial_chain_matches(&expected, &actual);
        assert_eq!(diagnostics.spatial_effect_layers, 1);
        assert_eq!(diagnostics.spatial_effect_passes, 1);
        assert_eq!(diagnostics.spatial_point_stages, 0);
        assert_eq!(diagnostics.spatial_scratch_textures_peak, 2);
        assert_eq!(diagnostics.cpu_fallback_composites, 0);
    }

    #[tokio::test]
    async fn spatial_chain_runs_a_four_pass_kernel_in_one_chain() {
        use mondrian_effects::{
            EffectRenderOp, MinimaxChannel, MinimaxDirection, MinimaxGrade, MinimaxOperation,
        };

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping spatial chain four pass test: no GPU adapter available");
            return;
        };
        let width = 11;
        let height = 9;
        let pixels = spatial_test_pixels(width, height);
        let frame = spatial_test_frame(&pixels, width, height);
        let grade = MinimaxGrade::new(
            1.0,
            MinimaxOperation::MinimumThenMaximum,
            MinimaxChannel::Color,
            MinimaxDirection::Both,
            0.0,
        )
        .expect("valid minimax grade");
        let pipeline = spatial_test_pipeline(EffectRenderOp::Minimax { grade });
        let layer = spatial_test_layer(&frame);
        let entries = [GpuCompositeSpatialLayer { layer, spatial_plan: Some(&pipeline) }];
        let (actual, diagnostics) = readback_spatial_test_composite_with_diagnostics(
            &context,
            &entries,
            width,
            height,
            WorkingColorSpace::LinearRec709,
        );

        let mut expected = pixels;
        assert!(mondrian_effects::minimax_frame_f32(
            &mut expected,
            width as usize,
            height as usize,
            &grade
        ));
        assert_spatial_chain_matches(&expected, &actual);
        assert_eq!(diagnostics.spatial_effect_layers, 1);
        // Both directions over the RGB mask is four texture passes, and the
        // scratch pool must hand the kernel four rotating rasters plus one.
        assert_eq!(diagnostics.spatial_effect_passes, 4);
        assert_eq!(diagnostics.spatial_scratch_textures_peak, 5);
        assert_eq!(diagnostics.cpu_fallback_composites, 0);
    }

    #[tokio::test]
    async fn spatial_chain_runs_point_spatial_point_stages_in_order() {
        use mondrian_effects::{
            compile_reference_render_graph, EffectFloatFrameView, EffectFrameExtent,
            EffectGraphBuilderState, EffectRenderOp, EffectValueFormat, EffectWorkingPrecision,
            InvertChannels, TwirlGrade,
        };

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping point/spatial/point chain test: no GPU adapter available");
            return;
        };
        let width = 11;
        let height = 9;
        let pixels = spatial_test_pixels(width, height);
        let frame = spatial_test_frame(&pixels, width, height);
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(EffectRenderOp::BrightnessContrast {
            grade: mondrian_effects::BrightnessContrastGrade::new(40.0, 20.0, false)
                .expect("valid brightness contrast grade"),
        });
        builder.append_unary(EffectRenderOp::Twirl {
            grade: TwirlGrade::new(-35.0, 70.0, [width as f32 * 0.4, height as f32 * 0.6])
                .expect("valid twirl grade"),
        });
        builder.append_unary(EffectRenderOp::Invert {
            channels: InvertChannels::Rgb,
            blend_with_original: 0.25,
        });
        let graph = compile_reference_render_graph(builder.finish()).expect("valid graph");
        let pipeline = mondrian_effects::lower_effect_graph_to_gpu_pipeline(&graph)
            .expect("lowered GPU effect pipeline");
        assert_eq!(pipeline.stages().len(), 3);
        assert_eq!(pipeline.point_operation_count(), 2);
        assert_eq!(pipeline.spatial_stage_count(), 1);

        let layer = spatial_test_layer(&frame);
        let entries = [GpuCompositeSpatialLayer { layer, spatial_plan: Some(&pipeline) }];
        let (actual, diagnostics) = readback_spatial_test_composite_with_diagnostics(
            &context,
            &entries,
            width,
            height,
            WorkingColorSpace::LinearRec709,
        );

        let expected = mondrian_effects::apply_compiled_effect_graph_rgba_f32(
            EffectFloatFrameView::new(
                EffectFrameExtent::new(width, height),
                EffectValueFormat::new(
                    EffectWorkingPrecision::Float32,
                    mondrian_effects::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::Opaque,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &frame.rgba_f32().data,
            ),
            &graph,
            0,
        )
        .expect("CPU float reference");
        assert_spatial_chain_matches(expected.pixels(), &actual);
        assert_eq!(diagnostics.spatial_point_stages, 2);
        assert_eq!(diagnostics.spatial_effect_passes, 1);
        assert_eq!(diagnostics.spatial_effect_layers, 1);
    }

    #[tokio::test]
    async fn spatial_chain_glow_runs_on_a_multi_layer_request() {
        use mondrian_effects::{
            EffectGpuSpatialPass, EffectRenderOp, GlowBasedOn, GlowColors, GlowComposite,
            GlowDimensions, GlowGrade, GlowOperation,
        };

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping glow spatial chain test: no GPU adapter available");
            return;
        };
        let width = 11;
        let height = 9;
        let pixels = spatial_test_pixels(width, height);
        let frame = spatial_test_frame(&pixels, width, height);
        let grade = GlowGrade::new(
            GlowBasedOn::Color,
            153.0,
            4.0,
            60.0,
            GlowComposite::OnTop,
            GlowOperation::Screen,
            GlowColors::Original,
            1.0,
            0.0,
            0.5,
            [1.0, 1.0, 1.0],
            [0.0, 0.0, 0.0],
            GlowDimensions::Both,
            [0.2126, 0.7152, 0.0722],
        )
        .expect("valid glow grade");
        assert_eq!(
            crate::spatial_pass_gpu::EffectSpatialGpu::texture_pass_count(
                &EffectGpuSpatialPass::Glow { grade }
            )
            .expect("glow pass count"),
            6
        );
        let pipeline = spatial_test_pipeline(EffectRenderOp::Glow { grade });
        let base = GpuCompositeLayer {
            source: GpuCompositeLayerSource::SolidColor(Color {
                r: 0.05,
                g: 0.10,
                b: 0.15,
                a: 1.0,
            }),
            opacity: 1.0,
            blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: None,
            frame_seed: 0,
        };
        let glow_layer = spatial_test_layer(&frame);
        let entries = [
            GpuCompositeSpatialLayer { layer: base, spatial_plan: None },
            GpuCompositeSpatialLayer { layer: glow_layer, spatial_plan: Some(&pipeline) },
        ];
        let (actual, diagnostics) = readback_spatial_test_composite_with_diagnostics(
            &context,
            &entries,
            width,
            height,
            WorkingColorSpace::LinearRec709,
        );

        let mut expected = pixels;
        assert!(mondrian_effects::glow_frame_f32(
            &mut expected,
            width as usize,
            height as usize,
            &grade
        ));
        for pixel in expected.iter_mut() {
            *pixel = spatial_test_normal_over_opaque([0.05, 0.10, 0.15, 1.0], *pixel);
        }
        assert_spatial_chain_matches(&expected, &actual);
        assert_eq!(diagnostics.spatial_effect_layers, 1);
        assert_eq!(diagnostics.spatial_effect_passes, 6);
        assert!(diagnostics.spatial_scratch_textures_peak >= 7);
    }

    #[tokio::test]
    async fn spatial_chain_reports_a_directional_blur_unsupported_pass() {
        use mondrian_effects::EffectRenderOp;

        let width = 9;
        let height = 7;
        let pipeline = spatial_test_pipeline(EffectRenderOp::DirectionalBlur {
            length: 4.0,
            direction_degrees: 30.0,
        });
        let blocker = crate::spatial_driver::gpu_spatial_chain_plan(&pipeline, width, height)
            .expect_err("directional blur has no GPU spatial math");
        assert!(
            matches!(
                &blocker,
                crate::spatial_driver::GpuSpatialChainBlocker::UnsupportedPass { pass, .. }
                    if *pass == "directional_blur"
            ),
            "{blocker:?}"
        );
        assert_eq!(blocker.code(), "spatial_pass_unsupported");

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping directional blur admission test: no GPU adapter available");
            return;
        };
        let pixels = spatial_test_pixels(width, height);
        let frame = spatial_test_frame(&pixels, width, height);
        let layer = spatial_test_layer(&frame);
        let entries = [GpuCompositeSpatialLayer { layer, spatial_plan: Some(&pipeline) }];
        let compositor = GpuFrameCompositor::new(&context.device).expect("GPU compositor");
        let mut ids = GpuColorFrameIdAllocator::new(1).expect("frame id allocator");
        let mut table = GpuColorFrameResourceTable::new();
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian-test-gpu-spatial-chain-admission"),
        });
        let error = compositor
            .record_with_spatial_effect_chains(
                &context.device,
                &context.queue,
                &mut encoder,
                &mut ids,
                &mut table,
                None,
                GpuCompositeSpatialRequest {
                    width,
                    height,
                    working_color_space: WorkingColorSpace::LinearRec709,
                    layers: &entries,
                },
            )
            .err()
            .expect("an attached unsupported chain must fail closed");
        assert!(
            matches!(
                &error,
                GpuCompositeError::SpatialChainFailed { pass, .. } if *pass == "directional_blur"
            ),
            "{error:?}"
        );
        assert_eq!(
            compositor.spatial_scratch_diagnostics().retained_textures,
            0
        );
    }

    #[test]
    fn gpu_dust_scratches_plan_past_the_gpu_cap_falls_back_to_cpu() {
        use mondrian_effects::{DustScratchesGrade, EffectRenderOp};

        let width = 9;
        let height = 7;
        let wide = spatial_test_pipeline(EffectRenderOp::DustScratches {
            grade: DustScratchesGrade::new(4.0, 0.0, 0.0).expect("valid dust & scratches grade"),
        });
        let blocker = crate::spatial_driver::gpu_spatial_chain_plan(&wide, width, height)
            .expect_err("a radius past the GPU cap has no spatial plan");
        assert!(
            matches!(
                &blocker,
                crate::spatial_driver::GpuSpatialChainBlocker::UnsupportedPass { pass, .. }
                    if *pass == "dust_scratches_radius"
            ),
            "{blocker:?}"
        );
        assert_eq!(blocker.code(), "spatial_pass_unsupported");

        // A refused chain always chooses the CPU path and keeps the blocker as the
        // reason, so the layer still renders through the fallback instead of
        // failing the frame.
        let choice = crate::choose_spatial_chain_for_plan(
            Err(&blocker),
            width,
            height,
            true,
            crate::SpatialChainPreference::Auto,
        );
        assert!(!choice.use_gpu, "a blocked chain must fall back to the CPU");
        assert!(
            matches!(
                choice.reason,
                crate::SpatialChainChoiceReason::PlanUnavailable(_)
            ),
            "{:?}",
            choice.reason
        );

        // The CPU plan for the same authored state still executes, and it is what
        // the fallback renders: the same frame in, the same bits out.
        let grade = DustScratchesGrade::new(4.0, 0.0, 0.0).expect("valid dust & scratches grade");
        let pixels = spatial_test_pixels(width, height);
        let mut cpu = pixels.clone();
        assert!(mondrian_effects::dust_scratches_frame_f32(
            &mut cpu, width, height, &grade
        ));
        let mut again = pixels.clone();
        assert!(mondrian_effects::dust_scratches_frame_f32(
            &mut again, width, height, &grade
        ));
        for (index, (left, right)) in cpu.iter().zip(again.iter()).enumerate() {
            assert_eq!(
                left.map(f32::to_bits),
                right.map(f32::to_bits),
                "pixel {index}"
            );
        }

        // Radius three is still admitted, which is the boundary the cap names.
        let narrow = spatial_test_pipeline(EffectRenderOp::DustScratches {
            grade: DustScratchesGrade::new(3.0, 0.0, 0.0).expect("valid dust & scratches grade"),
        });
        let plan = crate::spatial_driver::gpu_spatial_chain_plan(&narrow, width, height)
            .expect("radius three is inside the GPU cap");
        assert_eq!(plan.spatial_passes, 1);
    }

    #[test]
    fn spatial_chain_rejects_a_layer_carrying_both_plans() {
        use mondrian_effects::{EffectRenderOp, TwirlGrade};

        let width = 5;
        let height = 4;
        let pixels = spatial_test_pixels(width, height);
        let frame = spatial_test_frame(&pixels, width, height);
        let pipeline = spatial_test_pipeline(EffectRenderOp::Twirl {
            grade: TwirlGrade::new(30.0, 60.0, [2.0, 2.0]).expect("valid twirl grade"),
        });
        let mut builder = mondrian_effects::EffectGraphBuilderState::new();
        builder.append_unary(EffectRenderOp::BrightnessContrast {
            grade: mondrian_effects::BrightnessContrastGrade::new(20.0, 10.0, false)
                .expect("valid brightness contrast grade"),
        });
        let graph = mondrian_effects::compile_reference_render_graph(builder.finish())
            .expect("valid graph");
        let point_plan =
            mondrian_effects::lower_effect_graph_to_gpu_plan(&graph).expect("supported point plan");

        let mut layer = spatial_test_layer(&frame);
        layer.effect_plan = Some(&point_plan);
        let entries = [GpuCompositeSpatialLayer { layer, spatial_plan: Some(&pipeline) }];
        let request = GpuCompositeSpatialRequest {
            width,
            height,
            working_color_space: WorkingColorSpace::LinearRec709,
            layers: &entries,
        };
        let error = validate_spatial_request(&request)
            .expect_err("a layer cannot carry both a point and a spatial plan");
        assert!(
            matches!(error, GpuCompositeError::SpatialPlanWithPointPlan),
            "{error:?}"
        );

        let solid = GpuCompositeLayer {
            source: GpuCompositeLayerSource::SolidColor(Color { r: 1.0, g: 0.5, b: 0.25, a: 1.0 }),
            opacity: 1.0,
            blend_policy: mondrian_effects::EffectPassBlendPolicy::Blend(BlendMode::Normal),
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: None,
            frame_seed: 0,
        };
        let entries = [GpuCompositeSpatialLayer { layer: solid, spatial_plan: Some(&pipeline) }];
        let request = GpuCompositeSpatialRequest {
            width,
            height,
            working_color_space: WorkingColorSpace::LinearRec709,
            layers: &entries,
        };
        let error = validate_spatial_request(&request)
            .expect_err("a solid colour cannot carry a spatial chain");
        assert!(
            matches!(
                error,
                GpuCompositeError::SpatialPlanOnUnsupportedSource { source_kind }
                    if source_kind == "solid_color"
            ),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn spatial_chain_disables_the_opaque_normal_fast_path() {
        use mondrian_effects::{DentEdge, DentGrade, DentKind, EffectRenderOp};

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping spatial chain opaque fast path test: no GPU adapter available");
            return;
        };
        let width = 10;
        let height = 8;
        let pixels = spatial_test_pixels(width, height);
        let frame = spatial_test_frame(&pixels, width, height);
        let layer = spatial_test_layer(&frame);
        let plain = [GpuCompositeSpatialLayer { layer, spatial_plan: None }];
        let (_, fast) = readback_spatial_test_composite_with_diagnostics(
            &context,
            &plain,
            width,
            height,
            WorkingColorSpace::LinearRec709,
        );
        assert_eq!(fast.opaque_normal_single_accumulator_composites, 1);
        assert_eq!(fast.spatial_effect_layers, 0);

        let grade = DentGrade::new(
            0.4,
            0.6,
            [width as f32 * 0.5, height as f32 * 0.5],
            DentKind::Dent,
            DentEdge::Smooth,
        )
        .expect("valid dent grade");
        let pipeline = spatial_test_pipeline(EffectRenderOp::Dent { grade });
        let layer = spatial_test_layer(&frame);
        let dented = [GpuCompositeSpatialLayer { layer, spatial_plan: Some(&pipeline) }];
        let (_, slow) = readback_spatial_test_composite_with_diagnostics(
            &context,
            &dented,
            width,
            height,
            WorkingColorSpace::LinearRec709,
        );
        assert_eq!(slow.opaque_normal_single_accumulator_composites, 0);
        assert_eq!(slow.avoided_accumulator_sample_pixels, 0);
        assert_eq!(slow.spatial_effect_layers, 1);
    }
}
