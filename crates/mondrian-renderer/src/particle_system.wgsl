struct Controls {
    physical_per_sample: vec2<f32>,
    antialias_radius: f32,
    ellipse_aspect: f32,
    tile_cols: u32,
    tile_size: u32,
    blend: u32,
    reserved: u32,
};

struct Particle {
    center_radius: vec4<f32>,
    orientation: vec4<f32>,
    color: vec4<f32>,
};

@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var<uniform> controls: Controls;
@group(0) @binding(2) var<storage, read> particles: array<Particle>;
@group(0) @binding(3) var<storage, read> index: array<u32>;

@vertex
fn vs_main(@builtin(vertex_index) vertex: u32) -> @builtin(position) vec4<f32> {
    let points = array(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    return vec4(points[vertex], 0.0, 1.0);
}

fn particle_coverage(p: Particle, center: vec2<f32>) -> f32 {
    let delta = (center - p.center_radius.xy) * controls.physical_per_sample;
    let x = p.orientation.x * delta.x + p.orientation.y * delta.y;
    let y = p.orientation.x * delta.y - p.orientation.y * delta.x;
    let sx = x * controls.ellipse_aspect;
    let radius = p.center_radius.z;
    let aa = controls.antialias_radius;
    let distance = sqrt(sx * sx + y * y) - radius * controls.ellipse_aspect;
    let weight = min(radius * radius * controls.ellipse_aspect / (aa * aa), 1.0);
    return clamp(0.5 - 0.5 * distance / aa, 0.0, 1.0) * weight;
}

fn paint(backdrop: vec4<f32>, color: vec3<f32>, alpha: f32) -> vec4<f32> {
    if alpha <= 0.0 { return backdrop; }
    let base_alpha = clamp(backdrop.a, 0.0, 1.0);
    if base_alpha <= 0.0 { return vec4(color, alpha); }
    let base = backdrop.rgb;
    let under_alpha = base_alpha * (1.0 - alpha);
    let output_alpha = alpha + under_alpha;
    var ink = color;
    switch controls.blend {
        case 0u: {
            return vec4((base * base_alpha + color * alpha) / output_alpha, output_alpha);
        }
        case 2u: {
            ink = 1.0 - (1.0 - base) * (1.0 - color);
        }
        case 3u: {
            ink = select(
                1.0 - 2.0 * (1.0 - base) * (1.0 - color),
                2.0 * base * color,
                base <= vec3(0.5),
            );
        }
        default: {}
    }
    return vec4((ink * alpha + base * under_alpha) / output_alpha, output_alpha);
}

@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<u32>(position.xy);
    let center = vec2<f32>(pixel) + vec2(0.5);
    let tile = pixel / vec2(controls.tile_size);
    let slot = tile.y * controls.tile_cols + tile.x;
    var result = textureLoad(source, vec2<i32>(pixel), 0);
    let end = index[slot + 1u];
    for (var i = index[slot]; i < end; i += 1u) {
        let p = particles[index[i]];
        result = paint(result, p.color.rgb, particle_coverage(p, center) * p.color.a);
    }
    return result;
}
