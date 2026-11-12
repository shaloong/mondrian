// One invocation owns one grid cell and gathers its trilinear contributions.
struct Config {
    image_grid: vec4<u32>, // image width/height, grid width/height
    range: vec4<u32>, // grid depth, slab width
    spatial_scale: vec4<f32>,
    window: vec4<u32>, // first global plane, number of planes, output slab
    luma: vec4<f32>,
    spatial_weights: vec4<f32>,
};

@group(0) @binding(0) var<uniform> config: Config;
@group(0) @binding(1) var source: texture_2d<f32>;
@group(0) @binding(2) var<storage, read> slab_starts: array<u32>;
@group(0) @binding(3) var<storage, read> pixels: array<u32>;
@group(0) @binding(4) var<storage, read_write> moments: array<vec2<f32>>;
@group(0) @binding(5) var<storage, read_write> labels: array<u32>;
@group(0) @binding(6) var<storage, read> block_prefixes: array<u32>;
@group(0) @binding(7) var<storage, read> page_prefixes: array<u32>;
@group(0) @binding(8) var<storage, read> guide_words: array<u32>;
@group(0) @binding(9) var<storage, read> fresh_starts: array<u32>;

const PIXELS_PER_BLOCK: u32 = 128u;
const BLOCKS_PER_PAGE: u32 = 256u;

fn source_pixel(pixel: u32) -> vec4<f32> {
    let width = config.image_grid.x;
    return textureLoad(source, vec2<i32>(i32(pixel % width), i32(pixel / width)), 0);
}

fn read_sample(pixel: u32) -> vec3<f32> {
    let rgba = source_pixel(pixel);
    return vec3<f32>(signal_value(rgba, config.range.w, config.luma.xyz), rgba.a,
        bitcast<f32>(guide_words[pixel]));
}

@compute @workgroup_size(64)
fn classify_samples(@builtin(global_invocation_id) id: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>) {
    let pixel = id.x + id.y * groups.x * 64u;
    if (pixel >= config.image_grid.x * config.image_grid.y) {
        return;
    }
    let sample = sample_signal(source_pixel(pixel), config.range.w, config.spatial_scale.y, config.spatial_scale.z, config.luma.xyz);
    labels[pixel] = select(0xffffffffu, bitcast<u32>(sample.z), sample.y > 0.0 && finite_signal(sample.x));
}

fn lower_bound(slab: u32, pixel: u32) -> u32 {
    var low = slab_starts[slab];
    var high = slab_starts[slab + 1u];
    let blocks = (config.image_grid.x * config.image_grid.y + PIXELS_PER_BLOCK - 1u) / PIXELS_PER_BLOCK;
    let block = pixel / PIXELS_PER_BLOCK;
    if (block >= blocks) {
        return high;
    }
    low = block_start(slab, block, blocks);
    if (pixel % PIXELS_PER_BLOCK == 0u) {
        return low;
    }
    high = block_start(slab, block + 1u, blocks);
    while (low < high) {
        let mid = (low + high) / 2u;
        if (pixels[mid] < pixel) {
            low = mid + 1u;
        } else {
            high = mid;
        }
    }
    return low;
}

fn block_start(slab: u32, block: u32, blocks: u32) -> u32 {
    if (block == blocks) {
        return slab_starts[slab + 1u];
    }
    let pages = (blocks + BLOCKS_PER_PAGE - 1u) / BLOCKS_PER_PAGE;
    let page = block / BLOCKS_PER_PAGE;
    return slab_starts[slab] + page_prefixes[slab * pages + page]
        + block_prefixes[slab * blocks + block];
}

fn spatial_window(center: u32, size: u32, scale: f32) -> vec2<u32> {
    let lo = (f32(center) - 1.0) / scale;
    let hi = (f32(center) + 1.0) / scale;
    // Include a guard pixel when the reciprocal rounds past an exact edge.
    return vec2<u32>(clamp(vec2<f32>(floor(lo) - 1.0, ceil(hi) + 1.0),
        vec2<f32>(0.0), vec2<f32>(f32(size))));
}

fn tent_at(coordinate: f32, cell: u32) -> f32 {
    let base = floor(coordinate);
    let position = f32(cell);
    if (position == base) {
        return 1.0 - fract(coordinate);
    }
    // Test support before computing a fraction. A fused multiply-add for
    // 1 - abs(pixel * scale - cell) can otherwise leak into an empty cell.
    if (position == base + 1.0 && coordinate > base) {
        return fract(coordinate);
    }
    return 0.0;
}


// Own all fifteen possible range planes at one spatial grid point. Reuse the
// sorted-row lookup and source texel load for both trilinear range corners.
@compute @workgroup_size(64)
fn gather_columns(@builtin(global_invocation_id) id: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>) {
    let cell = id.x + id.y * groups.x * 64u;
    let grid_w = config.image_grid.z;
    let plane = grid_w * config.image_grid.w;
    if (cell >= plane) {
        return;
    }
    let image_w = config.image_grid.x;
    let xy = vec2<u32>(cell % grid_w, cell / grid_w);
    let scale = config.spatial_scale.x;
    let x_window = spatial_window(xy.x, image_w, scale);
    let y_window = spatial_window(xy.y, config.image_grid.y, scale);
    let first_z = fresh_starts[config.window.z];
    let end_z = config.window.x + config.window.y;
    let slab_w = config.range.y;
    let first_slab = (max(1u, first_z) - 1u) / slab_w;
    let last_slab = (end_z - 1u) / slab_w;
    var result: array<vec2<f32>, 15>;
    for (var slab = first_slab; slab <= last_slab; slab += 1u) {
        if (slab_starts[slab] == slab_starts[slab + 1u]) {
            continue;
        }
        for (var row = y_window.x; row < y_window.y; row += 1u) {
            let wy = tent_at(f32(row) * scale, xy.y);
            if (wy == 0.0) {
                continue;
            }
            let start = lower_bound(slab, row * image_w + x_window.x);
            let end = row * image_w + x_window.y;
            for (var index = start; index < slab_starts[slab + 1u]; index += 1u) {
                let pixel = pixels[index];
                if (pixel >= end) {
                    break;
                }
                let sample = read_sample(pixel);
                let wx = tent_at(f32(pixel % image_w) * scale, xy.x);
                let base = u32(sample.z);
                let fraction = fract(sample.z);
                if (base >= first_z && base < end_z) {
                    let weight = (wx * wy * (1.0 - fraction)) * sample.y;
                    result[base - first_z] += vec2<f32>(sample.x * weight, weight);
                }
                let high = base + 1u;
                if (fraction > 0.0 && high >= first_z && high < end_z) {
                    let weight = (wx * wy * fraction) * sample.y;
                    result[high - first_z] += vec2<f32>(sample.x * weight, weight);
                }
            }
        }
    }
    for (var z = 0u; z < end_z - first_z; z += 1u) {
        moments[z * plane + cell] = result[z];
    }
}
