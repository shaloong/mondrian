//! Star burst.
//!
//! Places a procedural star field in front of the frame and mixes it over the
//! source. Stars sit on a square lattice whose cell size comes from the grid
//! spacing, and every cell derives its star from an integer hash of the cell
//! coordinate and the phase, so the pattern is reproducible on the CPU and the
//! GPU bit for bit.
//!
//! The field reads no neighbouring pixels: each pixel decides on its own
//! whether it falls inside a star and how bright that star is, which keeps the
//! effect in the fused point pass. Stars closer to the frame edge are drawn
//! larger, which is what gives the field its tunnel perspective.

use thiserror::Error;

/// Smallest grid spacing, in pixels.
pub const STAR_BURST_MIN_GRID_SPACING: f32 = 1.0;
/// Largest grid spacing, in pixels.
pub const STAR_BURST_MAX_GRID_SPACING: f32 = 32.0;
/// Documented default grid spacing, in pixels.
pub const STAR_BURST_DEFAULT_GRID_SPACING: f32 = 4.0;

/// Smallest star size, in pixels.
pub const STAR_BURST_MIN_SIZE: f32 = 20.0;
/// Largest star size, in pixels.
pub const STAR_BURST_MAX_SIZE: f32 = 400.0;
/// Documented default star size, in pixels.
pub const STAR_BURST_DEFAULT_SIZE: f32 = 100.0;

/// Smallest shading, as a percentage.
pub const STAR_BURST_MIN_SHADING: f32 = -100.0;
/// Largest shading, as a percentage.
pub const STAR_BURST_MAX_SHADING: f32 = 100.0;
/// Documented default shading, as a percentage.
pub const STAR_BURST_DEFAULT_SHADING: f32 = 0.0;

/// Smallest scatter, in pixels.
pub const STAR_BURST_MIN_SCATTER: f32 = -10_000.0;
/// Largest scatter, in pixels.
pub const STAR_BURST_MAX_SCATTER: f32 = 10_000.0;
/// Documented default scatter, in pixels.
pub const STAR_BURST_DEFAULT_SCATTER: f32 = 100.0;

/// Smallest speed factor.
pub const STAR_BURST_MIN_SPEED: f32 = -5.0;
/// Largest speed factor.
pub const STAR_BURST_MAX_SPEED: f32 = 5.0;
/// Documented default speed factor.
pub const STAR_BURST_DEFAULT_SPEED: f32 = 1.0;

/// Smallest phase, in seconds.
pub const STAR_BURST_MIN_PHASE: f32 = -10_000.0;
/// Largest phase, in seconds.
pub const STAR_BURST_MAX_PHASE: f32 = 10_000.0;
/// Documented default phase, in seconds.
pub const STAR_BURST_DEFAULT_PHASE: f32 = 0.0;

/// Smallest blend with the original, as a fraction.
pub const STAR_BURST_MIN_BLEND: f32 = 0.0;
/// Largest blend with the original, as a fraction.
pub const STAR_BURST_MAX_BLEND: f32 = 1.0;
/// Documented default blend with the original, as a fraction.
pub const STAR_BURST_DEFAULT_BLEND: f32 = 0.0;

/// Fraction of cells that hold a star.
const STAR_PRESENCE: f32 = 0.5;
/// Perspective falloff that makes stars near the frame edge larger.
const STAR_PERSPECTIVE: f32 = 3.0;
/// Smallest distance that still has a defined star gradient.
const MINIMUM_DISTANCE: f32 = 1.0e-4;

/// Authored parameters of the star burst effect.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StarBurstParameters {
    /// Lattice cell size, in pixels.
    pub grid_spacing: f32,
    /// Star diameter at the centre of the frame, in pixels.
    pub size: f32,
    /// Brightness gradient across a star, as a percentage.
    pub shading: f32,
    /// Random offset of every star inside its cell, in pixels.
    pub scatter: f32,
    /// Multiplier applied to the phase when seeding the lattice.
    pub speed: f32,
    /// Animation offset, in seconds.
    pub phase: f32,
    /// Mix between the star field and the source frame.
    pub blend: f32,
}

impl Default for StarBurstParameters {
    fn default() -> Self {
        Self {
            grid_spacing: STAR_BURST_DEFAULT_GRID_SPACING,
            size: STAR_BURST_DEFAULT_SIZE,
            shading: STAR_BURST_DEFAULT_SHADING,
            scatter: STAR_BURST_DEFAULT_SCATTER,
            speed: STAR_BURST_DEFAULT_SPEED,
            phase: STAR_BURST_DEFAULT_PHASE,
            blend: STAR_BURST_DEFAULT_BLEND,
        }
    }
}

/// A star burst parameter outside its documented range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("star_burst.{parameter} must be finite and within its documented range")]
pub struct StarBurstError {
    parameter: &'static str,
}

impl StarBurstError {
    /// Name of the rejected parameter.
    #[must_use]
    pub const fn parameter(&self) -> &'static str {
        self.parameter
    }

    const fn new(parameter: &'static str) -> Self {
        Self { parameter }
    }
}

/// Validated star burst parameters plus the packed star lattice.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StarBurstGrade {
    parameters: StarBurstParameters,
}

impl StarBurstGrade {
    /// Validates the authored parameters.
    pub fn new(parameters: StarBurstParameters) -> Result<Self, StarBurstError> {
        finite(
            parameters.grid_spacing,
            "grid_spacing",
            STAR_BURST_MIN_GRID_SPACING,
            STAR_BURST_MAX_GRID_SPACING,
        )?;
        finite(
            parameters.size,
            "size",
            STAR_BURST_MIN_SIZE,
            STAR_BURST_MAX_SIZE,
        )?;
        finite(
            parameters.shading,
            "shading",
            STAR_BURST_MIN_SHADING,
            STAR_BURST_MAX_SHADING,
        )?;
        finite(
            parameters.scatter,
            "scatter",
            STAR_BURST_MIN_SCATTER,
            STAR_BURST_MAX_SCATTER,
        )?;
        finite(
            parameters.speed,
            "speed",
            STAR_BURST_MIN_SPEED,
            STAR_BURST_MAX_SPEED,
        )?;
        finite(
            parameters.phase,
            "phase",
            STAR_BURST_MIN_PHASE,
            STAR_BURST_MAX_PHASE,
        )?;
        finite(
            parameters.blend,
            "blend",
            STAR_BURST_MIN_BLEND,
            STAR_BURST_MAX_BLEND,
        )?;
        Ok(Self { parameters })
    }

    /// The documented defaults.
    #[must_use]
    pub fn documented_defaults() -> Self {
        Self { parameters: StarBurstParameters::default() }
    }

    /// The authored parameters.
    #[must_use]
    pub const fn parameters(&self) -> &StarBurstParameters {
        &self.parameters
    }

    /// Whether the effect leaves the frame untouched.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.parameters.blend <= 0.0
    }

    /// Relative cost of the effect.
    #[must_use]
    pub const fn cost(&self) -> u32 {
        2
    }

    /// Packed point uniform: lattice size, star size, shading and scatter, then
    /// the speed, phase and blend, and nothing else.
    #[must_use]
    pub fn gpu_controls(&self) -> ([f32; 4], [f32; 4]) {
        let cell = self.parameters.grid_spacing.max(1.0).round();
        (
            [
                cell,
                self.parameters.size,
                self.parameters.shading / 100.0,
                self.parameters.scatter,
            ],
            [
                self.parameters.speed,
                self.parameters.phase,
                self.parameters.blend,
                0.0,
            ],
        )
    }

    /// Folds every authored value into the render op hash.
    pub fn hash_state<H: std::hash::Hasher>(&self, state: &mut H) {
        use std::hash::Hash;
        for value in [
            self.parameters.grid_spacing,
            self.parameters.size,
            self.parameters.shading,
            self.parameters.scatter,
            self.parameters.speed,
            self.parameters.phase,
            self.parameters.blend,
        ] {
            value.to_bits().hash(state);
        }
    }
}

impl Default for StarBurstGrade {
    fn default() -> Self {
        Self::documented_defaults()
    }
}

fn finite(
    value: f32,
    parameter: &'static str,
    minimum: f32,
    maximum: f32,
) -> Result<(), StarBurstError> {
    if value.is_finite() && (minimum..=maximum).contains(&value) {
        Ok(())
    } else {
        Err(StarBurstError::new(parameter))
    }
}

/// Applies the star burst to a frame.
#[must_use]
pub fn star_burst_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &StarBurstGrade,
) -> bool {
    apply_star_burst_f32_controlled(pixels, width, height, grade, &mut || {
        Ok::<_, std::convert::Infallible>(())
    })
    .unwrap_or(false)
}

/// Applies the star burst to a frame, honouring a cancellation checkpoint.
pub(crate) fn apply_star_burst_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &StarBurstGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(pixel_count) = width.checked_mul(height) else {
        return Ok(false);
    };
    if pixel_count == 0 || working.len() < pixel_count || width == 0 || height == 0 {
        return Ok(false);
    }
    if grade.is_identity() {
        return Ok(true);
    }
    checkpoint()?;

    let cell = grade.parameters.grid_spacing.max(1.0).round();
    let size = grade.parameters.size;
    let shading = grade.parameters.shading / 100.0;
    let scatter = grade.parameters.scatter;
    let blend = grade.parameters.blend;
    let seed = lattice_seed(grade.parameters.speed, grade.parameters.phase);
    let centre_x = width as f32 * 0.5;
    let centre_y = height as f32 * 0.5;
    let extent = (centre_x * centre_x + centre_y * centre_y).sqrt().max(1.0);

    for row in 0..height {
        for column in 0..width {
            let pixel = &mut working[row * width + column];
            // Straight alpha: a transparent pixel still receives stars.
            let dx = column as f32 + 0.5 - centre_x;
            let dy = row as f32 + 0.5 - centre_y;
            let radius = (dx * dx + dy * dy).sqrt() / extent;
            let star_radius = star_radius(size, radius);

            let cell_x = (column as f32 / cell).floor();
            let cell_y = (row as f32 / cell).floor();
            let (presence, offset_x, offset_y, star) = cell_hash(cell_x, cell_y, seed);
            if presence > STAR_PRESENCE {
                continue;
            }

            let star_x = (cell_x + 0.5) * cell + offset_x * scatter * 0.5;
            let star_y = (cell_y + 0.5) * cell + offset_y * scatter * 0.5;
            let offset_px = column as f32 + 0.5 - star_x;
            let offset_py = row as f32 + 0.5 - star_y;
            let distance = (offset_px * offset_px + offset_py * offset_py).sqrt();
            let gradient = 1.0 - (distance / star_radius.max(MINIMUM_DISTANCE)).min(1.0);
            if gradient <= 0.0 {
                continue;
            }
            let coverage = gradient * gradient * (3.0 - 2.0 * gradient);
            let strength = coverage * blend;
            if strength <= 0.0 {
                continue;
            }
            let brightness = (1.0 + shading * (gradient - 0.5)) * (0.35 + 0.65 * star);
            let star_colour = brightness.clamp(0.0, 1.0);
            for channel in pixel.iter_mut().take(3) {
                *channel = *channel + (star_colour - *channel) * strength;
            }
            pixel[3] = pixel[3].max(strength);
        }
    }

    checkpoint()?;
    Ok(true)
}

/// Radius of a star at a normalised distance from the frame centre. Stars
/// near the edge are scaled up, which produces the tunnel perspective.
fn star_radius(size: f32, normalised_radius: f32) -> f32 {
    let perspective = 1.0 + STAR_PERSPECTIVE * normalised_radius;
    (size * 0.5 * perspective).max(0.5)
}

/// Converts the speed and phase into the integer lattice seed.
fn lattice_seed(speed: f32, phase: f32) -> u32 {
    let scaled = (speed * 100.0 + phase * 1000.0).round();
    if scaled.is_finite() {
        scaled as i64 as u64 as u32
    } else {
        0
    }
}

/// Hashes one lattice cell. Returns the presence gate, the two scatter offsets
/// and the star size factor, all in `[0, 1]`.
fn cell_hash(cell_x: f32, cell_y: f32, seed: u32) -> (f32, f32, f32, f32) {
    let x = (cell_x as i64 as u64 as u32).wrapping_add(0x9e37_79b9);
    let y = (cell_y as i64 as u64 as u32).wrapping_add(0x85eb_ca6b);
    let base = hash_u32(x ^ hash_u32(y ^ hash_u32(seed)));
    let presence = (base & 0xFFFF) as f32 / 65_535.0;
    let offset_x = ((base >> 4) & 0x3FF) as f32 / 1_023.0 - 0.5;
    let offset_y = ((base >> 14) & 0x3FF) as f32 / 1_023.0 - 0.5;
    let star = ((base >> 24) & 0xFF) as f32 / 255.0;
    (presence, offset_x, offset_y, star)
}

/// Bit mixing hash; all operations wrap so the GPU can reproduce them.
fn hash_u32(value: u32) -> u32 {
    let mut value = value;
    value ^= value >> 16;
    value = value.wrapping_mul(0x7feb_352d);
    value ^= value >> 15;
    value = value.wrapping_mul(0x846c_a68b);
    value ^= value >> 16;
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::hash::Hasher;

    fn grade(parameters: StarBurstParameters) -> StarBurstGrade {
        StarBurstGrade::new(parameters).expect("valid grade")
    }

    fn flat(width: usize, height: usize, colour: [f32; 4]) -> Vec<[f32; 4]> {
        vec![colour; width * height]
    }

    fn dark_field(parameters: StarBurstParameters) -> StarBurstParameters {
        StarBurstParameters { blend: 1.0, ..parameters }
    }

    #[test]
    fn rejects_values_outside_the_documented_ranges() {
        for grid_spacing in [0.5, 33.0, f32::NAN] {
            let parameters = StarBurstParameters { grid_spacing, ..StarBurstParameters::default() };
            let error = StarBurstGrade::new(parameters).expect_err("grid spacing");
            assert_eq!(error.parameter(), "grid_spacing");
        }
        for size in [19.0, 401.0] {
            let parameters = StarBurstParameters { size, ..StarBurstParameters::default() };
            let error = StarBurstGrade::new(parameters).expect_err("size");
            assert_eq!(error.parameter(), "size");
        }
        for shading in [-101.0, 101.0] {
            let parameters = StarBurstParameters { shading, ..StarBurstParameters::default() };
            let error = StarBurstGrade::new(parameters).expect_err("shading");
            assert_eq!(error.parameter(), "shading");
        }
        for scatter in [-10_001.0, 10_001.0] {
            let parameters = StarBurstParameters { scatter, ..StarBurstParameters::default() };
            let error = StarBurstGrade::new(parameters).expect_err("scatter");
            assert_eq!(error.parameter(), "scatter");
        }
        for speed in [-5.5, 5.5] {
            let parameters = StarBurstParameters { speed, ..StarBurstParameters::default() };
            let error = StarBurstGrade::new(parameters).expect_err("speed");
            assert_eq!(error.parameter(), "speed");
        }
        for blend in [-0.1, 1.1] {
            let parameters = StarBurstParameters { blend, ..StarBurstParameters::default() };
            let error = StarBurstGrade::new(parameters).expect_err("blend");
            assert_eq!(error.parameter(), "blend");
        }
    }

    #[test]
    fn documented_defaults_are_stable() {
        let grade = StarBurstGrade::documented_defaults();
        assert_eq!(grade.parameters().grid_spacing, 4.0);
        assert_eq!(grade.parameters().size, 100.0);
        assert_eq!(grade.parameters().blend, 0.0);
        assert!(grade.is_identity());
        assert_eq!(grade.cost(), 2);
    }

    #[test]
    fn uniform_cells_carry_the_authored_values() {
        let grade = grade(StarBurstParameters {
            grid_spacing: 4.4,
            size: 120.0,
            shading: 50.0,
            scatter: -30.0,
            speed: 2.0,
            phase: 0.5,
            blend: 0.75,
        });
        let (params, extra) = grade.gpu_controls();
        assert_eq!(params, [4.0, 120.0, 0.5, -30.0]);
        assert_eq!(extra, [2.0, 0.5, 0.75, 0.0]);
    }

    #[test]
    fn a_zero_blend_leaves_the_frame_alone() {
        let mut pixels = flat(8, 6, [0.2, 0.3, 0.4, 1.0]);
        let original = pixels.clone();
        assert!(star_burst_frame_f32(
            &mut pixels,
            8,
            6,
            &StarBurstGrade::documented_defaults()
        ));
        assert_eq!(pixels, original);
    }

    #[test]
    fn stars_light_up_a_dark_frame() {
        let mut pixels = flat(64, 48, [0.0, 0.0, 0.0, 1.0]);
        let grade = grade(dark_field(StarBurstParameters::default()));
        assert!(star_burst_frame_f32(&mut pixels, 64, 48, &grade));
        assert!(pixels.iter().any(|pixel| pixel[0] > 0.05));
    }

    #[test]
    fn the_grid_spacing_changes_the_lattice() {
        let mut dense_pixels = flat(64, 64, [0.0, 0.0, 0.0, 1.0]);
        let mut sparse_pixels = dense_pixels.clone();
        let dense = grade(dark_field(StarBurstParameters {
            grid_spacing: 2.0,
            size: 40.0,
            scatter: 0.0,
            ..StarBurstParameters::default()
        }));
        let sparse = grade(dark_field(StarBurstParameters {
            grid_spacing: 32.0,
            size: 40.0,
            scatter: 0.0,
            ..StarBurstParameters::default()
        }));
        assert!(star_burst_frame_f32(&mut dense_pixels, 64, 64, &dense));
        assert!(star_burst_frame_f32(&mut sparse_pixels, 64, 64, &sparse));
        assert_ne!(dense_pixels, sparse_pixels);
    }

    #[test]
    fn larger_stars_cover_more_pixels() {
        let mut small_pixels = flat(64, 64, [0.0, 0.0, 0.0, 1.0]);
        let mut large_pixels = small_pixels.clone();
        let small = grade(dark_field(StarBurstParameters {
            size: 20.0,
            ..StarBurstParameters::default()
        }));
        let large = grade(dark_field(StarBurstParameters {
            size: 200.0,
            ..StarBurstParameters::default()
        }));
        assert!(star_burst_frame_f32(&mut small_pixels, 64, 64, &small));
        assert!(star_burst_frame_f32(&mut large_pixels, 64, 64, &large));
        let small_lit = small_pixels.iter().filter(|pixel| pixel[0] > 0.0).count();
        let large_lit = large_pixels.iter().filter(|pixel| pixel[0] > 0.0).count();
        assert!(large_lit > small_lit);
    }

    #[test]
    fn shading_darkens_the_far_side_of_a_star() {
        let mut positive_pixels = flat(64, 64, [0.0, 0.0, 0.0, 1.0]);
        let mut negative_pixels = positive_pixels.clone();
        let positive = grade(dark_field(StarBurstParameters {
            shading: 80.0,
            ..StarBurstParameters::default()
        }));
        let negative = grade(dark_field(StarBurstParameters {
            shading: -80.0,
            ..StarBurstParameters::default()
        }));
        assert!(star_burst_frame_f32(
            &mut positive_pixels,
            64,
            64,
            &positive
        ));
        assert!(star_burst_frame_f32(
            &mut negative_pixels,
            64,
            64,
            &negative
        ));
        let positive_total: f32 = positive_pixels.iter().map(|pixel| pixel[0]).sum();
        let negative_total: f32 = negative_pixels.iter().map(|pixel| pixel[0]).sum();
        assert!(positive_total > negative_total);
    }

    #[test]
    fn scatter_moves_the_stars() {
        let mut still_pixels = flat(64, 64, [0.0, 0.0, 0.0, 1.0]);
        let mut scattered_pixels = still_pixels.clone();
        let still = grade(dark_field(StarBurstParameters {
            scatter: 0.0,
            ..StarBurstParameters::default()
        }));
        let scattered = grade(dark_field(StarBurstParameters {
            scatter: 8.0,
            ..StarBurstParameters::default()
        }));
        assert!(star_burst_frame_f32(&mut still_pixels, 64, 64, &still));
        assert!(star_burst_frame_f32(
            &mut scattered_pixels,
            64,
            64,
            &scattered
        ));
        assert_ne!(still_pixels, scattered_pixels);
    }

    #[test]
    fn moving_the_phase_changes_the_lattice() {
        let mut first_pixels = flat(48, 48, [0.0, 0.0, 0.0, 1.0]);
        let mut second_pixels = first_pixels.clone();
        let first = grade(dark_field(StarBurstParameters {
            phase: 0.0,
            ..StarBurstParameters::default()
        }));
        let second = grade(dark_field(StarBurstParameters {
            phase: 1.0,
            ..StarBurstParameters::default()
        }));
        assert!(star_burst_frame_f32(&mut first_pixels, 48, 48, &first));
        assert!(star_burst_frame_f32(&mut second_pixels, 48, 48, &second));
        assert_ne!(first_pixels, second_pixels);
    }

    #[test]
    fn speed_scales_the_phase() {
        let mut slow_pixels = flat(48, 48, [0.0, 0.0, 0.0, 1.0]);
        let mut fast_pixels = slow_pixels.clone();
        let slow = grade(dark_field(StarBurstParameters {
            speed: 0.5,
            phase: 0.4,
            ..StarBurstParameters::default()
        }));
        let fast = grade(dark_field(StarBurstParameters {
            speed: 4.0,
            phase: 0.4,
            ..StarBurstParameters::default()
        }));
        assert!(star_burst_frame_f32(&mut slow_pixels, 48, 48, &slow));
        assert!(star_burst_frame_f32(&mut fast_pixels, 48, 48, &fast));
        assert_ne!(slow_pixels, fast_pixels);
    }

    #[test]
    fn stars_near_the_edge_are_larger() {
        let centre = star_radius(80.0, 0.0);
        let edge = star_radius(80.0, 0.9);
        assert!(edge > centre);
        assert!((centre - 40.0).abs() < 1.0e-4);
        assert!((edge - 148.0).abs() < 0.01);
        assert!(star_radius(1.0, 0.0) >= 0.5);
    }

    #[test]
    fn transparent_pixels_receive_stars() {
        let mut pixels = flat(32, 32, [0.0, 0.0, 0.0, 0.0]);
        let grade = grade(dark_field(StarBurstParameters::default()));
        assert!(star_burst_frame_f32(&mut pixels, 32, 32, &grade));
        assert!(pixels.iter().any(|pixel| pixel[3] > 0.0));
    }

    #[test]
    fn the_star_colour_overwrites_the_source_by_the_blend() {
        let mut blended = flat(32, 32, [0.05, 0.05, 0.05, 1.0]);
        let mut full = blended.clone();
        let half = grade(StarBurstParameters { blend: 0.5, ..StarBurstParameters::default() });
        let solid = grade(dark_field(StarBurstParameters::default()));
        assert!(star_burst_frame_f32(&mut blended, 32, 32, &half));
        assert!(star_burst_frame_f32(&mut full, 32, 32, &solid));
        let half_total: f32 = blended.iter().map(|pixel| pixel[0]).sum();
        let full_total: f32 = full.iter().map(|pixel| pixel[0]).sum();
        assert!(full_total > half_total);
    }

    #[test]
    fn short_and_empty_frames_are_rejected() {
        let grade = grade(dark_field(StarBurstParameters::default()));
        let mut short = vec![[0.0, 0.0, 0.0, 1.0]; 3];
        assert!(!star_burst_frame_f32(&mut short, 4, 4, &grade));
        let mut empty = Vec::new();
        assert!(!star_burst_frame_f32(&mut empty, 0, 0, &grade));
    }

    #[test]
    fn the_frame_conversion_is_deterministic() {
        let mut first = flat(40, 30, [0.1, 0.2, 0.3, 0.8]);
        let mut second = first.clone();
        let grade = grade(dark_field(StarBurstParameters::default()));
        assert!(star_burst_frame_f32(&mut first, 40, 30, &grade));
        assert!(star_burst_frame_f32(&mut second, 40, 30, &grade));
        assert_eq!(first, second);
    }

    #[test]
    fn cancellation_stops_the_conversion() {
        let mut pixels = flat(32, 32, [0.0, 0.0, 0.0, 1.0]);
        let grade = grade(dark_field(StarBurstParameters::default()));
        let mut calls = 0;
        let result = apply_star_burst_f32_controlled(&mut pixels, 32, 32, &grade, &mut || {
            calls += 1;
            if calls == 1 {
                Ok::<(), &'static str>(())
            } else {
                Err("cancelled")
            }
        });
        assert_eq!(result, Err("cancelled"));
    }

    #[test]
    fn the_hash_separates_phases_and_blends() {
        fn hash_of(parameters: StarBurstParameters) -> u64 {
            let grade = grade(parameters);
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            grade.hash_state(&mut hasher);
            hasher.finish()
        }
        let base = hash_of(StarBurstParameters::default());
        assert_ne!(
            base,
            hash_of(StarBurstParameters { phase: 1.0, ..StarBurstParameters::default() })
        );
        assert_ne!(
            base,
            hash_of(StarBurstParameters { blend: 0.5, ..StarBurstParameters::default() })
        );
        assert_ne!(
            base,
            hash_of(StarBurstParameters { speed: 2.0, ..StarBurstParameters::default() })
        );
    }

    #[test]
    fn the_lattice_hash_is_stable() {
        // Pinning the hash keeps the GPU shader honest: any change to the mix
        // must be made on both sides at once.
        assert_eq!(hash_u32(0), 0x0000_0000);
        assert_eq!(hash_u32(1), 0x6889_90c0);
        assert_eq!((cell_hash(0.0, 0.0, 7).0 * 65_535.0).round() as u32, 1_182);
    }

    #[test]
    fn the_hash_trait_is_implemented() {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        StarBurstGrade::documented_defaults().hash_state(&mut hasher);
        assert_ne!(hasher.finish(), 0);
    }
}
