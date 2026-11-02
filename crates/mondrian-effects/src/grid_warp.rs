//! Grid warp: displaces a checkerboard of cell corners along the cell diagonals.
//!
//! The mapping is evaluated in texel-index space. A destination sample
//! `(column, row)` picks its cell with `u = (column + 0.5) / cell_w`, blends the
//! four corner displacements with a smoothstep weight, and returns the displaced
//! source position. The CPU kernel and the WGSL shader must evaluate this
//! expression for expression. `@builtin(position).xy` is already
//! `index + 0.5`, so the shader divides `position.xy` directly and never adds
//! the half-pixel again.

use crate::resample::sample_bilinear_clamped;
use crate::straight_alpha::{premultiply_rgba_controlled, unpremultiply_rgba_controlled};

/// Smallest authored vertex count along one axis (a single cell).
pub const GRID_WARP_MIN_AXIS: i64 = 1;
/// Largest authored vertex count along one axis.
pub const GRID_WARP_MAX_AXIS: i64 = 31;
/// Default vertex count along one axis.
pub const GRID_WARP_DEFAULT_AXIS: i64 = 7;
/// Lowest quality menu value.
pub const GRID_WARP_QUALITY_MIN: u32 = 1;
/// Highest quality menu value.
pub const GRID_WARP_QUALITY_MAX: u32 = 10;
/// Default quality menu value.
pub const GRID_WARP_DEFAULT_QUALITY: u32 = 1;
/// Displacement amplitude as a fraction of the smaller cell edge.
pub const GRID_WARP_AMPLITUDE_RATIO: f32 = 0.125;
/// Maximum slope of the cell blend weight.
pub const GRID_WARP_MAX_BLEND_SLOPE: f32 = 1.5;
/// Axes at or below this count have no interior vertex and are the identity.
pub const GRID_WARP_IDENTITY_MAX_AXIS: i64 = 2;

/// Reasons a [`GridWarpGrade`] cannot be constructed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GridWarpError {
    /// An axis vertex count is outside `GRID_WARP_MIN_AXIS..=GRID_WARP_MAX_AXIS`.
    AxisOutOfRange {
        /// The rejected parameter name.
        parameter: &'static str,
        /// The rejected value.
        value: i64,
    },
    /// The quality menu value is outside `GRID_WARP_QUALITY_MIN..=GRID_WARP_QUALITY_MAX`.
    QualityOutOfRange {
        /// The rejected value.
        value: u32,
    },
}

impl std::fmt::Display for GridWarpError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AxisOutOfRange { parameter, value } => write!(
                formatter,
                "grid warp {parameter} {value} is outside {}..={}",
                GRID_WARP_MIN_AXIS, GRID_WARP_MAX_AXIS
            ),
            Self::QualityOutOfRange { value } => write!(
                formatter,
                "grid warp quality {value} is outside {}..={}",
                GRID_WARP_QUALITY_MIN, GRID_WARP_QUALITY_MAX
            ),
        }
    }
}

impl std::error::Error for GridWarpError {}

/// Validated controls for one grid warp.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridWarpGrade {
    columns: u32,
    rows: u32,
    quality: u32,
}

impl GridWarpGrade {
    /// Validates the authored axes and quality.
    pub fn new(columns: u32, rows: u32, quality: u32) -> Result<Self, GridWarpError> {
        for (parameter, value) in [("columns", i64::from(columns)), ("rows", i64::from(rows))] {
            if !(GRID_WARP_MIN_AXIS..=GRID_WARP_MAX_AXIS).contains(&value) {
                return Err(GridWarpError::AxisOutOfRange { parameter, value });
            }
        }
        if !(GRID_WARP_QUALITY_MIN..=GRID_WARP_QUALITY_MAX).contains(&quality) {
            return Err(GridWarpError::QualityOutOfRange { value: quality });
        }
        Ok(Self { columns, rows, quality })
    }

    /// Number of authored vertex columns.
    pub const fn columns(self) -> u32 {
        self.columns
    }

    /// Number of authored vertex rows.
    pub const fn rows(self) -> u32 {
        self.rows
    }

    /// Authored quality menu value.
    pub const fn quality(self) -> u32 {
        self.quality
    }

    /// Two columns or two rows pin every vertex, so the mapping is the identity.
    pub const fn is_identity(self) -> bool {
        (self.columns as i64) <= GRID_WARP_IDENTITY_MAX_AXIS
            || (self.rows as i64) <= GRID_WARP_IDENTITY_MAX_AXIS
    }

    /// Number of cells along the horizontal axis.
    pub const fn cells_x(self) -> u32 {
        self.columns - 1
    }

    /// Number of cells along the vertical axis.
    pub const fn cells_y(self) -> u32 {
        self.rows - 1
    }

    /// Displacement amplitude in pixels for a frame of this size.
    pub fn amplitude_pixels(self, width: usize, height: usize) -> f32 {
        if self.is_identity() {
            return 0.0;
        }
        let cell_w = width as f32 / self.cells_x() as f32;
        let cell_h = height as f32 / self.cells_y() as f32;
        GRID_WARP_AMPLITUDE_RATIO * cell_w.min(cell_h)
    }

    /// Maps a destination texel index to its source texel index.
    pub fn sample_position(self, column: f32, row: f32, width: usize, height: usize) -> [f32; 2] {
        if self.is_identity() {
            return [column, row];
        }
        let nx = self.cells_x();
        let ny = self.cells_y();
        let cell_w = width as f32 / nx as f32;
        let cell_h = height as f32 / ny as f32;
        let amplitude = self.amplitude_pixels(width, height);
        let u = (column + 0.5) / cell_w;
        let v = (row + 0.5) / cell_h;
        // `min` then `max`, matching the shader's clamp order.
        let k = u.floor().min(nx as f32 - 1.0).max(0.0);
        let l = v.floor().min(ny as f32 - 1.0).max(0.0);
        let alpha = (self.quality - GRID_WARP_QUALITY_MIN) as f32
            / (GRID_WARP_QUALITY_MAX - GRID_WARP_QUALITY_MIN) as f32;
        let s = blend(u - k, alpha);
        let t = blend(v - l, alpha);
        let i = k as u32;
        let j = l as u32;
        let d00 = vertex_displacement(i, j, nx, ny, amplitude);
        let d10 = vertex_displacement(i + 1, j, nx, ny, amplitude);
        let d01 = vertex_displacement(i, j + 1, nx, ny, amplitude);
        let d11 = vertex_displacement(i + 1, j + 1, nx, ny, amplitude);
        // Term by term in the documented order: the shader repeats this
        // association, and reordering changes the last bit.
        let dx = (1.0 - s) * (1.0 - t) * d00[0]
            + s * (1.0 - t) * d10[0]
            + (1.0 - s) * t * d01[0]
            + s * t * d11[0];
        let dy = (1.0 - s) * (1.0 - t) * d00[1]
            + s * (1.0 - t) * d10[1]
            + (1.0 - s) * t * d01[1]
            + s * t * d11[1];
        [column + dx, row + dy]
    }

    /// Packs the uniforms shared with the GPU pass.
    ///
    /// `params` is `(cell_w, cell_h, alpha, gamma)` and `extra` is
    /// `(N_x, N_y, 0, 0)`: the shader keeps the division on the CPU because its
    /// halo signature has no frame dimensions.
    pub fn gpu_controls(self, width: usize, height: usize) -> ([f32; 4], [f32; 4]) {
        let nx = self.cells_x();
        let ny = self.cells_y();
        let cell_w = width as f32 / nx as f32;
        let cell_h = height as f32 / ny as f32;
        let alpha = (self.quality - GRID_WARP_QUALITY_MIN) as f32
            / (GRID_WARP_QUALITY_MAX - GRID_WARP_QUALITY_MIN) as f32;
        (
            [cell_w, cell_h, alpha, GRID_WARP_AMPLITUDE_RATIO],
            [nx as f32, ny as f32, 0.0, 0.0],
        )
    }
}

/// Smoothstep blend between the linear and the smooth cell weight.
fn blend(z: f32, alpha: f32) -> f32 {
    let z2 = z * z;
    (1.0 - alpha) * z + alpha * (3.0 * z2 - 2.0 * z2 * z)
}

/// Checkerboard displacement of one cell vertex.
fn vertex_displacement(i: u32, j: u32, nx: u32, ny: u32, amplitude: f32) -> [f32; 2] {
    if i == 0 || j == 0 || i == nx || j == ny {
        return [0.0, 0.0];
    }
    let sign = if ((i + j) & 1) == 0 { 1.0 } else { -1.0 };
    [amplitude * sign, -amplitude * sign]
}

/// Applies a grid warp in place, returning whether the frame geometry was executable.
pub fn grid_warp_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: GridWarpGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_grid_warp_f32_controlled(pixels, width, height, grade, &mut checkpoint).unwrap_or(false)
}

/// Applies the grid warp in place with an explicit cancellation checkpoint.
///
/// `Ok(true)` means the channel supports and executed the effect (including the
/// identity), `Ok(false)` means the frame geometry is not executable.
pub(crate) fn apply_grid_warp_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: GridWarpGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(pixel_count) = width.checked_mul(height) else {
        return Ok(false);
    };
    if pixel_count == 0 || width == 0 || height == 0 || working.len() < pixel_count {
        return Ok(false);
    }
    if grade.is_identity() {
        return Ok(true);
    }

    let frame = &mut working[..pixel_count];
    // Premultiply exactly once, immediately before the resampling: the shared
    // bilinear helper filters colour and coverage together.
    premultiply_rgba_controlled(frame, checkpoint)?;
    let source = frame.to_vec();
    checkpoint()?;

    crate::parallel::for_each_row_band(frame, width, 0..height, |row_start, band| {
        for (offset, chunk) in band.chunks_mut(width).enumerate() {
            let row = row_start + offset;
            for (column, pixel) in chunk.iter_mut().enumerate() {
                let column = column as f32;
                let row = row as f32;
                let [sample_x, sample_y] = grade.sample_position(column, row, width, height);
                // Pinned boundary rows and columns are already exact; skipping
                // them keeps "no displacement means bitwise unchanged" testable.
                if sample_x.to_bits() == column.to_bits() && sample_y.to_bits() == row.to_bits() {
                    continue;
                }
                *pixel = sample_bilinear_clamped(
                    &source,
                    width,
                    height,
                    f64::from(sample_x),
                    f64::from(sample_y),
                );
            }
        }
    });

    unpremultiply_rgba_controlled(frame, checkpoint)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grade(columns: u32, rows: u32, quality: u32) -> GridWarpGrade {
        GridWarpGrade::new(columns, rows, quality).expect("valid grid warp grade")
    }

    fn frame(width: usize, height: usize) -> Vec<[f32; 4]> {
        let mut pixels = Vec::with_capacity(width * height);
        for y in 0..height {
            for x in 0..width {
                let value = 0.05 * (x as f32 + 2.0 * y as f32);
                let alpha = if (x + 2 * y) % 7 == 0 { 0.5 } else { 1.0 };
                pixels.push([value, 0.5 - value * 0.25, value * 1.5, alpha]);
            }
        }
        pixels
    }

    fn bits(pixels: &[[f32; 4]]) -> Vec<[u32; 4]> {
        pixels.iter().map(|pixel| pixel.map(f32::to_bits)).collect()
    }

    fn reference_position(
        grade: GridWarpGrade,
        column: f64,
        row: f64,
        width: usize,
        height: usize,
    ) -> [f64; 2] {
        let nx = f64::from(grade.cells_x());
        let ny = f64::from(grade.cells_y());
        let cell_w = width as f64 / nx;
        let cell_h = height as f64 / ny;
        let amplitude = 0.125_f64 * cell_w.min(cell_h);
        let u = (column + 0.5) / cell_w;
        let v = (row + 0.5) / cell_h;
        let k = u.floor().min(nx - 1.0).max(0.0);
        let l = v.floor().min(ny - 1.0).max(0.0);
        let alpha = (f64::from(grade.quality()) - 1.0) / 9.0;
        let blend = |z: f64| {
            let z2 = z * z;
            (1.0 - alpha) * z + alpha * (3.0 * z2 - 2.0 * z2 * z)
        };
        let s = blend(u - k);
        let t = blend(v - l);
        let displacement = |i: u32, j: u32| {
            if i == 0 || j == 0 || i == grade.cells_x() || j == grade.cells_y() {
                [0.0, 0.0]
            } else if ((i + j) & 1) == 0 {
                [amplitude, -amplitude]
            } else {
                [-amplitude, amplitude]
            }
        };
        let i = k as u32;
        let j = l as u32;
        let d00 = displacement(i, j);
        let d10 = displacement(i + 1, j);
        let d01 = displacement(i, j + 1);
        let d11 = displacement(i + 1, j + 1);
        let dx = (1.0 - s) * (1.0 - t) * d00[0]
            + s * (1.0 - t) * d10[0]
            + (1.0 - s) * t * d01[0]
            + s * t * d11[0];
        let dy = (1.0 - s) * (1.0 - t) * d00[1]
            + s * (1.0 - t) * d10[1]
            + (1.0 - s) * t * d01[1]
            + s * t * d11[1];
        [column + dx, row + dy]
    }

    fn analytic_vertex_image(
        grade: GridWarpGrade,
        i: u32,
        j: u32,
        width: usize,
        height: usize,
    ) -> [f32; 2] {
        let cell_w = width as f32 / grade.cells_x() as f32;
        let cell_h = height as f32 / grade.cells_y() as f32;
        let amplitude = grade.amplitude_pixels(width, height);
        let displacement = vertex_displacement(i, j, grade.cells_x(), grade.cells_y(), amplitude);
        [
            i as f32 * cell_w - 0.5 + displacement[0],
            j as f32 * cell_h - 0.5 + displacement[1],
        ]
    }

    #[test]
    fn rejects_axes_and_quality_outside_the_declared_ranges() {
        assert_eq!(
            GridWarpGrade::new(0, 7, 1),
            Err(GridWarpError::AxisOutOfRange { parameter: "columns", value: 0 })
        );
        assert_eq!(
            GridWarpGrade::new(7, 32, 1),
            Err(GridWarpError::AxisOutOfRange { parameter: "rows", value: 32 })
        );
        assert_eq!(
            GridWarpGrade::new(7, 7, 0),
            Err(GridWarpError::QualityOutOfRange { value: 0 })
        );
        assert_eq!(
            GridWarpGrade::new(7, 7, 11),
            Err(GridWarpError::QualityOutOfRange { value: 11 })
        );
    }

    #[test]
    fn identity_axes_report_identity() {
        assert!(grade(2, 31, 1).is_identity());
        assert!(grade(31, 2, 1).is_identity());
        assert!(grade(1, 1, 10).is_identity());
        assert!(!grade(3, 3, 1).is_identity());
    }

    #[test]
    fn identity_frames_are_bitwise_unchanged() {
        for columns in 1..=2 {
            for rows in 1..=2 {
                for quality in 1..=10 {
                    let mut pixels = frame(9, 7);
                    let before = bits(&pixels);
                    assert!(grid_warp_frame_f32(
                        &mut pixels,
                        9,
                        7,
                        grade(columns, rows, quality)
                    ));
                    assert_eq!(bits(&pixels), before);
                }
            }
        }
    }

    #[test]
    fn boundary_vertices_alternate_on_the_checkerboard() {
        let amplitude = 1.5;
        assert_eq!(vertex_displacement(0, 3, 6, 6, amplitude), [0.0, 0.0]);
        assert_eq!(vertex_displacement(3, 0, 6, 6, amplitude), [0.0, 0.0]);
        assert_eq!(vertex_displacement(6, 3, 6, 6, amplitude), [0.0, 0.0]);
        assert_eq!(vertex_displacement(3, 6, 6, 6, amplitude), [0.0, 0.0]);
        assert_eq!(
            vertex_displacement(1, 1, 6, 6, amplitude),
            [amplitude, -amplitude]
        );
        assert_eq!(
            vertex_displacement(2, 2, 6, 6, amplitude),
            [amplitude, -amplitude]
        );
        assert_eq!(
            vertex_displacement(1, 2, 6, 6, amplitude),
            [-amplitude, amplitude]
        );
    }

    #[test]
    fn boundary_sample_parameters_are_exact_identity() {
        let grade = grade(5, 4, 7);
        let (width, height) = (37_usize, 29_usize);
        for row in [0.0_f32, 3.5, 11.0, 27.5] {
            assert_eq!(grade.sample_position(-0.5, row, width, height), [-0.5, row]);
            let last_column = width as f32 - 0.5;
            assert_eq!(
                grade.sample_position(last_column, row, width, height),
                [last_column, row]
            );
        }
        for column in [0.0_f32, 4.25, 20.0, 36.5] {
            assert_eq!(
                grade.sample_position(column, -0.5, width, height),
                [column, -0.5]
            );
            let last_row = height as f32 - 0.5;
            assert_eq!(
                grade.sample_position(column, last_row, width, height),
                [column, last_row]
            );
        }
    }

    #[test]
    fn displacement_stays_within_the_amplitude_bound() {
        for columns in 3..=31_u32 {
            for rows in 3..=31_u32 {
                let grade = grade(columns, rows, 10);
                for (width, height) in [(16_usize, 12_usize), (31, 17), (64, 48)] {
                    let amplitude = grade.amplitude_pixels(width, height);
                    let ceiling = amplitude.ceil();
                    for y in 0..height {
                        for x in 0..width {
                            let [sample_x, sample_y] =
                                grade.sample_position(x as f32, y as f32, width, height);
                            assert!(
                                (sample_x - x as f32).abs() <= ceiling
                                    && (sample_y - y as f32).abs() <= ceiling,
                                "displacement escapes the amplitude bound"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn mapping_has_positive_jacobian_determinant() {
        let (width, height) = (48_usize, 36_usize);
        for columns in 3..=31_u32 {
            for rows in 3..=31_u32 {
                for quality in [1_u32, 10] {
                    let grade = grade(columns, rows, quality);
                    let cell_w = width as f32 / grade.cells_x() as f32;
                    let cell_h = height as f32 / grade.cells_y() as f32;
                    let step = 0.05 * cell_w.min(cell_h).max(0.01);
                    for cell_y in 0..grade.cells_y() {
                        for cell_x in 0..grade.cells_x() {
                            for sample_y in 0..4 {
                                for sample_x in 0..4 {
                                    let x = (cell_x as f32 + (sample_x as f32 + 0.5) / 4.0)
                                        * cell_w
                                        - 0.5;
                                    let y = (cell_y as f32 + (sample_y as f32 + 0.5) / 4.0)
                                        * cell_h
                                        - 0.5;
                                    let right = grade.sample_position(x + step, y, width, height);
                                    let left = grade.sample_position(x - step, y, width, height);
                                    let down = grade.sample_position(x, y + step, width, height);
                                    let up = grade.sample_position(x, y - step, width, height);
                                    let j00 = (right[0] - left[0]) / (2.0 * step);
                                    let j10 = (right[1] - left[1]) / (2.0 * step);
                                    let j01 = (down[0] - up[0]) / (2.0 * step);
                                    let j11 = (down[1] - up[1]) / (2.0 * step);
                                    assert!(
                                        j00 * j11 - j01 * j10 > 0.0,
                                        "mapping folds at cell ({cell_x}, {cell_y})"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn cell_quadrilaterals_preserve_the_frame_area() {
        for (width, height) in [(64_usize, 48_usize), (17, 13)] {
            for columns in [3_u32, 4, 7, 16, 31] {
                for rows in [3_u32, 5, 12] {
                    for quality in [1_u32, 5, 10] {
                        let grade = grade(columns, rows, quality);
                        let mut area = 0.0_f64;
                        for cell_y in 0..grade.cells_y() {
                            for cell_x in 0..grade.cells_x() {
                                let corners = [
                                    analytic_vertex_image(grade, cell_x, cell_y, width, height),
                                    analytic_vertex_image(grade, cell_x + 1, cell_y, width, height),
                                    analytic_vertex_image(
                                        grade,
                                        cell_x + 1,
                                        cell_y + 1,
                                        width,
                                        height,
                                    ),
                                    analytic_vertex_image(grade, cell_x, cell_y + 1, width, height),
                                ];
                                let mut doubled = 0.0_f64;
                                for index in 0..4 {
                                    let here = corners[index];
                                    let next = corners[(index + 1) % 4];
                                    doubled += f64::from(here[0]) * f64::from(next[1])
                                        - f64::from(next[0]) * f64::from(here[1]);
                                }
                                assert!(doubled > 0.0, "cell orientation flipped");
                                area += doubled * 0.5;
                            }
                        }
                        let expected = f64::from(width as u32 * height as u32);
                        assert!(
                            (area - expected).abs() / expected < 1.0e-3,
                            "quadrilateral area {area} does not fill the frame"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn sample_position_matches_an_independent_f64_reference() {
        for (columns, rows, quality) in [(7_u32, 7_u32, 1_u32), (5, 9, 4), (31, 31, 10)] {
            let grade = grade(columns, rows, quality);
            let (width, height) = (53_usize, 41_usize);
            for y in 0..height {
                for x in 0..width {
                    let [sample_x, sample_y] =
                        grade.sample_position(x as f32, y as f32, width, height);
                    let reference = reference_position(
                        grade,
                        f64::from(x as u32),
                        f64::from(y as u32),
                        width,
                        height,
                    );
                    assert!((f64::from(sample_x) - reference[0]).abs() < 1.0e-4);
                    assert!((f64::from(sample_y) - reference[1]).abs() < 1.0e-4);
                }
            }
        }
    }

    #[test]
    fn gpu_controls_share_the_kernel_cell_geometry() {
        let (width, height) = (61_usize, 37_usize);
        let grade = grade(9, 5, 6);
        let (params, extra) = grade.gpu_controls(width, height);
        assert_eq!(params[0], width as f32 / 8.0);
        assert_eq!(params[1], height as f32 / 4.0);
        assert_eq!(params[2], 5.0 / 9.0);
        assert_eq!(params[3].to_bits(), GRID_WARP_AMPLITUDE_RATIO.to_bits());
        assert_eq!(extra, [8.0, 4.0, 0.0, 0.0]);
        let [sample_x, sample_y] = grade.sample_position(13.0, 9.0, width, height);
        assert!(sample_x.is_finite() && sample_y.is_finite());
    }

    #[test]
    fn gpu_controls_use_the_smaller_cell_axis() {
        let grade = grade(3, 9, 1);
        let (params, _extra) = grade.gpu_controls(100, 18);
        // cell_w = 50, cell_h = 18 / 8 = 2.25, so the amplitude follows cell_h.
        let amplitude = grade.amplitude_pixels(100, 18);
        assert_eq!(amplitude, 0.125 * params[1]);
        assert_eq!(params[0], 50.0);
        assert!((params[1] - 2.25).abs() < 1.0e-6);
    }

    #[test]
    fn quality_one_is_a_linear_blend() {
        for step in 0..=8 {
            let z = step as f32 / 8.0;
            assert_eq!(blend(z, 0.0).to_bits(), z.to_bits());
        }
        let (params, _extra) = grade(7, 7, 1).gpu_controls(32, 32);
        assert_eq!(params[2], 0.0);
    }

    #[test]
    fn higher_quality_moves_the_blend_toward_smoothstep() {
        let alpha = |quality: u32| {
            (quality - GRID_WARP_QUALITY_MIN) as f32
                / (GRID_WARP_QUALITY_MAX - GRID_WARP_QUALITY_MIN) as f32
        };
        let mut previous_low = blend(0.25, alpha(1));
        let mut previous_high = blend(0.75, alpha(1));
        for quality in 2..=10 {
            let low = blend(0.25, alpha(quality));
            let high = blend(0.75, alpha(quality));
            assert!(low < previous_low, "the low shoulder must ease down");
            assert!(high > previous_high, "the high shoulder must ease up");
            previous_low = low;
            previous_high = high;
        }
        let smoothstep = 3.0 * 0.25 * 0.25 - 2.0 * 0.25 * 0.25 * 0.25;
        assert!((blend(0.25, 1.0) - smoothstep).abs() < 1.0e-6);
    }

    #[test]
    fn preset_swatch_is_invariant_under_the_warp() {
        for alpha in [1.0_f32, 0.5] {
            let mut pixels = vec![[0.25, 0.5, 0.75, alpha]; 11 * 7];
            assert!(grid_warp_frame_f32(&mut pixels, 11, 7, grade(7, 7, 5)));
            for pixel in &pixels {
                assert_eq!(pixel[0].to_bits(), 0.25_f32.to_bits());
                assert_eq!(pixel[1].to_bits(), 0.5_f32.to_bits());
                assert_eq!(pixel[2].to_bits(), 0.75_f32.to_bits());
                assert_eq!(pixel[3].to_bits(), alpha.to_bits());
            }
        }
    }

    #[test]
    fn transparent_frames_stay_transparent() {
        let mut pixels = vec![[3.0, -1.0, 0.5, 0.0]; 13 * 11];
        assert!(grid_warp_frame_f32(&mut pixels, 13, 11, grade(7, 7, 10)));
        for pixel in &pixels {
            assert_eq!(*pixel, [0.0, 0.0, 0.0, 0.0]);
        }
    }

    #[test]
    fn hdr_colours_are_not_clamped() {
        let mut pixels = vec![[1.0e6, -2.0e5, 5.0e5, 1.0]; 9 * 9];
        assert!(grid_warp_frame_f32(&mut pixels, 9, 9, grade(5, 4, 3)));
        for pixel in &pixels {
            assert_eq!(pixel[0].to_bits(), 1.0e6_f32.to_bits());
            assert_eq!(pixel[1].to_bits(), (-2.0e5_f32).to_bits());
            assert_eq!(pixel[2].to_bits(), 5.0e5_f32.to_bits());
            assert_eq!(pixel[3], 1.0);
        }
    }

    #[test]
    fn mismatched_frame_length_is_unsupported() {
        let mut pixels = frame(6, 5);
        pixels.pop();
        assert!(!grid_warp_frame_f32(&mut pixels, 6, 5, grade(7, 7, 1)));
    }

    #[test]
    fn zero_extent_is_unsupported() {
        let mut pixels: Vec<[f32; 4]> = Vec::new();
        assert!(!grid_warp_frame_f32(&mut pixels, 0, 0, grade(7, 7, 1)));
        assert!(!grid_warp_frame_f32(&mut pixels, 0, 5, grade(7, 7, 1)));
        assert!(!grid_warp_frame_f32(&mut pixels, 5, 0, grade(7, 7, 1)));
    }

    #[test]
    fn overflowing_extent_is_unsupported() {
        let mut pixels = frame(4, 4);
        assert!(!grid_warp_frame_f32(
            &mut pixels,
            usize::MAX,
            2,
            grade(7, 7, 1)
        ));
    }

    #[test]
    fn repeated_runs_are_bitwise_identical() {
        let original = frame(19, 13);
        let mut first = original.clone();
        let mut second = original;
        assert!(grid_warp_frame_f32(&mut first, 19, 13, grade(7, 6, 4)));
        assert!(grid_warp_frame_f32(&mut second, 19, 13, grade(7, 6, 4)));
        assert_eq!(bits(&first), bits(&second));
    }

    #[test]
    fn extreme_axes_do_not_panic_on_tiny_and_wide_frames() {
        for (width, height) in [(1_usize, 1_usize), (2, 1), (8192, 1), (1, 8192)] {
            let mut pixels = vec![[0.25, 0.5, 0.75, 1.0]; width * height];
            assert!(grid_warp_frame_f32(
                &mut pixels,
                width,
                height,
                grade(31, 31, 10)
            ));
            for pixel in &pixels {
                assert!(pixel.iter().all(|channel| channel.is_finite()));
            }
        }
    }

    #[test]
    fn checkpoint_rhythm_matches_the_dent_kernel() {
        let mut pixels = frame(9, 7);
        let mut checkpoints = 0_usize;
        let mut checkpoint = || {
            checkpoints += 1;
            Ok::<(), std::convert::Infallible>(())
        };
        apply_grid_warp_f32_controlled(&mut pixels, 9, 7, grade(7, 7, 1), &mut checkpoint)
            .expect("no checkpoint failure");
        // Premultiply enters and leaves, the source copy once, unpremultiply
        // enters and leaves: the same rhythm as the dent kernel.
        assert_eq!(checkpoints, 5);
    }

    #[test]
    fn constants_match_the_effect_list() {
        assert_eq!(GRID_WARP_MIN_AXIS, 1);
        assert_eq!(GRID_WARP_MAX_AXIS, 31);
        assert_eq!(GRID_WARP_DEFAULT_AXIS, 7);
        assert_eq!(GRID_WARP_QUALITY_MIN, 1);
        assert_eq!(GRID_WARP_QUALITY_MAX, 10);
        assert_eq!(GRID_WARP_DEFAULT_QUALITY, 1);
        assert_eq!(GRID_WARP_AMPLITUDE_RATIO, 0.125);
        assert_eq!(GRID_WARP_MAX_BLEND_SLOPE, 1.5);
        assert_eq!(GRID_WARP_IDENTITY_MAX_AXIS, 2);
    }

    #[test]
    fn amplitude_pixels_matches_the_ratio() {
        let grade = grade(5, 9, 1);
        // cells_x = 4, cells_y = 8.
        let amplitude = grade.amplitude_pixels(80, 80);
        assert_eq!(amplitude, 0.125 * 10.0);
        assert_eq!(grade.amplitude_pixels(80, 80), amplitude);
    }
}
