//! The flip effect: mirrors the frame about an authored axis.
//!
//! The effect list gives the axis menu, a centre point and a blend amount, but
//! it does not say what the blend measures. This module reads the blend as the
//! weight of the *flipped* frame, so the listed default of 100 % is a plain
//! flip and zero is the untouched frame. That reading matches "混合 0–100,
//! default 100" in the list: the default must be the effect, not its absence.
//!
//! The axis names follow the line that is mirrored across, not the direction
//! content moves: a horizontal axis flips the frame top to bottom, a vertical
//! axis flips it left to right, and `Both` is the two composed, which is the
//! same as rotating the frame half a turn about the centre.

use crate::resample::sample_bilinear_clamped;
use crate::straight_alpha::{premultiply_rgba_controlled, unpremultiply_rgba_controlled};

/// Largest authoring coordinate the centre accepts, in pixels.
pub const FLIP_MAX_CENTRE_PIXELS: f32 = 1_000_000.0;

/// Largest authoring blend percentage the effect accepts.
pub const FLIP_MAX_BLEND_PERCENT: f32 = 100.0;

/// Number of axis options the effect list declares.
pub const FLIP_AXIS_COUNT: usize = 3;

/// Blend values at or below this are treated as the untouched frame.
const BLEND_EPSILON: f32 = 1.0e-6;

/// The axis a flip mirrors across.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlipAxis {
    /// Mirrors across a horizontal line, exchanging rows.
    Horizontal,
    /// Mirrors across a vertical line, exchanging columns.
    Vertical,
    /// Mirrors across both lines, a half turn about the centre.
    Both,
}

impl FlipAxis {
    /// Every axis, in the order the effect list declares them.
    pub const ALL: [Self; FLIP_AXIS_COUNT] = [Self::Horizontal, Self::Vertical, Self::Both];

    /// Returns the stable authoring key.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Horizontal => "horizontal",
            Self::Vertical => "vertical",
            Self::Both => "both",
        }
    }

    /// Returns the one-based menu value the effect list declares.
    pub const fn as_menu(self) -> i64 {
        match self {
            Self::Horizontal => 1,
            Self::Vertical => 2,
            Self::Both => 3,
        }
    }

    /// Returns the value the GPU control block carries.
    pub const fn as_u32(self) -> u32 {
        match self {
            Self::Horizontal => 1,
            Self::Vertical => 2,
            Self::Both => 3,
        }
    }

    /// Resolves a stable authoring key.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|axis| axis.as_key() == key)
    }

    /// Resolves a one-based menu value.
    pub fn from_menu(menu: i64) -> Option<Self> {
        Self::ALL.into_iter().find(|axis| axis.as_menu() == menu)
    }
}

/// Reasons a flip grade can be rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlipError {
    /// The blend percentage was not finite or outside `[0, 100]`.
    Blend,
    /// The centre or one of its coordinates was not finite or out of range.
    Centre,
}

impl std::fmt::Display for FlipError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Blend => formatter.write_str("flip blend must be finite and within 0..=100"),
            Self::Centre => formatter.write_str("flip centre must be finite and within range"),
        }
    }
}

impl std::error::Error for FlipError {}

/// A validated flip configuration.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FlipGrade {
    axis: FlipAxis,
    centre: [f32; 2],
    blend: f32,
}

impl FlipGrade {
    /// Validates an authoring configuration.
    ///
    /// # Errors
    ///
    /// Returns [`FlipError::Blend`] when the percentage is not finite or lands
    /// outside `[0, 100]`, and [`FlipError::Centre`] when a centre coordinate
    /// is not finite or exceeds [`FLIP_MAX_CENTRE_PIXELS`] in magnitude.
    pub fn new(axis: FlipAxis, centre: [f32; 2], blend_percent: f32) -> Result<Self, FlipError> {
        if !blend_percent.is_finite() || !(0.0..=FLIP_MAX_BLEND_PERCENT).contains(&blend_percent) {
            return Err(FlipError::Blend);
        }
        if centre
            .iter()
            .any(|coordinate| !coordinate.is_finite() || coordinate.abs() > FLIP_MAX_CENTRE_PIXELS)
        {
            return Err(FlipError::Centre);
        }
        Ok(Self {
            axis,
            centre,
            blend: blend_percent / FLIP_MAX_BLEND_PERCENT,
        })
    }

    /// Returns the mirrored axis.
    pub const fn axis(&self) -> FlipAxis {
        self.axis
    }

    /// Returns the authored centre, in pixels.
    pub const fn centre(&self) -> [f32; 2] {
        self.centre
    }

    /// Returns the weight of the flipped frame, in `[0, 1]`.
    pub const fn blend(&self) -> f32 {
        self.blend
    }

    /// Returns whether the grade leaves the frame untouched.
    pub fn is_identity(&self) -> bool {
        self.blend <= BLEND_EPSILON
    }

    /// Returns the source position that feeds one destination pixel.
    pub fn sample_position(&self, column: f64, row: f64) -> [f64; 2] {
        let centre_x = f64::from(self.centre[0]);
        let centre_y = f64::from(self.centre[1]);
        let mirror_x = match self.axis {
            FlipAxis::Horizontal => column,
            FlipAxis::Vertical | FlipAxis::Both => 2.0 * centre_x - column,
        };
        let mirror_y = match self.axis {
            FlipAxis::Vertical => row,
            FlipAxis::Horizontal | FlipAxis::Both => 2.0 * centre_y - row,
        };
        [mirror_x, mirror_y]
    }

    /// Applies the grade to a whole premultiplied frame.
    ///
    /// Returns whether any pixel changed.
    pub fn apply(&self, frame: &mut [[f32; 4]], width: usize, height: usize) -> bool {
        if self.is_identity() {
            return false;
        }
        let source = frame.to_vec();
        let weight = f64::from(self.blend);
        for row in 0..height {
            for column in 0..width {
                let [sample_x, sample_y] = self.sample_position(column as f64, row as f64);
                let sample = sample_bilinear_clamped(&source, width, height, sample_x, sample_y);
                let original = source[row * width + column];
                let index = row * width + column;
                for channel in 0..4 {
                    let mixed = f64::from(original[channel]) * (1.0 - weight)
                        + f64::from(sample[channel]) * weight;
                    frame[index][channel] = mixed as f32;
                }
            }
        }
        true
    }

    /// Returns the uniform payload the GPU pass consumes.
    ///
    /// The first block carries the centre, the blended weight and the axis
    /// menu value; the second block is reserved.
    pub fn gpu_controls(&self) -> ([f32; 4], [f32; 4]) {
        (
            [
                self.centre[0],
                self.centre[1],
                self.blend,
                self.axis.as_u32() as f32,
            ],
            [0.0; 4],
        )
    }
}

/// Flips a straight-alpha frame about the authored axis.
///
/// Returns whether any pixel changed. A zero blend leaves the frame untouched.
pub fn flip_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &FlipGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_flip_f32_controlled(pixels, width, height, grade, &mut checkpoint).unwrap_or(false)
}

/// Flips a straight-alpha frame about the authored axis, with cancellation.
///
/// # Errors
///
/// Returns whatever `checkpoint` reports: [`FlipError`] variants are produced by
/// the grade itself before this call, so the error type stays generic.
pub(crate) fn apply_flip_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &FlipGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    if width == 0 || height == 0 {
        return Ok(false);
    }
    checkpoint()?;
    if grade.is_identity() {
        return Ok(false);
    }
    premultiply_rgba_controlled(pixels, checkpoint)?;
    let changed = grade.apply(pixels, width, height);
    unpremultiply_rgba_controlled(pixels, checkpoint)?;
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIDTH: usize = 5;
    const HEIGHT: usize = 3;

    fn grade(axis: FlipAxis, centre: [f32; 2], blend: f32) -> FlipGrade {
        FlipGrade::new(axis, centre, blend).expect("the grade must validate")
    }

    fn gradient_frame(width: usize, height: usize) -> Vec<[f32; 4]> {
        let mut pixels = Vec::with_capacity(width * height);
        for row in 0..height {
            for column in 0..width {
                let value = (column + row * width) as f32 / 20.0;
                pixels.push([value, 0.5, 0.25, 1.0]);
            }
        }
        pixels
    }

    #[test]
    fn the_blend_and_centre_ranges_are_enforced() {
        let centre = [2.0, 1.0];
        assert_eq!(
            FlipGrade::new(FlipAxis::Both, centre, -1.0),
            Err(FlipError::Blend)
        );
        assert_eq!(
            FlipGrade::new(FlipAxis::Both, centre, 101.0),
            Err(FlipError::Blend)
        );
        assert_eq!(
            FlipGrade::new(FlipAxis::Both, centre, f32::NAN),
            Err(FlipError::Blend)
        );
        assert_eq!(
            FlipGrade::new(FlipAxis::Both, [f32::INFINITY, 1.0], 50.0),
            Err(FlipError::Centre)
        );
    }

    #[test]
    fn the_axis_menus_round_trip() {
        for axis in FlipAxis::ALL {
            assert_eq!(FlipAxis::from_key(axis.as_key()), Some(axis));
            assert_eq!(FlipAxis::from_menu(axis.as_menu()), Some(axis));
            assert_eq!(axis.as_u32() as i64, axis.as_menu());
        }
        assert_eq!(FlipAxis::ALL.len(), FLIP_AXIS_COUNT);
        assert_eq!(FlipAxis::from_menu(4), None);
    }

    #[test]
    fn a_zero_blend_is_the_untouched_frame() {
        let original = gradient_frame(WIDTH, HEIGHT);
        let mut pixels = original.clone();
        assert!(!flip_frame_f32(
            &mut pixels,
            WIDTH,
            HEIGHT,
            &grade(FlipAxis::Both, [2.0, 1.0], 0.0)
        ));
        assert_eq!(pixels, original);
    }

    #[test]
    fn a_horizontal_axis_exchanges_rows_about_the_centre() {
        let original = gradient_frame(WIDTH, HEIGHT);
        let mut pixels = original.clone();
        assert!(flip_frame_f32(
            &mut pixels,
            WIDTH,
            HEIGHT,
            &grade(FlipAxis::Horizontal, [2.0, 1.0], 100.0)
        ));
        for row in 0..HEIGHT {
            for column in 0..WIDTH {
                let mirrored = (HEIGHT - 1 - row) * WIDTH + column;
                assert_eq!(pixels[row * WIDTH + column], original[mirrored]);
            }
        }
    }

    #[test]
    fn a_vertical_axis_exchanges_columns_about_the_centre() {
        let original = gradient_frame(WIDTH, HEIGHT);
        let mut pixels = original.clone();
        assert!(flip_frame_f32(
            &mut pixels,
            WIDTH,
            HEIGHT,
            &grade(FlipAxis::Vertical, [2.0, 1.0], 100.0)
        ));
        for row in 0..HEIGHT {
            for column in 0..WIDTH {
                let mirrored = row * WIDTH + (WIDTH - 1 - column);
                assert_eq!(pixels[row * WIDTH + column], original[mirrored]);
            }
        }
    }

    #[test]
    fn both_axes_are_a_half_turn() {
        let original = gradient_frame(WIDTH, HEIGHT);
        let mut pixels = original.clone();
        assert!(flip_frame_f32(
            &mut pixels,
            WIDTH,
            HEIGHT,
            &grade(FlipAxis::Both, [2.0, 1.0], 100.0)
        ));
        for row in 0..HEIGHT {
            for column in 0..WIDTH {
                let rotated = (HEIGHT - 1 - row) * WIDTH + (WIDTH - 1 - column);
                assert_eq!(pixels[row * WIDTH + column], original[rotated]);
            }
        }
    }

    #[test]
    fn the_centre_pixel_is_a_fixed_point() {
        for axis in FlipAxis::ALL {
            let grade = grade(axis, [2.0, 1.0], 100.0);
            let position = grade.sample_position(2.0, 1.0);
            assert!((position[0] - 2.0).abs() <= 1e-9);
            assert!((position[1] - 1.0).abs() <= 1e-9);
        }
    }

    #[test]
    fn a_half_blend_sits_between_the_two_frames() {
        let original = gradient_frame(WIDTH, HEIGHT);
        let mut pixels = original.clone();
        flip_frame_f32(
            &mut pixels,
            WIDTH,
            HEIGHT,
            &grade(FlipAxis::Vertical, [2.0, 1.0], 50.0),
        );
        let row = 1;
        let left = row * WIDTH;
        let right = row * WIDTH + WIDTH - 1;
        let expected = 0.5 * (original[left][0] + original[right][0]);
        assert!((pixels[left][0] - expected).abs() <= 1e-6);
    }

    #[test]
    fn sampling_outside_the_frame_clamps_to_the_edge() {
        let mut pixels = vec![[0.2, 0.4, 0.6, 1.0]; WIDTH * HEIGHT];
        let grade = grade(FlipAxis::Both, [-10.0, -10.0], 100.0);
        assert!(flip_frame_f32(&mut pixels, WIDTH, HEIGHT, &grade));
        for pixel in &pixels {
            assert!((pixel[0] - 0.2).abs() <= 1e-6);
            assert!((pixel[1] - 0.4).abs() <= 1e-6);
        }
    }

    #[test]
    fn transparent_neighbours_do_not_tint_visible_pixels() {
        let mut pixels = vec![[0.0, 0.0, 0.0, 0.0]; WIDTH * HEIGHT];
        pixels[2] = [0.4, 0.4, 0.4, 0.5];
        let grade = grade(FlipAxis::Vertical, [2.5, 0.0], 100.0);
        assert!(flip_frame_f32(&mut pixels, WIDTH, HEIGHT, &grade));
        let moved = 3;
        assert!((pixels[moved][3] - 0.5).abs() <= 1e-6);
        assert!((pixels[moved][0] - 0.4).abs() <= 1e-6);
    }

    #[test]
    fn extended_range_values_survive_a_flip() {
        let mut pixels = vec![[0.0, 0.0, 0.0, 1.0]; WIDTH * HEIGHT];
        pixels[4] = [2.5, -0.75, 1.25, 1.0];
        let grade = grade(FlipAxis::Vertical, [2.0, 0.0], 100.0);
        assert!(flip_frame_f32(&mut pixels, WIDTH, HEIGHT, &grade));
        let moved = 0;
        assert!((pixels[moved][0] - 2.5).abs() <= 1e-5);
        assert!((pixels[moved][1] + 0.75).abs() <= 1e-5);
    }

    #[test]
    fn the_whole_frame_entry_agrees_with_the_controlled_entry() {
        let mut through_entry = gradient_frame(WIDTH, HEIGHT);
        let mut through_control = gradient_frame(WIDTH, HEIGHT);
        let grade = grade(FlipAxis::Both, [1.5, 0.5], 75.0);
        let entry_changed = flip_frame_f32(&mut through_entry, WIDTH, HEIGHT, &grade);
        let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
        let control_changed =
            apply_flip_f32_controlled(&mut through_control, WIDTH, HEIGHT, &grade, &mut checkpoint)
                .expect("the checkpoint never fails");
        assert_eq!(entry_changed, control_changed);
        assert_eq!(through_entry, through_control);
    }

    #[test]
    fn empty_geometry_is_rejected() {
        let mut pixels: Vec<[f32; 4]> = Vec::new();
        let grade = grade(FlipAxis::Horizontal, [0.0, 0.0], 100.0);
        assert!(!flip_frame_f32(&mut pixels, 0, 0, &grade));
        assert_eq!(
            grade.sample_position(3.0, 4.0),
            [3.0, -4.0],
            "an axis through the origin mirrors the row coordinate"
        );
    }

    #[test]
    fn the_gpu_controls_carry_the_centre_weight_and_axis() {
        let grade = grade(FlipAxis::Vertical, [12.5, 7.25], 60.0);
        let (params, extra) = grade.gpu_controls();
        assert_eq!(params[0], 12.5);
        assert_eq!(params[1], 7.25);
        assert!((params[2] - 0.6).abs() <= 1e-6);
        assert_eq!(params[3], FlipAxis::Vertical.as_u32() as f32);
        assert_eq!(extra, [0.0; 4]);
    }
}
