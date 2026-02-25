// 可分离高斯模糊（水平 pass）
// 垂直 pass 在第二个渲染 pass 执行，传入 direction=(0,1)

@group(0) @binding(0) var input_tex: texture_2d<f32>;
@group(0) @binding(1) var samp:      sampler;

struct Uniforms {
    radius: f32,
    direction: vec2<f32>,
    _pad: f32,
}
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
    let dims = vec2<f32>(textureDimensions(input_tex));
    let step = u.direction / dims;
    var color = vec4(0.0);
    var weight_sum = 0.0;

    let r = i32(u.radius);
    for (var i = -r; i <= r; i++) {
        let offset = f32(i) * step;
        let w = gaussian_weight(f32(i), u.radius * 0.3333);
        color += textureSample(input_tex, samp, in.uv + offset) * w;
        weight_sum += w;
    }
    return color / weight_sum;
}

fn gaussian_weight(x: f32, sigma: f32) -> f32 {
    return exp(-0.5 * (x / sigma) * (x / sigma));
}
