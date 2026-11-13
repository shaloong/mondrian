struct Tile {
    origin_pixel_size: vec4<f32>,
    half_size_crop: vec4<f32>,
    radius_width_feather: vec4<f32>,
    border: vec4<f32>,
    transform_xy: vec4<f32>,
    transform_yz: vec4<f32>,
}

@group(0) @binding(0) var picture: texture_2d<f32>;
@group(0) @binding(1) var<uniform> tile: Tile;

@vertex
fn vs_main(@builtin(vertex_index) vertex: u32) -> @builtin(position) vec4<f32> {
    let triangle = array<vec2<f32>, 3>(vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    return vec4<f32>(triangle[vertex], 0.0, 1.0);
}

fn sample_picture(uv: vec2<f32>) -> vec4<f32> {
    let dimensions = vec2<i32>(textureDimensions(picture));
    let upper = dimensions - vec2<i32>(1);
    let position = clamp(uv, vec2<f32>(0.0), vec2<f32>(1.0)) * vec2<f32>(dimensions) - vec2<f32>(PIXEL_CENTRE);
    let base = floor(position);
    let fraction = position - base;
    let inverse = vec2<f32>(1.0) - fraction;
    let weights = array<f32, 4>(inverse.x * inverse.y, fraction.x * inverse.y, inverse.x * fraction.y, fraction.x * fraction.y);
    var sample = vec4<f32>(0.0);
    for (var i = 0u; i < 4u; i++) {
        let tap = clamp(vec2<i32>(base) + TAPS[i], vec2<i32>(0), upper);
        let value = textureLoad(picture, tap, 0);
        let alpha = clamp(value.a, 0.0, 1.0);
        sample += vec4<f32>(value.rgb * alpha, alpha) * weights[i];
    }
    return sample;
}

@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let half_size = tile.half_size_crop.xy;
    let local = (floor(position.xy) + vec2<f32>(PIXEL_CENTRE) - tile.origin_pixel_size.xy)
        * tile.origin_pixel_size.zw - half_size;
    let radius = tile.radius_width_feather.x;
    let q = abs(local) - half_size + vec2<f32>(radius);
    let distance = length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - radius;
    let feather = tile.radius_width_feather.z;
    let outer = clamp(0.5 - distance / feather, 0.0, 1.0);
    if outer == 0.0 {
        return vec4<f32>(0.0);
    }
    let inner = clamp(0.5 - (distance + tile.radius_width_feather.y) / feather, 0.0, 1.0);
    let uv = vec2<f32>(PIXEL_CENTRE) + PIXEL_CENTRE * local / half_size * tile.half_size_crop.zw;
    let mapped = vec2<f32>(
        tile.transform_xy.x * uv.x + tile.transform_xy.y * uv.y + tile.transform_xy.z,
        tile.transform_xy.w * uv.x + tile.transform_yz.x * uv.y + tile.transform_yz.y,
    );
    let border = vec4<f32>(tile.border.rgb * tile.border.a, tile.border.a);
    let associated = sample_picture(mapped) * inner + border * (outer - inner);
    let alpha = clamp(associated.a, 0.0, 1.0);
    if alpha <= 0.0 {
        return vec4<f32>(0.0);
    }
    return vec4<f32>(associated.rgb / alpha, alpha);
}
