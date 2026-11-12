// Stable grouping of classified pixels into one bounded batch of range slabs.
// Pixel ids are row-major. The host clears block_offsets before each batch.
struct Config {
    pixel_count: u32,
    block_count: u32,
    first_slab: u32,
    slab_count: u32,
};

const BLOCK_SIZE: u32 = 128u;
const TILES_PER_PAGE: u32 = 256u;
const INACTIVE: u32 = 0xffffffffu;

@group(0) @binding(0) var<uniform> config: Config;
@group(0) @binding(1) var<storage, read> labels: array<u32>;
@group(0) @binding(2) var<storage, read_write> block_offsets: array<atomic<u32>>;
@group(0) @binding(3) var<storage, read_write> slab_offsets: array<u32>;
@group(0) @binding(4) var<storage, read_write> output_ids: array<u32>;
@group(0) @binding(5) var<storage, read_write> page_offsets: array<u32>;

fn slab_index(pixel: u32) -> u32 {
    if (pixel >= config.pixel_count) {
        return INACTIVE;
    }
    let word = labels[pixel];
    if (word == INACTIVE) {
        return INACTIVE;
    }
    let label = u32(bitcast<f32>(word)) / 8u;
    if (label < config.first_slab || label - config.first_slab >= config.slab_count) {
        return INACTIVE;
    }
    return label - config.first_slab;
}

fn block_index(group: vec3<u32>, grid: vec3<u32>) -> u32 {
    return group.y * grid.x + group.x;
}

@compute @workgroup_size(128)
fn classify_blocks(
    @builtin(workgroup_id) group: vec3<u32>,
    @builtin(num_workgroups) grid: vec3<u32>,
    @builtin(local_invocation_index) lane: u32,
) {
    let tile = block_index(group, grid);
    if (tile >= config.block_count) {
        return;
    }
    let slab = slab_index(tile * BLOCK_SIZE + lane);
    if (slab != INACTIVE) {
        atomicAdd(&block_offsets[slab * config.block_count + tile], 1u);
    }
}

fn page_count() -> u32 {
    return (config.block_count + TILES_PER_PAGE - 1u) / TILES_PER_PAGE;
}

var<workgroup> prefix: array<u32, 64>;

@compute @workgroup_size(64)
fn scan_block_pages(
    @builtin(workgroup_id) group: vec3<u32>,
    @builtin(local_invocation_index) lane: u32,
) {
    let page = group.x;
    let slab = group.y;
    let tile = page * TILES_PER_PAGE + lane * 4u;
    let base = slab * config.block_count;
    var exclusive: array<u32, 4>;
    var sum = 0u;
    for (var i = 0u; i < 4u && tile + i < config.block_count; i += 1u) {
        exclusive[i] = sum;
        sum += atomicLoad(&block_offsets[base + tile + i]);
    }
    prefix[lane] = sum;
    workgroupBarrier();
    for (var stride = 1u; stride < 64u; stride *= 2u) {
        var prior = 0u;
        if (lane >= stride) {
            prior = prefix[lane - stride];
        }
        workgroupBarrier();
        prefix[lane] += prior;
        workgroupBarrier();
    }
    let carry = prefix[lane] - sum;
    for (var i = 0u; i < 4u && tile + i < config.block_count; i += 1u) {
        atomicStore(&block_offsets[base + tile + i], carry + exclusive[i]);
    }
    if (lane == 63u) {
        page_offsets[slab * page_count() + page] = prefix[lane];
    }
}

@compute @workgroup_size(32)
fn scan_pages(@builtin(global_invocation_id) id: vec3<u32>) {
    let slab = id.x;
    if (slab >= config.slab_count) {
        return;
    }
    var sum = 0u;
    let pages = page_count();
    for (var page = 0u; page < pages; page += 1u) {
        let index = slab * pages + page;
        let count = page_offsets[index];
        page_offsets[index] = sum;
        sum += count;
    }
    slab_offsets[slab] = sum;
}

@compute @workgroup_size(1)
fn scan_slabs() {
    var sum = 0u;
    for (var slab = 0u; slab < config.slab_count; slab += 1u) {
        let count = slab_offsets[slab];
        slab_offsets[slab] = sum;
        sum += count;
    }
    slab_offsets[config.slab_count] = sum;
}

var<workgroup> order: array<vec2<u32>, 128>;
var<workgroup> has_work: atomic<u32>;
var<workgroup> work_status: u32;

fn greater(left: vec2<u32>, right: vec2<u32>) -> bool {
    return left.x > right.x || (left.x == right.x && left.y > right.y);
}

@compute @workgroup_size(128)
fn scatter_stable(
    @builtin(workgroup_id) group: vec3<u32>,
    @builtin(num_workgroups) grid: vec3<u32>,
    @builtin(local_invocation_index) lane: u32,
) {
    let tile = block_index(group, grid);
    if (tile >= config.block_count) {
        return;
    }
    let pixel = tile * BLOCK_SIZE + lane;
    let key = slab_index(pixel);
    order[lane] = vec2<u32>(key, pixel);
    if (key != INACTIVE) {
        atomicOr(&has_work, 1u);
    }
    workgroupBarrier();
    if (lane == 0u) {
        work_status = atomicLoad(&has_work);
    }
    if (workgroupUniformLoad(&work_status) == 0u) {
        return;
    }

    // Sort each tile by slab and original pixel id. Prefixes of earlier tiles
    // supply the deterministic global rank within each slab.
    for (var width = 2u; width <= BLOCK_SIZE; width *= 2u) {
        for (var stride = width / 2u; stride > 0u; stride /= 2u) {
            let mine = order[lane];
            let other = order[lane ^ stride];
            let keep_min = ((lane & width) == 0u) == ((lane & stride) == 0u);
            workgroupBarrier();
            order[lane] = select(mine, other, greater(mine, other) == keep_min);
            workgroupBarrier();
        }
    }

    let entry = order[lane];
    let slab = entry.x;
    if (slab == INACTIVE) {
        return;
    }
    var low = 0u;
    var high = lane;
    while (low < high) {
        let middle = (low + high) / 2u;
        if (order[middle].x < slab) {
            low = middle + 1u;
        } else {
            high = middle;
        }
    }
    let page = tile / TILES_PER_PAGE;
    let offset = slab_offsets[slab] + page_offsets[slab * page_count() + page]
        + atomicLoad(&block_offsets[slab * config.block_count + tile]);
    output_ids[offset + lane - low] = entry.y;
}
