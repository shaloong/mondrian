// Batched composite shader — blends up to 4 layers onto a base texture
// in a single render pass. For groups of Normal-blend layers with
// identical dimensions, this eliminates intermediate ping-pong passes.

struct Uniforms {
    layer_count: u32,   // 1–4, how many layer textures are active
    _pad: vec3<u32>,
}

@group(0) @binding(0) var base_tex: texture_2d<f32>;
@group(0) @binding(1) var layer_0:  texture_2d<f32>;
@group(0) @binding(2) var layer_1:  texture_2d<f32>;
@group(0) @binding(3) var layer_2:  texture_2d<f32>;
@group(0) @binding(4) var layer_3:  texture_2d<f32>;
@group(0) @binding(5) var samp:     sampler;
@group(1) @binding(0) var<uniform> u: Uniforms;

struct VertexOutput {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOutput {
    let x = f32((vi & 1u) * 2u) - 1.0;
    let y = f32((vi >> 1u) * 2u) - 1.0;
    return VertexOutput(vec4(x, y, 0.0, 1.0), vec2((x+1.0)*0.5, (1.0-y)*0.5));
}

fn blend_over(src: vec4<f32>, dst: vec4<f32>) -> vec4<f32> {
    let a_out = src.a + dst.a * (1.0 - src.a);
    if a_out < 0.0001 {
        return vec4(0.0);
    }
    let rgb = (src.rgb * src.a + dst.rgb * dst.a * (1.0 - src.a)) / a_out;
    return vec4(rgb, a_out);
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    var result = textureSample(base_tex, samp, in.uv);

    if u.layer_count >= 1u {
        let l0 = textureSample(layer_0, samp, in.uv);
        result = blend_over(l0, result);
    }
    if u.layer_count >= 2u {
        let l1 = textureSample(layer_1, samp, in.uv);
        result = blend_over(l1, result);
    }
    if u.layer_count >= 3u {
        let l2 = textureSample(layer_2, samp, in.uv);
        result = blend_over(l2, result);
    }
    if u.layer_count >= 4u {
        let l3 = textureSample(layer_3, samp, in.uv);
        result = blend_over(l3, result);
    }

    return result;
}
