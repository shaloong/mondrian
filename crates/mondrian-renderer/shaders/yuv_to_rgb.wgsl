// YUV420P → RGBA 转换 (BT.709)
// Y plane: @binding(0), U plane: @binding(1), V plane: @binding(2)

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOutput {
    // 全屏三角形（无需顶点缓冲区）
    let x = f32((vi & 1u) * 2u) - 1.0;
    let y = f32((vi >> 1u) * 2u) - 1.0;
    return VertexOutput(
        vec4(x, y, 0.0, 1.0),
        vec2((x + 1.0) * 0.5, (1.0 - y) * 0.5),
    );
}

@group(0) @binding(0) var y_tex:  texture_2d<f32>;
@group(0) @binding(1) var u_tex:  texture_2d<f32>;
@group(0) @binding(2) var v_tex:  texture_2d<f32>;
@group(0) @binding(3) var samp:   sampler;

// BT.709 YCbCr → RGB 变换矩阵（Full Range）
const M: mat3x3<f32> = mat3x3<f32>(
    vec3( 1.000,  1.000,  1.000),
    vec3( 0.000, -0.187,  1.856),
    vec3( 1.575, -0.468,  0.000),
);

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let y  = textureSample(y_tex, samp, in.uv).r - 0.0625;
    let cb = textureSample(u_tex, samp, in.uv).r - 0.5;
    let cr = textureSample(v_tex, samp, in.uv).r - 0.5;
    let rgb = M * vec3(y, cb, cr);
    return vec4(clamp(rgb, vec3(0.0), vec3(1.0)), 1.0);
}
