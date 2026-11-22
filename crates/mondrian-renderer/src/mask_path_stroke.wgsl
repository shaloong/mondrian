struct StrokeUniform {
    ink: vec4<f32>,
    profile: vec4<f32>,
    pixel_scale: vec2<f32>,
    pixel_origin: vec2<u32>,
    control: vec4<u32>,
};

struct Node {
    bounds: vec4<f32>,
    metadata: vec4<u32>, // escape index, first primitive, count (zero for branch), reserved
};

@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var<uniform> stroke: StrokeUniform;
@group(0) @binding(2) var<storage, read> nodes: array<Node>;
@group(0) @binding(3) var<storage, read> primitives: array<vec4<f32>>;

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let positions = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    return vec4(positions[index], 0.0, 1.0);
}

fn primitive_coverage(p: vec2<f32>, endpoints: vec4<f32>) -> f32 {
    let a = endpoints.xy;
    let b = endpoints.zw;
    let d = b - a;
    let len2 = dot(d, d);
    var t = 0.0;
    if len2 >= 1.17549435e-38 { // f32::MIN_POSITIVE
        t = clamp(dot(p - a, d) / len2, 0.0, 1.0);
    }
    let q = clamp(a + t * d, min(a, b), max(a, b));
    let dist = length(p - q);
    if dist > stroke.profile.x { return 0.0; }
    return 1.0 - clamp((dist - stroke.profile.y) / stroke.profile.z, 0.0, 1.0);
}

fn coverage_at(p: vec2<f32>) -> f32 {
    var coverage = 0.0;
    var index = 0u;
    while index < stroke.control.y {
        let node = nodes[index];
        if any(p < node.bounds.xy) || any(p > node.bounds.zw) {
            index = node.metadata.x;
        } else if node.metadata.z == 0u {
            index += 1u;
        } else {
            let end = node.metadata.y + node.metadata.z;
            for (var i = node.metadata.y; i < end; i += 1u) {
                coverage = max(coverage, primitive_coverage(p, primitives[i]));
                if coverage >= 1.0 { return 1.0; }
            }
            index = node.metadata.x;
        }
    }
    return coverage;
}

@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let local = vec2<u32>(position.xy);
    let pixel = textureLoad(source, vec2<i32>(local), 0);
    let p = (vec2<f32>(local + stroke.pixel_origin) + vec2(0.5)) * stroke.pixel_scale;
    let alpha = coverage_at(p) * stroke.ink.a;
    if alpha <= 0.0 { return pixel; }
    switch stroke.control.x {
        case 2u: { return vec4(stroke.ink.rgb, alpha); }
        case 3u: { return vec4(pixel.rgb, pixel.a * alpha); }
        default: {
            let source_weight = pixel.a * (1.0 - alpha);
            let out_alpha = alpha + source_weight;
            return vec4((stroke.ink.rgb * alpha + pixel.rgb * source_weight) / out_alpha, out_alpha);
        }
    }
}
