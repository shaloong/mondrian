// Alpha 预乘混合 (Porter-Duff "Over")
// src: 上层，dst: 下层

struct Uniforms {
    opacity: f32,
    blend_mode: u32,
    _padding: vec2<f32>,
}

@group(0) @binding(0) var src_tex: texture_2d<f32>;
@group(0) @binding(1) var dst_tex: texture_2d<f32>;
@group(0) @binding(2) var samp:    sampler;
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

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    var src = textureSample(src_tex, samp, in.uv);
    let dst = textureSample(dst_tex, samp, in.uv);
    src.a *= u.opacity;

    // Normal blend (Porter-Duff Over)
    let a_out = src.a + dst.a * (1.0 - src.a);
    if a_out < 0.0001 {
        return vec4(0.0);
    }
    let rgb = (src.rgb * src.a + dst.rgb * dst.a * (1.0 - src.a)) / a_out;
    return vec4(rgb, a_out);
}
