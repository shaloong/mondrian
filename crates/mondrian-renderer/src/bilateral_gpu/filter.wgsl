struct Config {
    image_grid: vec4<u32>,
    range: vec4<u32>, // grid depth, slab width, blur axis
    spatial_scale: vec4<f32>,
    window: vec4<u32>, // first global plane, number of planes, output slab
    luma: vec4<f32>,
    spatial_weights: vec4<f32>,
};

const GAUSS: array<f32, 4> = array<f32, 4>(1.0, 0.60653066, 0.13533528, 0.011108996);

@group(0) @binding(0) var<uniform> config: Config;
@group(0) @binding(1) var<storage, read> input_grid: array<vec2<f32>>;
@group(0) @binding(2) var<storage, read_write> output_grid: array<vec2<f32>>;
@group(0) @binding(3) var source: texture_2d<f32>;
@group(0) @binding(4) var<storage, read_write> output_samples: array<f32>;
@group(0) @binding(5) var<storage, read> slab_starts: array<u32>;
@group(0) @binding(6) var<storage, read> grouped_pixels: array<u32>;
@group(0) @binding(7) var<storage, read_write> dispatch_args: array<u32>;
@group(0) @binding(9) var<storage, read> guide_words: array<u32>;
@group(0) @binding(10) var<storage, read_write> fresh_starts: array<u32>;
@group(0) @binding(11) var<storage, read> fresh_start: array<u32>;
@group(0) @binding(12) var<storage, read> green_result: array<f32>;
@group(0) @binding(13) var<storage, read> blue_result: array<f32>;
@group(0) @binding(15) var<storage, read> first_result: array<f32>;

const GROUPS_PER_ROW: u32 = 1024u;

fn source_pixel(pixel: u32) -> vec4<f32> {
    let width = config.image_grid.x;
    return textureLoad(source, vec2<i32>(i32(pixel % width), i32(pixel / width)), 0);
}

fn read_sample(pixel: u32, rgba: vec4<f32>) -> vec3<f32> {
    return vec3<f32>(signal_value(rgba, config.range.w, config.luma.xyz), rgba.a,
        bitcast<f32>(guide_words[pixel]));
}

fn write_groups(slot: u32, count: u32) {
    dispatch_args[slot] = min(GROUPS_PER_ROW, count);
    dispatch_args[slot + 1u] = max(1u, (count + GROUPS_PER_ROW - 1u) / GROUPS_PER_ROW);
    dispatch_args[slot + 2u] = 1u;
}

fn write_dispatch(slot: u32, invocations: u32) {
    write_groups(slot, (invocations + 63u) / 64u);
}

@compute @workgroup_size(64)
fn plan_windows(@builtin(global_invocation_id) id: vec3<u32>) {
    let slab = id.x;
    let slab_width = config.range.y;
    let depth = config.range.x;
    if (slab > (depth - 1u) / slab_width) {
        return;
    }
    let samples_in_slab = slab_starts[slab + 1u] - slab_starts[slab];
    let first = max(3u, slab * slab_width) - 3u;
    let end = min(depth, (slab + 1u) * slab_width + 4u);
    var fresh = first;
    if (slab > 0u && slab_starts[slab] > slab_starts[slab - 1u]) {
        // Two slab steps are wider than a full window, so only the immediately
        // preceding slab can share planes with this one.
        fresh = min(end, slab * slab_width + 4u);
    }
    fresh_starts[slab] = fresh;
    let spatial_plane = config.image_grid.z * config.image_grid.w;
    let owned_planes = min(depth, (slab + 1u) * slab_width + 1u) - slab * slab_width;
    let new_planes = select(0u, end - fresh, samples_in_slab > 0u);
    let xy_tiles = ((config.image_grid.z + 7u) / 8u) * ((config.image_grid.w + 7u) / 8u);
    write_dispatch(slab * 15u, spatial_plane * new_planes);
    write_dispatch(slab * 15u + 3u, samples_in_slab);
    write_dispatch(slab * 15u + 6u, select(0u, spatial_plane, new_planes > 0u));
    write_dispatch(slab * 15u + 9u, select(0u, spatial_plane * owned_planes, samples_in_slab > 0u));
    write_groups(slab * 15u + 12u, xy_tiles * new_planes);
}

fn grid_index(coord: vec3<u32>) -> u32 {
    return (coord.z * config.image_grid.w + coord.y) * config.image_grid.z + coord.x;
}


var<workgroup> tile_source: array<vec2<f32>, 196>;
var<workgroup> tile_horizontal: array<vec2<f32>, 112>;

@compute @workgroup_size(64)
fn blur_xy_tile(@builtin(workgroup_id) id: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>,
                @builtin(local_invocation_index) lane: u32) {
    let tile = id.x + id.y * groups.x;
    let dims = config.image_grid.zw;
    let tiles = (dims + vec2<u32>(7u)) / 8u;
    let tiles_per_plane = tiles.x * tiles.y;
    let local_z = tile / tiles_per_plane;
    let global_z = fresh_start[config.window.z] + local_z;
    if (global_z >= config.window.x + config.window.y) {
        return;
    }
    let xy = vec2<u32>(tile % tiles.x, (tile % tiles_per_plane) / tiles.x) * 8u;
    let origin = vec2<i32>(xy) - vec2<i32>(3);
    for (var i = lane; i < 196u; i += 64u) {
        let p = origin + vec2<i32>(i32(i % 14u), i32(i / 14u));
        var value = vec2<f32>(0.0);
        if (all(p >= vec2<i32>(0)) && all(p < vec2<i32>(dims))) {
            value = input_grid[grid_index(vec3<u32>(vec2<u32>(p), local_z))];
        }
        tile_source[i] = value;
    }
    workgroupBarrier();
    for (var i = lane; i < 112u; i += 64u) {
        let x = i % 8u;
        let row = (i / 8u) * 14u;
        var accum = vec2<f32>(0.0);
        for (var tap = 0u; tap < 7u; tap += 1u) {
            accum += tile_source[row + x + tap] * config.spatial_weights[abs(i32(tap) - 3)];
        }
        tile_horizontal[i] = accum;
    }
    workgroupBarrier();
    let x = lane % 8u;
    let y = lane / 8u;
    let p = xy + vec2<u32>(x, y);
    if (all(p < dims)) {
        var accum = vec2<f32>(0.0);
        for (var tap = 0u; tap < 7u; tap += 1u) {
            accum += tile_horizontal[(y + tap) * 8u + x] * config.spatial_weights[abs(i32(tap) - 3)];
        }
        let ring = global_z % min(config.range.x, 15u);
        output_grid[grid_index(vec3<u32>(p, ring))] = accum;
    }
}

@compute @workgroup_size(64)
fn blur_ring_z(@builtin(global_invocation_id) id: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>) {
    let index = id.x + id.y * groups.x * 64u;
    let plane = config.image_grid.z * config.image_grid.w;
    let base = config.window.z * config.range.y;
    let z = base + index / plane;
    if (z >= min(config.range.x, base + config.range.y + 1u)) {
        return;
    }
    let ring_planes = min(config.range.x, 15u);
    let xy = index % plane;
    var accum = vec2<f32>(0.0);
    for (var offset = -3; offset <= 3; offset += 1) {
        let near = i32(z) + offset;
        if (near >= 0 && near < i32(config.range.x)) {
            accum += input_grid[(u32(near) % ring_planes) * plane + xy] * GAUSS[abs(offset)];
        }
    }
    output_grid[(z - config.window.x) * plane + xy] = accum;
}

@compute @workgroup_size(64)
fn seed_output(@builtin(global_invocation_id) id: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>) {
    let pixel = id.x + id.y * groups.x * 64u;
    if (pixel < config.image_grid.x * config.image_grid.y) {
        output_samples[pixel] = signal_value(source_pixel(pixel), config.range.w, config.luma.xyz);
    }
}






@compute @workgroup_size(64)
fn slice_window(@builtin(global_invocation_id) id: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>) {
    let slot = id.x + id.y * groups.x * 64u + slab_starts[config.window.z];
    if (slot < slab_starts[config.window.z + 1u]) {
        slice_pixel(grouped_pixels[slot]);
    }
}

fn slice_pixel(pixel: u32) {
    let image_w = config.image_grid.x;
    let rgba = source_pixel(pixel);
    if (rgba.a == 0.0) {
        output_samples[pixel] = signal_value(rgba, config.range.w, config.luma.xyz);
        return;
    }
    let sample = read_sample(pixel, rgba);
    let xy = vec2<f32>(f32(pixel % image_w), f32(pixel / image_w)) * config.spatial_scale.x;
    let coord = vec3<f32>(xy, sample.z);
    let lower = vec3<u32>(floor(coord));
    let upper = vec3<u32>(ceil(coord));
    let frac = fract(coord);
    var accum = vec2<f32>(0.0);
    for (var z = lower.z; z <= upper.z; z += 1u) {
        for (var y = lower.y; y <= upper.y; y += 1u) {
            for (var x = lower.x; x <= upper.x; x += 1u) {
                let cell = vec3<u32>(x, y, z);
                let weight = select(frac, 1.0 - frac, cell == lower);
                let local_cell = vec3<u32>(cell.xy, cell.z - config.window.x);
                accum += input_grid[grid_index(local_cell)] * (weight.x * weight.y * weight.z);
            }
        }
    }
    var value = sample.x;
    if (accum.y > 0.0) {
        value = accum.x / accum.y;
    }
    if (!finite_signal(value)) {
        value = sample.x;
    }
    let strength = config.spatial_scale.w;
    output_samples[pixel] = select(fma(strength, value - sample.x, sample.x), value, strength == 1.0);
}

@vertex
fn pack_vertex(@builtin(vertex_index) vertex: u32) -> @builtin(position) vec4<f32> {
    let x = f32((vertex << 1u) & 2u);
    let y = f32(vertex & 2u);
    return vec4<f32>(2.0 * x - 1.0, 1.0 - 2.0 * y, 0.0, 1.0);
}

@fragment
fn pack_fragment(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let xy = vec2<u32>(position.xy);
    let pixel = xy.y * config.image_grid.x + xy.x;
    let rgba = textureLoad(source, vec2<i32>(xy), 0);
    if (rgba.a <= 0.0) {
        return rgba;
    }
    if (config.range.w == 3u) {
        let gray = first_result[pixel];
        return select(rgba, vec4<f32>(vec3<f32>(gray), rgba.a), finite_signal(gray));
    }
    return vec4<f32>(first_result[pixel], green_result[pixel], blue_result[pixel], rgba.a);
}
