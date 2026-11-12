//! Coverage-weighted bilateral grid over straight Float32 working pixels.

use mondrian_core::WorkingColorSpace;

/// Largest authored radius in pixels.
pub const BILATERAL_MAX_RADIUS: f32 = 30_000.0;
/// Largest authored range threshold.
pub const BILATERAL_MAX_THRESHOLD: f32 = 200.0;
const RANGE_SCALE_LIMIT: f32 = 512.0;
const SLAB: u32 = 8;
const HALO: u32 = 3;
const TILE_CELLS: u32 = 32;
const RANGE_WEIGHTS: [f32; 4] = [1.0, 0.60653066, 0.13533528, 0.011108996];

/// Validated bilateral controls and the graph's working luminance weights.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BilateralBlurGrade {
    radius: f32,
    threshold: f32,
    colorize: bool,
    luma: [f32; 3],
}

/// Invalid bilateral authoring controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BilateralBlurError {
    /// The spatial radius is outside the documented interval.
    #[error("bilateral blur radius must be finite and between 0 and 30000 pixels")]
    Radius,
    /// The range threshold is outside the documented interval.
    #[error("bilateral blur threshold must be finite and between 0 and 200")]
    Threshold,
}

/// Grid dimensions and shared Float32 controls for the CPU and compute paths.
#[derive(Debug, Clone, Copy)]
pub struct BilateralBlurGrid {
    /// Width, height and range depth of the logical grid.
    pub dimensions: [u32; 3],
    /// Pixel-to-grid multiplier, range multiplier, HDR tail and blur strength.
    pub scale: [f32; 4],
    /// Gaussian weights for offsets zero through three in the spatial axes.
    pub spatial_weights: [f32; 4],
    /// Working luminance weights, padded to one uniform cell.
    pub luma: [f32; 4],
}

impl BilateralBlurGrade {
    /// Validate the persisted controls.
    pub fn new(
        radius: f32,
        threshold: f32,
        colorize: bool,
        working: WorkingColorSpace,
    ) -> Result<Self, BilateralBlurError> {
        if !(0.0..=BILATERAL_MAX_RADIUS).contains(&radius) {
            return Err(BilateralBlurError::Radius);
        }
        if !(0.0..=BILATERAL_MAX_THRESHOLD).contains(&threshold) {
            return Err(BilateralBlurError::Threshold);
        }
        Ok(Self {
            radius,
            threshold,
            colorize,
            luma: working.luminance_coefficients(),
        })
    }

    /// Whether the three channels use independent range guides.
    pub const fn colorize(self) -> bool {
        self.colorize
    }

    /// Whether a neighborhood can affect the output.
    pub const fn filters_neighbors(self) -> bool {
        self.radius > 0.0 && self.threshold > 0.0
    }

    /// Conservative support for both spatial axes, including splat and slice.
    pub fn halo(self) -> u32 {
        if self.filters_neighbors() {
            (5.0 * (self.radius / 3.0).max(1.0)).ceil() as u32
        } else {
            0
        }
    }

    /// Resolve the grid on the actual raster. A sub-cell spatial sigma uses
    /// narrower coefficients; a sub-grid range threshold fades to the source.
    pub fn grid(self, width: u32, height: u32) -> BilateralBlurGrid {
        let sigma = self.radius / 3.0;
        let step = sigma.max(1.0);
        let inverse_step = step.recip();
        let range_scale = (BILATERAL_MAX_THRESHOLD / self.threshold).min(RANGE_SCALE_LIMIT);
        let strength = if self.filters_neighbors() {
            (self.threshold * (RANGE_SCALE_LIMIT / BILATERAL_MAX_THRESHOLD)).min(1.0)
        } else {
            0.0
        };
        let spatial_weights = if sigma >= 1.0 {
            RANGE_WEIGHTS
        } else {
            [
                1.0,
                gaussian(1.0, sigma),
                gaussian(2.0, sigma),
                gaussian(3.0, sigma),
            ]
        };
        let [r, g, b] = self.luma;
        BilateralBlurGrid {
            dimensions: [
                grid_edge(width, inverse_step),
                grid_edge(height, inverse_step),
                (10.0 * range_scale).ceil() as u32 + 1,
            ],
            scale: [inverse_step, range_scale, 4.0, strength],
            spatial_weights,
            luma: [r, g, b, 0.0],
        }
    }

    pub(crate) fn hash_signature(self, state: &mut impl std::hash::Hasher) {
        use std::hash::Hash as _;
        self.radius.to_bits().hash(state);
        self.threshold.to_bits().hash(state);
        self.colorize.hash(state);
        self.luma.map(f32::to_bits).hash(state);
    }

    /// Bound the two tile grids and the per-slab indexing tables in bytes.
    /// The source snapshot and source indices are accounted as two frame slots.
    pub(crate) fn tile_scratch_bytes(self, width: u32, height: u32) -> usize {
        if !self.filters_neighbors() || width == 0 || height == 0 {
            return 0;
        }
        let [w, h, depth] = self.grid(width, height).dimensions;
        tile_cells([w, h, depth]) * 16 + (depth.div_ceil(SLAB) as usize + 1) * 32
    }
}

fn tile_cells([w, h, depth]: [u32; 3]) -> usize {
    let edge = TILE_CELLS + 2 * HALO + 3;
    w.min(edge) as usize * h.min(edge) as usize * depth.min(15) as usize
}

fn gaussian(offset: f32, sigma: f32) -> f32 {
    (-0.5 * (offset / sigma).powi(2)).exp()
}

fn grid_edge(pixels: u32, scale: f32) -> u32 {
    ((pixels.saturating_sub(1) as f32 * scale).ceil() as u32).saturating_add(1)
}

fn signal(pixel: [f32; 4], channel: usize, grid: &BilateralBlurGrid) -> f32 {
    if channel < 3 {
        pixel[channel]
    } else {
        let [r, g, b, _] = grid.luma;
        pixel[2].mul_add(b, pixel[1].mul_add(g, pixel[0] * r))
    }
}

fn guide(value: f32, grid: &BilateralBlurGrid) -> f32 {
    let [_, scale, tail, _] = grid.scale;
    let offset = 1.0 + tail;
    let magnitude = value.abs();
    let coordinate = if magnitude > 1.0 {
        let excess = magnitude - 1.0;
        let direction = value.signum();
        (direction * tail).mul_add(excess / (tail + excess), offset + direction)
    } else {
        value + offset
    };
    coordinate * scale
}

fn tile_bounds(start: u32, end: u32, image: u32, grid: u32, scale: f32) -> ([u32; 2], [u32; 2]) {
    let first = ((start as f32 * scale).floor() as u32).saturating_sub(HALO);
    let last = (((end - 1) as f32 * scale).ceil() as u32 + HALO + 1).min(grid);
    let input_first = ((((first as f32 - 1.0) / scale).floor() - 1.0) as u32).min(image);
    let input_last = (((last as f32 / scale).ceil() + 1.0) as u32).min(image);
    ([first, last], [input_first, input_last])
}

fn grid_index([w, h, _]: [u32; 3], [x, y, z]: [u32; 3]) -> usize {
    ((z as usize * h as usize + y as usize) * w as usize) + x as usize
}

fn corners(coord: [f32; 3], mut visit: impl FnMut([u32; 3], f32)) {
    let [x, y, z] = coord;
    for iz in z as u32..=z.ceil() as u32 {
        for iy in y as u32..=y.ceil() as u32 {
            for ix in x as u32..=x.ceil() as u32 {
                let tent = |p: f32, i: u32| 1.0 - (p - i as f32).abs();
                visit([ix, iy, iz], tent(x, ix) * tent(y, iy) * tent(z, iz));
            }
        }
    }
}

fn blur(
    moments: &mut Vec<[f32; 2]>,
    alternate: &mut Vec<[f32; 2]>,
    dims: [u32; 3],
    spatial: [f32; 4],
) {
    let [w, h, _] = dims.map(|d| d as usize);
    for (axis, weights) in [spatial, spatial, RANGE_WEIGHTS].into_iter().enumerate() {
        for (i, out) in alternate.iter_mut().enumerate() {
            let coord = [(i % w) as u32, (i / w % h) as u32, (i / w / h) as u32];
            let mut sum = [0.0; 2];
            for offset in -3_i32..=3 {
                if let Some(next) =
                    coord[axis].checked_add_signed(offset).filter(|&v| v < dims[axis])
                {
                    let mut neighbor = coord;
                    neighbor[axis] = next;
                    let [value, coverage] = moments[grid_index(dims, neighbor)];
                    let weight = weights[offset.unsigned_abs() as usize];
                    sum[0] += value * weight;
                    sum[1] += coverage * weight;
                }
            }
            *out = sum;
        }
        std::mem::swap(moments, alternate);
    }
}

/// Apply bilateral blur to one tightly packed straight Float32 raster.
pub fn bilateral_blur_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &BilateralBlurGrade,
) -> bool {
    apply_bilateral_blur_controlled(pixels, width, height, grade, &mut || {
        Ok::<_, std::convert::Infallible>(())
    })
    .unwrap_or(false)
}

pub(crate) fn apply_bilateral_blur_controlled<E>(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &BilateralBlurGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let (Ok(w), Ok(h)) = (u32::try_from(width), u32::try_from(height)) else {
        return Ok(false);
    };
    let Some(count) = w.checked_mul(h).filter(|&n| n > 0) else {
        return Ok(false);
    };
    let Some(pixels) = pixels.get_mut(..count as usize) else {
        return Ok(false);
    };
    checkpoint()?;
    let grid = grade.grid(w, h);
    if !grade.filters_neighbors() {
        if !grade.colorize() {
            for row in pixels.chunks_mut(width) {
                checkpoint()?;
                for pixel in row.iter_mut().filter(|p| p[3] > 0.0) {
                    let value = signal(*pixel, 3, &grid);
                    if value.is_finite() {
                        pixel[..3].fill(value);
                    }
                }
            }
        }
        return Ok(true);
    }
    let source = pixels.to_vec();
    for channel in if grade.colorize() { 0..3 } else { 3..4 } {
        filter_channel(&source, pixels, w, h, channel, &grid, checkpoint)?;
    }
    Ok(true)
}

fn filter_channel<E>(
    source: &[[f32; 4]],
    output: &mut [[f32; 4]],
    w: u32,
    h: u32,
    channel: usize,
    grid: &BilateralBlurGrid,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), E> {
    let [gw, gh, depth] = grid.dimensions;
    let [scale, _, _, strength] = grid.scale;
    let tile = (TILE_CELLS as f32 / scale).ceil() as u32;
    let mut moments = Vec::with_capacity(tile_cells(grid.dimensions));
    let mut alternate = Vec::with_capacity(tile_cells(grid.dimensions));
    let mut groups = (0..depth.div_ceil(SLAB)).map(|_| Vec::<u32>::new()).collect::<Vec<_>>();
    let mut active = vec![false; groups.len()];
    for y0 in (0..h).step_by(tile as usize) {
        let y1 = y0.saturating_add(tile).min(h);
        let ([gy0, gy1], [sy0, sy1]) = tile_bounds(y0, y1, h, gh, scale);
        for x0 in (0..w).step_by(tile as usize) {
            let x1 = x0.saturating_add(tile).min(w);
            let ([gx0, gx1], [sx0, sx1]) = tile_bounds(x0, x1, w, gw, scale);
            // Drop preceding capacities so disjoint tiles cannot accumulate
            // a high-water allocation for every possible range bucket.
            groups.iter_mut().for_each(|group| *group = Vec::new());
            active.fill(false);
            for y in sy0..sy1 {
                checkpoint()?;
                for x in sx0..sx1 {
                    let id = y * w + x;
                    let pixel = source[id as usize];
                    let value = signal(pixel, channel, grid);
                    if pixel[3] > 0.0 && value.is_finite() {
                        let slab = (guide(value, grid) as u32 / SLAB) as usize;
                        groups[slab].push(id);
                        active[slab] |= x >= x0 && x < x1 && y >= y0 && y < y1;
                    }
                }
            }
            for (slab, _) in active.iter().enumerate().filter(|&(_, &used)| used) {
                checkpoint()?;
                let base = slab as u32 * SLAB;
                let gz0 = base.saturating_sub(HALO);
                let gz1 = (base + SLAB + 1 + HALO).min(depth);
                let dims = [gx1 - gx0, gy1 - gy0, gz1 - gz0];
                let len = dims.into_iter().map(|axis| axis as usize).product();
                moments.clear();
                moments.resize(len, [0.0; 2]);
                alternate.resize(len, [0.0; 2]);
                let sample = |id: u32| {
                    let pixel = source[id as usize];
                    let value = signal(pixel, channel, grid);
                    (
                        [
                            ((id % w) as f32) * scale,
                            ((id / w) as f32) * scale,
                            guide(value, grid),
                        ],
                        value,
                        pixel[3],
                    )
                };
                let start = [gx0, gy0, gz0];
                let end = [gx1, gy1, gz1];
                let first = gz0.saturating_sub(1) as usize / SLAB as usize;
                let last = (gz1 - 1) as usize / SLAB as usize;
                for indices in &groups[first..=last] {
                    for chunk in indices.chunks(4096) {
                        checkpoint()?;
                        for &id in chunk {
                            let (coord, value, alpha) = sample(id);
                            corners(coord, |p, w| {
                                if (0..3).all(|axis| (start[axis]..end[axis]).contains(&p[axis])) {
                                    let local = std::array::from_fn(|axis| p[axis] - start[axis]);
                                    let cell = &mut moments[grid_index(dims, local)];
                                    let weight = w * alpha;
                                    cell[0] += value * weight;
                                    cell[1] += weight;
                                }
                            });
                        }
                    }
                }
                blur(&mut moments, &mut alternate, dims, grid.spatial_weights);
                for &id in &groups[slab] {
                    let (x, y) = (id % w, id / w);
                    if x < x0 || x >= x1 || y < y0 || y >= y1 {
                        continue;
                    }
                    let (coord, value, _) = sample(id);
                    let mut result = [0.0; 2];
                    corners(coord, |p, weight| {
                        let local = std::array::from_fn(|axis| p[axis] - start[axis]);
                        let [color, coverage] = moments[grid_index(dims, local)];
                        result[0] += color * weight;
                        result[1] += coverage * weight;
                    });
                    let filtered = if result[1] > 0.0 {
                        result[0] / result[1]
                    } else {
                        value
                    };
                    let filtered = if filtered.is_finite() {
                        filtered
                    } else {
                        value
                    };
                    let filtered = if strength == 1.0 {
                        filtered
                    } else {
                        (filtered - value).mul_add(strength, value)
                    };
                    let pixel = &mut output[id as usize];
                    if channel == 3 {
                        pixel[..3].fill(filtered);
                    } else {
                        pixel[channel] = filtered;
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grade(radius: f32, threshold: f32, colorize: bool) -> BilateralBlurGrade {
        BilateralBlurGrade::new(radius, threshold, colorize, WorkingColorSpace::LinearRec709)
            .unwrap()
    }

    #[test]
    fn zero_controls_preserve_color_or_apply_working_luminance() {
        let original = [[-0.25, 0.5, 1.5, 0.5], [32.0, 64.0, 2.0, 0.0]];
        let mut pixels = original;
        assert!(bilateral_blur_frame_f32(
            &mut pixels,
            2,
            1,
            &grade(30_000.0, 0.0, true)
        ));
        assert_eq!(pixels, original);
        let gray = grade(0.0, 3.0, false);
        assert!(bilateral_blur_frame_f32(&mut pixels, 2, 1, &gray));
        let luma = signal(original[0], 3, &gray.grid(2, 1));
        assert_eq!(pixels, [[luma, luma, luma, 0.5], original[1]]);
    }

    #[test]
    fn hdr_edges_and_coverage_survive_default_and_maximum_radius() {
        let original = [
            [32.0, 32.0, 32.0, 0.5],
            [64.0, 64.0, 64.0, 1.0],
            [8.0, 9.0, 10.0, 0.0],
        ];
        for radius in [0.1, 25.0, 30_000.0] {
            let mut pixels = original;
            assert!(bilateral_blur_frame_f32(
                &mut pixels,
                3,
                1,
                &grade(radius, 3.0, true)
            ));
            assert_eq!(pixels[2], original[2]);
            for (result, source) in pixels.into_iter().zip(original) {
                assert!((result[0] - source[0]).abs() <= 1.0e-4);
                assert_eq!(result[3], source[3]);
            }
        }
        assert!(grade(25.0, 0.01, true).grid(8, 5).scale[3] < 0.03);
    }
}
