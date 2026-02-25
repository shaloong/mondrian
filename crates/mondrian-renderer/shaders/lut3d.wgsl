// 3D LUT 调色 (支持 16³/33³/65³)
@group(0) @binding(0) var input_tex: texture_2d<f32>;
@group(0) @binding(1) var lut_tex:   texture_3d<f32>;
@group(0) @binding(2) var samp:      sampler;

struct Uniforms { intensity: f32, lut_size: f32, _pad: vec2<f32> }
@group(1) @binding(0) var<uniform> u: Uniforms;

struct VertexOutput {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
}
@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOutput {
    let x = f32((vi & 1u) * 2u) - 1.0;
    let y = f32((vi >> 1u) * 2u) - 1.0;
    return VertexOutput(vec4(x,y,0.0,1.0), vec2((x+1.0)*0.5,(1.0-y)*0.5));
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let orig = textureSample(input_tex, samp, in.uv);
    // 将 RGB 映射到 LUT 3D 纹理坐标（三线性插值）
    let scale = (u.lut_size - 1.0) / u.lut_size;
    let bias  = 0.5 / u.lut_size;
    let coord = orig.rgb * scale + bias;
    let graded = textureSample(lut_tex, samp, coord).rgb;
    let result = mix(orig.rgb, graded, u.intensity);
    return vec4(result, orig.a);
}
