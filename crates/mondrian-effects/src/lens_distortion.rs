//! Lens distortion correction and its inverse.
//!
//! The effect list gives a field of view, an inversion switch, a direction that
//! sets how the field of view is measured, a view centre, a best-pixels switch
//! and a resize mode, but it does not fix the projection. This module defines
//! one and documents it here so the CPU and the GPU agree and so the choice is
//! visible to anyone reading the effect:
//!
//! * the field of view spans the half extent chosen by the direction, so the
//!   focal length in pixels is `f = E / tan(fov / 2)`;
//! * a rectilinear sample at radius `r` corresponds to the angle
//!   `theta = atan(r / f)`, and the lens (fisheye) position of that sample is
//!   `f * theta`. Sampling the source at that radius therefore removes the
//!   barrel distortion and `invert` puts it back with `f * tan(r / f)`;
//! * a field of view of zero degrees is a plain identity;
//! * the resize mode scales the sampling radius so the frame keeps (or drops)
//!   its corners, and the best-pixels switch averages four sub samples.

use crate::resample::sample_bilinear_clamped;
use crate::straight_alpha::{premultiply_rgba_controlled, unpremultiply_rgba_controlled};

/// Largest field of view the effect accepts, in degrees.
pub const LENS_DISTORTION_MAX_FOV_DEGREES: f32 = 180.0;
/// Smallest field of view the effect accepts, in degrees.
pub const LENS_DISTORTION_MIN_FOV_DEGREES: f32 = 0.0;
/// Largest view centre coordinate the effect accepts, in pixels.
pub const LENS_DISTORTION_MAX_CENTRE_PIXELS: f32 = 1_000_000.0;
/// Number of view-centre directions the effect declares.
pub const LENS_DISTORTION_DIRECTION_COUNT: usize = 3;
/// Number of resize modes the effect declares.
pub const LENS_DISTORTION_RESIZE_COUNT: usize = 4;

/// Axis the field of view is measured across.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LensDistortionDirection {
    /// The field of view spans the frame width.
    Horizontal,
    /// The field of view spans the frame height.
    Vertical,
    /// The field of view spans the frame diagonal.
    Diagonal,
}

impl LensDistortionDirection {
    /// Every declared direction, in menu order.
    pub const ALL: [Self; LENS_DISTORTION_DIRECTION_COUNT] =
        [Self::Horizontal, Self::Vertical, Self::Diagonal];

    /// Stable authoring key.
    pub fn as_key(self) -> &'static str {
        match self {
            Self::Horizontal => "horizontal",
            Self::Vertical => "vertical",
            Self::Diagonal => "diagonal",
        }
    }

    /// Parses an authoring key.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|entry| entry.as_key() == key)
    }

    /// Menu value the effect list uses for this direction.
    pub fn as_menu(self) -> i64 {
        match self {
            Self::Horizontal => 1,
            Self::Vertical => 2,
            Self::Diagonal => 3,
        }
    }

    /// Parses a menu value.
    pub fn from_menu(value: i64) -> Option<Self> {
        Self::ALL.into_iter().find(|entry| entry.as_menu() == value)
    }

    /// Packed encoding used by the GPU controls.
    pub fn as_u32(self) -> u32 {
        self.as_menu() as u32
    }

    /// Half extent the field of view spans for a frame size.
    pub fn half_extent(self, width: usize, height: usize) -> f64 {
        let half_width = width as f64 * 0.5;
        let half_height = height as f64 * 0.5;
        match self {
            Self::Horizontal => half_width,
            Self::Vertical => half_height,
            Self::Diagonal => (half_width * half_width + half_height * half_height).sqrt(),
        }
    }
}

/// How the effect scales the frame to keep its corners.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LensDistortionResize {
    /// Leave the sampling scale alone.
    None,
    /// Scale up so the corners still sample inside the frame.
    Expand,
    /// Scale down so the frame fills the sampling area.
    Contract,
    /// Expand only when the distortion would pull the corners outside.
    Auto,
}

impl LensDistortionResize {
    /// Every declared resize mode, in menu order.
    pub const ALL: [Self; LENS_DISTORTION_RESIZE_COUNT] =
        [Self::None, Self::Expand, Self::Contract, Self::Auto];

    /// Stable authoring key.
    pub fn as_key(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Expand => "expand",
            Self::Contract => "contract",
            Self::Auto => "auto",
        }
    }

    /// Parses an authoring key.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|entry| entry.as_key() == key)
    }

    /// Menu value the effect list uses for this mode.
    pub fn as_menu(self) -> i64 {
        match self {
            Self::None => 1,
            Self::Expand => 2,
            Self::Contract => 3,
            Self::Auto => 4,
        }
    }

    /// Parses a menu value.
    pub fn from_menu(value: i64) -> Option<Self> {
        Self::ALL.into_iter().find(|entry| entry.as_menu() == value)
    }

    /// Packed encoding used by the GPU controls.
    pub fn as_u32(self) -> u32 {
        self.as_menu() as u32
    }
}

/// Reason a grade could not be built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LensDistortionError {
    /// The field of view is not a finite number inside the declared window.
    FieldOfView,
    /// A view-centre coordinate is not finite.
    Centre,
}

impl std::fmt::Display for LensDistortionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FieldOfView => write!(
                formatter,
                "lens distortion field of view must be finite and between {LENS_DISTORTION_MIN_FOV_DEGREES} and {LENS_DISTORTION_MAX_FOV_DEGREES}"
            ),
            Self::Centre => write!(
                formatter,
                "lens distortion view centre must be finite and within {LENS_DISTORTION_MAX_CENTRE_PIXELS} pixels"
            ),
        }
    }
}

impl std::error::Error for LensDistortionError {}

/// Resolved authoring state for one lens distortion application.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LensDistortionGrade {
    field_of_view: f32,
    invert: bool,
    direction: LensDistortionDirection,
    centre: [f32; 2],
    best_pixels: bool,
    resize: LensDistortionResize,
}

impl LensDistortionGrade {
    /// Builds a grade from authored values.
    pub fn new(
        field_of_view: f32,
        invert: bool,
        direction: LensDistortionDirection,
        centre: [f32; 2],
        best_pixels: bool,
        resize: LensDistortionResize,
    ) -> Result<Self, LensDistortionError> {
        if !field_of_view.is_finite()
            || !(LENS_DISTORTION_MIN_FOV_DEGREES..=LENS_DISTORTION_MAX_FOV_DEGREES)
                .contains(&field_of_view)
        {
            return Err(LensDistortionError::FieldOfView);
        }
        for axis in centre {
            if !axis.is_finite() || axis.abs() > LENS_DISTORTION_MAX_CENTRE_PIXELS {
                return Err(LensDistortionError::Centre);
            }
        }
        Ok(Self {
            field_of_view,
            invert,
            direction,
            centre,
            best_pixels,
            resize,
        })
    }

    /// Authored field of view, in degrees.
    pub fn field_of_view(&self) -> f32 {
        self.field_of_view
    }

    /// Whether the distortion is put back instead of removed.
    pub fn inverts(&self) -> bool {
        self.invert
    }

    /// Axis the field of view is measured across.
    pub fn direction(&self) -> LensDistortionDirection {
        self.direction
    }

    /// Authored view centre, in pixels.
    pub fn centre(&self) -> [f32; 2] {
        self.centre
    }

    /// Whether four sub samples are averaged per output pixel.
    pub fn uses_best_pixels(&self) -> bool {
        self.best_pixels
    }

    /// Resize mode applied to the sampling scale.
    pub fn resize(&self) -> LensDistortionResize {
        self.resize
    }

    /// A zero field of view leaves the frame exactly as it is.
    pub fn is_identity(&self) -> bool {
        self.field_of_view <= 1.0e-6
    }

    /// Focal length in pixels for a frame size, or `None` when the field of view
    /// is too small to define one.
    pub fn focal_length(&self, width: usize, height: usize) -> Option<f64> {
        if self.is_identity() {
            return None;
        }
        let half_extent = self.direction.half_extent(width, height);
        let half_angle = f64::from(self.field_of_view) * std::f64::consts::PI / 360.0;
        let tangent = half_angle.tan();
        if !tangent.is_finite() || tangent <= 1.0e-9 {
            return None;
        }
        Some(half_extent / tangent)
    }

    /// Scale applied to the sampling radius so the corners behave as authored.
    pub fn resize_scale(&self, width: usize, height: usize) -> f64 {
        if matches!(self.resize, LensDistortionResize::None) {
            return 1.0;
        }
        let Some(focal) = self.focal_length(width, height) else {
            return 1.0;
        };
        let half_width = width as f64 * 0.5;
        let half_height = height as f64 * 0.5;
        let corner = (half_width * half_width + half_height * half_height).sqrt();
        if corner <= 0.0 {
            return 1.0;
        }
        let mapped = self.map_radius(corner, focal);
        if mapped <= 1.0e-9 {
            return 1.0;
        }
        let gain = corner / mapped;
        match self.resize {
            LensDistortionResize::None => 1.0,
            LensDistortionResize::Expand => gain,
            LensDistortionResize::Contract => mapped / corner,
            LensDistortionResize::Auto => gain.max(1.0),
        }
    }

    /// Maps an undistorted radius onto the lens radius, or back when inverting.
    fn map_radius(&self, radius: f64, focal: f64) -> f64 {
        if self.invert {
            focal * (radius / focal).tan()
        } else {
            focal * (radius / focal).atan()
        }
    }

    /// Source coordinate a destination sample reads from.
    pub fn sample_position(&self, column: f64, row: f64, width: usize, height: usize) -> [f64; 2] {
        let centre_x = f64::from(self.centre[0]);
        let centre_y = f64::from(self.centre[1]);
        let delta_x = column - centre_x;
        let delta_y = row - centre_y;
        let radius = (delta_x * delta_x + delta_y * delta_y).sqrt();
        let Some(focal) = self.focal_length(width, height) else {
            return [column, row];
        };
        if radius <= 1.0e-9 {
            return [centre_x, centre_y];
        }
        let scale = self.resize_scale(width, height);
        let mapped = self.map_radius(radius, focal) * scale;
        let factor = mapped / radius;
        [centre_x + delta_x * factor, centre_y + delta_y * factor]
    }

    /// Reads one destination sample from the source frame.
    fn sample(
        &self,
        source: &[[f32; 4]],
        width: usize,
        height: usize,
        column: f64,
        row: f64,
    ) -> [f32; 4] {
        let [sample_x, sample_y] = self.sample_position(column, row, width, height);
        sample_bilinear_clamped(source, width, height, sample_x, sample_y)
    }

    /// Applies the distortion to a premultiplied frame.
    pub fn apply(&self, frame: &mut [[f32; 4]], width: usize, height: usize) {
        if self.is_identity() || width == 0 || height == 0 {
            return;
        }
        let source = frame.to_vec();
        let taps = if self.best_pixels { 2 } else { 1 };
        let step = 1.0 / taps as f64;
        let weight = 1.0 / (taps * taps) as f32;
        for row in 0..height {
            for column in 0..width {
                let mut accumulated = [0.0_f32; 4];
                for sub_row in 0..taps {
                    for sub_column in 0..taps {
                        let sample_x = column as f64 + (sub_column as f64 + 0.5) * step - 0.5;
                        let sample_y = row as f64 + (sub_row as f64 + 0.5) * step - 0.5;
                        let sample = self.sample(&source, width, height, sample_x, sample_y);
                        for channel in 0..4 {
                            accumulated[channel] += sample[channel] * weight;
                        }
                    }
                }
                frame[row * width + column] = accumulated;
            }
        }
    }

    /// Packed GPU controls: `params` and `extra`.
    pub fn gpu_controls(&self) -> ([f32; 4], [f32; 4]) {
        (
            [
                self.centre[0],
                self.centre[1],
                self.field_of_view,
                if self.invert { 1.0 } else { 0.0 },
            ],
            [
                self.direction.as_u32() as f32,
                self.resize.as_u32() as f32,
                if self.best_pixels { 1.0 } else { 0.0 },
                0.0,
            ],
        )
    }
}

/// Applies the grade to a straight-alpha frame.
pub fn lens_distortion_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: LensDistortionGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_lens_distortion_f32_controlled(pixels, width, height, grade, &mut checkpoint)
        .unwrap_or(false)
}

/// Applies the grade to a straight-alpha frame, reporting a geometry mismatch as
/// `false`.
pub(crate) fn apply_lens_distortion_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: LensDistortionGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(expected) = width.checked_mul(height) else {
        return Ok(false);
    };
    if expected == 0 || width < 2 || height < 2 || pixels.len() != expected {
        return Ok(false);
    }
    checkpoint()?;
    premultiply_rgba_controlled(pixels, checkpoint)?;
    grade.apply(pixels, width, height);
    unpremultiply_rgba_controlled(pixels, checkpoint)?;
    checkpoint()?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grade(field_of_view: f32) -> LensDistortionGrade {
        LensDistortionGrade::new(
            field_of_view,
            false,
            LensDistortionDirection::Diagonal,
            [5.0, 4.0],
            false,
            LensDistortionResize::None,
        )
        .expect("grade")
    }

    fn gradient_frame(width: usize, height: usize) -> Vec<[f32; 4]> {
        let mut pixels = vec![[0.0_f32; 4]; width * height];
        for row in 0..height {
            for column in 0..width {
                let value = 0.04 * column as f32 + 0.03 * row as f32;
                pixels[row * width + column] = [value, 1.0 - value, 0.25 + value * 0.5, 1.0];
            }
        }
        pixels
    }

    #[test]
    fn the_grade_rejects_a_field_of_view_outside_the_declared_window() {
        for value in [-1.0, 181.0, f32::NAN, f32::INFINITY] {
            assert_eq!(
                LensDistortionGrade::new(
                    value,
                    false,
                    LensDistortionDirection::Diagonal,
                    [0.0, 0.0],
                    false,
                    LensDistortionResize::None,
                )
                .expect_err("field of view must be rejected"),
                LensDistortionError::FieldOfView
            );
        }
    }

    #[test]
    fn the_grade_rejects_a_centre_that_is_not_finite() {
        assert_eq!(
            LensDistortionGrade::new(
                40.0,
                false,
                LensDistortionDirection::Diagonal,
                [f32::NAN, 0.0],
                false,
                LensDistortionResize::None,
            )
            .expect_err("centre must be rejected"),
            LensDistortionError::Centre
        );
    }

    #[test]
    fn directions_and_resize_modes_round_trip_through_their_keys_and_menus() {
        for direction in LensDistortionDirection::ALL {
            assert_eq!(
                LensDistortionDirection::from_key(direction.as_key()),
                Some(direction)
            );
            assert_eq!(
                LensDistortionDirection::from_menu(direction.as_menu()),
                Some(direction)
            );
            assert_eq!(direction.as_u32() as i64, direction.as_menu());
        }
        for resize in LensDistortionResize::ALL {
            assert_eq!(
                LensDistortionResize::from_key(resize.as_key()),
                Some(resize)
            );
            assert_eq!(
                LensDistortionResize::from_menu(resize.as_menu()),
                Some(resize)
            );
        }
        assert_eq!(
            LensDistortionDirection::ALL.len(),
            LENS_DISTORTION_DIRECTION_COUNT
        );
        assert_eq!(
            LensDistortionResize::ALL.len(),
            LENS_DISTORTION_RESIZE_COUNT
        );
        assert_eq!(LensDistortionDirection::from_menu(4), None);
        assert_eq!(LensDistortionResize::from_menu(5), None);
    }

    #[test]
    fn a_zero_field_of_view_is_an_identity() {
        let grade = grade(0.0);
        assert!(grade.is_identity());
        assert_eq!(grade.focal_length(11, 9), None);
        assert_eq!(grade.sample_position(3.0, 2.0, 11, 9), [3.0, 2.0]);
        let original = gradient_frame(11, 9);
        let mut pixels = original.clone();
        assert!(lens_distortion_frame_f32(&mut pixels, 11, 9, grade));
        assert_eq!(pixels, original);
    }

    #[test]
    fn the_view_centre_is_a_fixed_point() {
        let grade = grade(90.0);
        let centre = grade.centre();
        let position = grade.sample_position(f64::from(centre[0]), f64::from(centre[1]), 11, 9);
        assert!((position[0] - f64::from(centre[0])).abs() <= 1e-9);
        assert!((position[1] - f64::from(centre[1])).abs() <= 1e-9);
    }

    #[test]
    fn correcting_pulls_samples_towards_the_centre_and_inverting_pushes_them_out() {
        let centre = [5.0_f32, 4.0];
        let correcting = LensDistortionGrade::new(
            100.0,
            false,
            LensDistortionDirection::Diagonal,
            centre,
            false,
            LensDistortionResize::None,
        )
        .expect("correcting grade");
        let inverting = LensDistortionGrade::new(
            100.0,
            true,
            LensDistortionDirection::Diagonal,
            centre,
            false,
            LensDistortionResize::None,
        )
        .expect("inverting grade");
        let radius_of = |grade: &LensDistortionGrade| {
            let [x, y] = grade.sample_position(10.0, 4.0, 11, 9);
            ((x - 5.0).powi(2) + (y - 4.0).powi(2)).sqrt()
        };
        let corrected = radius_of(&correcting);
        let inverted = radius_of(&inverting);
        assert!(corrected < 5.0, "correcting must pull in, got {corrected}");
        assert!(inverted > 5.0, "inverting must push out, got {inverted}");
    }

    #[test]
    fn the_mapped_radius_matches_an_independent_projection() {
        let grade = LensDistortionGrade::new(
            80.0,
            false,
            LensDistortionDirection::Horizontal,
            [0.0, 0.0],
            false,
            LensDistortionResize::None,
        )
        .expect("grade");
        let half_extent = 24.0_f64;
        let half_angle = 80.0_f64 * std::f64::consts::PI / 360.0;
        let focal = half_extent / half_angle.tan();
        for radius in [1.0_f64, 4.5, 12.0] {
            let expected = focal * (radius / focal).atan();
            let [x, y] = grade.sample_position(radius, 0.0, 48, 32);
            let mapped = (x * x + y * y).sqrt();
            assert!(
                (mapped - expected).abs() <= 1e-9,
                "radius {radius}: expected {expected}, got {mapped}"
            );
        }
    }

    #[test]
    fn the_resize_modes_scale_the_sampling_radius() {
        let build = |resize| {
            LensDistortionGrade::new(
                120.0,
                false,
                LensDistortionDirection::Diagonal,
                [5.0, 4.0],
                false,
                resize,
            )
            .expect("grade")
        };
        assert!((build(LensDistortionResize::None).resize_scale(11, 9) - 1.0).abs() <= 1e-12);
        let expanded = build(LensDistortionResize::Expand).resize_scale(11, 9);
        let contracted = build(LensDistortionResize::Contract).resize_scale(11, 9);
        let automatic = build(LensDistortionResize::Auto).resize_scale(11, 9);
        assert!(expanded > 1.0, "expanding must scale up, got {expanded}");
        assert!(
            contracted < 1.0,
            "contracting must scale down, got {contracted}"
        );
        assert!((expanded * contracted - 1.0).abs() <= 1e-9);
        assert!(automatic >= 1.0);
    }

    #[test]
    fn a_corrected_frame_keeps_its_opaque_coverage() {
        let grade = grade(60.0);
        let mut pixels = gradient_frame(11, 9);
        assert!(lens_distortion_frame_f32(&mut pixels, 11, 9, grade));
        for pixel in &pixels {
            assert!((pixel[3] - 1.0).abs() <= 1e-6, "coverage {pixel:?}");
        }
    }

    #[test]
    fn transparent_neighbours_do_not_darken_the_visible_samples() {
        let grade = grade(140.0);
        let width = 9;
        let height = 7;
        let mut pixels = vec![[0.4_f32, 0.2, 0.6, 1.0]; width * height];
        for index in [0, 1, 7, 8] {
            pixels[index] = [0.0, 0.0, 0.0, 0.0];
        }
        assert!(lens_distortion_frame_f32(&mut pixels, width, height, grade));
        let sample = pixels[(height / 2) * width + width / 2];
        assert!(
            sample[0] > 0.3,
            "visible colour must survive, got {sample:?}"
        );
    }

    #[test]
    fn extended_range_samples_are_not_clamped() {
        let grade = grade(30.0);
        let width = 7;
        let height = 5;
        let mut pixels = vec![[0.0_f32; 4]; width * height];
        pixels[2 * width + 3] = [2.5, -0.75, 1.25, 1.0];
        pixels[2 * width + 4] = [2.5, -0.75, 1.25, 1.0];
        assert!(lens_distortion_frame_f32(&mut pixels, width, height, grade));
        let brightest = pixels.iter().map(|pixel| pixel[0]).fold(f32::MIN, f32::max);
        assert!(
            brightest > 1.0,
            "extended range must survive, got {brightest}"
        );
    }

    #[test]
    fn best_pixels_averages_sub_samples_without_changing_the_result_shape() {
        let coarse = LensDistortionGrade::new(
            70.0,
            false,
            LensDistortionDirection::Diagonal,
            [5.0, 4.0],
            false,
            LensDistortionResize::None,
        )
        .expect("grade");
        let fine = LensDistortionGrade::new(
            70.0,
            false,
            LensDistortionDirection::Diagonal,
            [5.0, 4.0],
            true,
            LensDistortionResize::None,
        )
        .expect("grade");
        let mut first = gradient_frame(11, 9);
        let mut second = first.clone();
        assert!(lens_distortion_frame_f32(&mut first, 11, 9, coarse));
        assert!(lens_distortion_frame_f32(&mut second, 11, 9, fine));
        for (left, right) in first.iter().zip(second.iter()) {
            for channel in 0..4 {
                assert!(
                    (left[channel] - right[channel]).abs() <= 0.25,
                    "sub samples drifted too far: {left:?} vs {right:?}"
                );
            }
        }
    }

    #[test]
    fn a_geometry_mismatch_is_refused() {
        let mut pixels = gradient_frame(4, 3);
        let grade = grade(60.0);
        assert!(!lens_distortion_frame_f32(&mut pixels, 5, 3, grade));
        assert!(!lens_distortion_frame_f32(&mut pixels, 4, 4, grade));
        assert!(!lens_distortion_frame_f32(&mut pixels, 0, 0, grade));
    }

    #[test]
    fn the_controls_pack_the_authored_state() {
        let grade = LensDistortionGrade::new(
            82.5,
            true,
            LensDistortionDirection::Vertical,
            [11.0, 7.5],
            true,
            LensDistortionResize::Contract,
        )
        .expect("grade");
        let (params, extra) = grade.gpu_controls();
        assert_eq!(params, [11.0, 7.5, 82.5, 1.0]);
        assert_eq!(extra, [2.0, 3.0, 1.0, 0.0]);
    }
}
