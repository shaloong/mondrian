//! Twirl distortion (DST-04 in the effect list).
//!
//! The effect rotates the frame around a centre with a smooth falloff: the
//! full authored angle applies at the centre and decays to nothing at the rim,
//! so the picture appears to swirl into a vortex. The mapping is a pure inverse
//! resampling — destination pixel in, source pixel out — which keeps the
//! kernel free of state and independent of the processing order.
//!
//! The falloff is `(1 - d^2 / r^2)^2` on the normalised distance, which is
//! `C^1` at the rim: the displacement and its first derivative both reach zero
//! there, so no seam or brightness step appears at the boundary of the vortex.
//! The kernel works on premultiplied colour in the working scene-linear space
//! and never clamps colour; coverage is capped at one by the shared
//! unpremultiply helper.

use crate::resample::sample_bilinear_clamped;
use crate::straight_alpha::{premultiply_rgba_controlled, unpremultiply_rgba_controlled};

/// Largest authored radius, as a percentage of the shorter frame side.
pub const TWIRL_MAX_RADIUS_PERCENT: f32 = 100.0;

/// Smallest authored radius.
pub const TWIRL_MIN_RADIUS_PERCENT: f32 = 0.0;

/// Largest magnitude accepted for the authored angle, in degrees.
///
/// The list leaves the angle unbounded. The cap keeps the product of the angle
/// and the falloff exact enough to round-trip between paths in `f32`.
pub const TWIRL_MAX_ANGLE_DEGREES: f32 = 360_000.0;

/// Errors raised when author values cannot describe an executable twirl.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TwirlError {
    /// The angle is not finite or exceeds the accepted magnitude.
    Angle,
    /// The radius is not finite or is outside the declared range.
    Radius,
    /// The centre is not a finite point.
    Centre,
}

impl std::fmt::Display for TwirlError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Angle => write!(
                formatter,
                "twirl angle must be finite and within +/-{TWIRL_MAX_ANGLE_DEGREES} degrees"
            ),
            Self::Radius => write!(
                formatter,
                "twirl radius must be within {TWIRL_MIN_RADIUS_PERCENT}..{TWIRL_MAX_RADIUS_PERCENT} percent"
            ),
            Self::Centre => formatter.write_str("twirl centre must be a finite point"),
        }
    }
}

impl std::error::Error for TwirlError {}

/// Authored twirl state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TwirlGrade {
    angle_degrees: f32,
    radius_fraction: f32,
    centre: [f32; 2],
}

impl TwirlGrade {
    /// Builds a grade, rejecting state the declared ranges cannot express.
    pub fn new(
        angle_degrees: f32,
        radius_percent: f32,
        centre: [f32; 2],
    ) -> Result<Self, TwirlError> {
        if !angle_degrees.is_finite() || angle_degrees.abs() > TWIRL_MAX_ANGLE_DEGREES {
            return Err(TwirlError::Angle);
        }
        if !radius_percent.is_finite()
            || !(TWIRL_MIN_RADIUS_PERCENT..=TWIRL_MAX_RADIUS_PERCENT).contains(&radius_percent)
        {
            return Err(TwirlError::Radius);
        }
        if !centre[0].is_finite() || !centre[1].is_finite() {
            return Err(TwirlError::Centre);
        }
        Ok(Self {
            angle_degrees,
            radius_fraction: radius_percent / TWIRL_MAX_RADIUS_PERCENT,
            centre,
        })
    }

    /// Rotation applied at the centre, in degrees.
    pub fn angle_degrees(&self) -> f32 {
        self.angle_degrees
    }

    /// Radius of influence as a percentage of the shorter frame side.
    pub fn radius_percent(&self) -> f32 {
        self.radius_fraction * TWIRL_MAX_RADIUS_PERCENT
    }

    /// Radius of influence as a fraction of the shorter frame side.
    pub fn radius_fraction(&self) -> f32 {
        self.radius_fraction
    }

    /// Centre of the vortex in frame pixels.
    pub fn centre(&self) -> [f32; 2] {
        self.centre
    }

    /// `true` when the effect cannot change any pixel.
    pub fn is_identity(&self) -> bool {
        self.angle_degrees.abs() <= 1.0e-6 || self.radius_fraction <= 0.0
    }

    /// Radius of influence in pixels for a frame of `width` x `height`.
    pub fn radius_pixels(&self, width: usize, height: usize) -> f32 {
        self.radius_fraction * width.min(height) as f32
    }

    /// Uniform packing for the GPU pass:
    /// `[centre_x, centre_y, radius_fraction, angle_degrees]`.
    pub fn gpu_controls(&self) -> [f32; 4] {
        [
            self.centre[0],
            self.centre[1],
            self.radius_fraction,
            self.angle_degrees,
        ]
    }
}

/// Applies the twirl to a whole frame, returning `false` when the frame cannot
/// be processed.
pub fn twirl_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: TwirlGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_twirl_f32_controlled(pixels, width, height, grade, &mut checkpoint).unwrap_or(false)
}

/// Applies the twirl in place with an explicit cancellation checkpoint.
pub(crate) fn apply_twirl_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: TwirlGrade,
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

    let frame = &mut working[..pixel_count];
    premultiply_rgba_controlled(frame, checkpoint)?;

    let source = frame.to_vec();
    let centre_x = f64::from(grade.centre[0]);
    let centre_y = f64::from(grade.centre[1]);
    let radius = f64::from(grade.radius_pixels(width, height));
    let angle = f64::from(grade.angle_degrees).to_radians();

    checkpoint()?;
    let rows = frame.len() / width;
    crate::parallel::for_each_row_band(frame, width, 0..rows, |row_start, band| {
        for (offset, chunk) in band.chunks_mut(width).enumerate() {
            let row = row_start + offset;
            for (column, pixel) in chunk.iter_mut().enumerate() {
                let delta_x = column as f64 - centre_x;
                let delta_y = row as f64 - centre_y;
                let distance = delta_x.hypot(delta_y);
                if radius <= 0.0 || distance >= radius {
                    continue;
                }
                let normalized = distance / radius;
                let falloff = (1.0 - normalized * normalized).powi(2);
                let rotation = angle * falloff;
                let (sin, cos) = rotation.sin_cos();
                let sample_x = centre_x + delta_x * cos + delta_y * sin;
                let sample_y = centre_y - delta_x * sin + delta_y * cos;
                *pixel = sample_bilinear_clamped(&source, width, height, sample_x, sample_y);
            }
        }
    });

    unpremultiply_rgba_controlled(frame, checkpoint)?;
    checkpoint()?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(width: usize, height: usize, counter: usize) -> Vec<[f32; 4]> {
        let mut pixels = vec![[0.0_f32; 4]; width * height];
        for (index, pixel) in pixels.iter_mut().enumerate() {
            let phase = (index * counter % 17) as f32 * 0.041;
            *pixel = [phase, 0.3 + phase * 0.5, 0.85 - phase, 1.0];
        }
        pixels
    }

    fn grade(angle: f32, radius_percent: f32, centre: [f32; 2]) -> TwirlGrade {
        TwirlGrade::new(angle, radius_percent, centre).expect("valid twirl grade")
    }

    #[test]
    fn rejects_author_state_outside_the_declared_ranges() {
        assert_eq!(
            TwirlGrade::new(f32::NAN, 30.0, [1.0, 1.0]),
            Err(TwirlError::Angle)
        );
        assert_eq!(
            TwirlGrade::new(720_001.0, 30.0, [1.0, 1.0]),
            Err(TwirlError::Angle)
        );
        assert_eq!(
            TwirlGrade::new(90.0, -1.0, [1.0, 1.0]),
            Err(TwirlError::Radius)
        );
        assert_eq!(
            TwirlGrade::new(90.0, 101.0, [1.0, 1.0]),
            Err(TwirlError::Radius)
        );
        assert_eq!(
            TwirlGrade::new(90.0, 30.0, [f32::INFINITY, 1.0]),
            Err(TwirlError::Centre)
        );
    }

    #[test]
    fn zero_angle_and_zero_radius_are_identity() {
        assert!(grade(0.0, 30.0, [4.0, 4.0]).is_identity());
        assert!(grade(90.0, 0.0, [4.0, 4.0]).is_identity());
        assert!(!grade(90.0, 30.0, [4.0, 4.0]).is_identity());

        let original = frame(9, 7, 3);
        let mut pixels = original.clone();
        assert!(twirl_frame_f32(
            &mut pixels,
            9,
            7,
            grade(0.0, 50.0, [4.0, 3.0])
        ));
        assert_eq!(pixels, original);
    }

    #[test]
    fn radius_percent_scales_with_the_shorter_frame_side() {
        let grade = grade(90.0, 40.0, [0.0, 0.0]);
        assert!((grade.radius_pixels(100, 50) - 20.0).abs() < 1.0e-5);
        assert!((grade.radius_pixels(50, 100) - 20.0).abs() < 1.0e-5);
        assert!((grade.radius_pixels(50, 50) - 20.0).abs() < 1.0e-5);
    }

    #[test]
    fn the_centre_pixel_keeps_its_own_sample() {
        let mut pixels = frame(9, 9, 5);
        let original = pixels.clone();
        assert!(twirl_frame_f32(
            &mut pixels,
            9,
            9,
            grade(180.0, 100.0, [4.0, 4.0])
        ));
        assert_eq!(pixels[4 * 9 + 4], original[4 * 9 + 4]);
    }

    #[test]
    fn pixels_beyond_the_radius_are_untouched() {
        let mut pixels = frame(11, 11, 7);
        let original = pixels.clone();
        // Radius 30% of the shorter side (11 px) is 3.3 px around the centre.
        assert!(twirl_frame_f32(
            &mut pixels,
            11,
            11,
            grade(120.0, 30.0, [5.0, 5.0])
        ));
        for (index, (before, after)) in original.iter().zip(pixels.iter()).enumerate() {
            let column = index % 11;
            let row = index / 11;
            let distance = ((column as f32 - 5.0).powi(2) + (row as f32 - 5.0).powi(2)).sqrt();
            if distance >= 3.3 {
                assert_eq!(before, after, "pixel {index} outside the radius moved");
            }
        }
    }

    #[test]
    fn the_mapping_matches_an_independent_rotation_evaluation() {
        let width = 9;
        let height = 9;
        let pixels = frame(width, height, 11);
        let mut twisted = pixels.clone();
        let centre = [4.0_f32, 4.0];
        let grade = grade(90.0, 100.0, centre);
        assert!(twirl_frame_f32(&mut twisted, width, height, grade));

        let radius = grade.radius_pixels(width, height) as f64;
        let angle = 90.0_f64.to_radians();
        for (column, row) in [(6_usize, 4_usize), (4, 6), (5, 5), (2, 3)] {
            let delta_x = column as f64 - f64::from(centre[0]);
            let delta_y = row as f64 - f64::from(centre[1]);
            let distance = delta_x.hypot(delta_y);
            let normalized = distance / radius;
            let rotation = angle * (1.0 - normalized * normalized).powi(2);
            let (sin, cos) = rotation.sin_cos();
            let sample_x = f64::from(centre[0]) + delta_x * cos + delta_y * sin;
            let sample_y = f64::from(centre[1]) - delta_x * sin + delta_y * cos;
            let expected = independent_bilinear(&pixels, width, height, sample_x, sample_y);
            let actual = twisted[row * width + column];
            for channel in 0..4 {
                let difference = (expected[channel] - actual[channel]).abs();
                assert!(
                    difference <= 2.0e-5,
                    "pixel ({column}, {row}) channel {channel}: expected {expected:?} got {actual:?}"
                );
            }
        }
    }

    /// Bit-for-bit independent bilinear evaluation written against the plain
    /// definition rather than the kernel helper.
    fn independent_bilinear(
        pixels: &[[f32; 4]],
        width: usize,
        height: usize,
        x: f64,
        y: f64,
    ) -> [f32; 4] {
        let x = x.clamp(0.0, (width - 1) as f64);
        let y = y.clamp(0.0, (height - 1) as f64);
        let x0 = x.floor();
        let y0 = y.floor();
        let fx = (x - x0) as f32;
        let fy = (y - y0) as f32;
        let x0 = x0 as usize;
        let y0 = y0 as usize;
        let x1 = (x0 + 1).min(width - 1);
        let y1 = (y0 + 1).min(height - 1);
        let mut result = [0.0_f32; 4];
        for channel in 0..4 {
            let a = pixels[y0 * width + x0][channel];
            let b = pixels[y0 * width + x1][channel];
            let c = pixels[y1 * width + x0][channel];
            let d = pixels[y1 * width + x1][channel];
            let top = a * (1.0 - fx) + b * fx;
            let bottom = c * (1.0 - fx) + d * fx;
            result[channel] = top * (1.0 - fy) + bottom * fy;
        }
        result
    }
    #[test]
    fn transparent_pixels_do_not_tint_their_neighbours() {
        let width = 9;
        let height = 9;
        let mut pixels = vec![[0.6, 0.2, 0.9, 1.0]; width * height];
        pixels[4 * width + 4] = [0.0, 0.0, 0.0, 0.0];
        assert!(twirl_frame_f32(
            &mut pixels,
            width,
            height,
            grade(45.0, 100.0, [4.0, 4.0])
        ));
        // Every visible pixel keeps its colour: the transparent centre
        // contributes no colour because the kernel works premultiplied.
        for (index, pixel) in pixels.iter().enumerate() {
            if index == 4 * width + 4 {
                assert_eq!(pixel[3], 0.0);
                continue;
            }
            assert!(pixel[3] > 0.0);
            for channel in 0..3 {
                let difference = (pixel[channel] - [0.6, 0.2, 0.9][channel]).abs();
                assert!(difference <= 1.0e-3, "pixel {index} picked up tint");
            }
        }
    }

    #[test]
    fn extended_range_values_are_not_clamped() {
        let width = 7;
        let height = 7;
        let mut pixels = vec![[1.0_f32, 1.0, 1.0, 1.0]; width * height];
        for pixel in pixels.iter_mut() {
            *pixel = [3.5, -0.75, 0.5, 1.0];
        }
        assert!(twirl_frame_f32(
            &mut pixels,
            width,
            height,
            grade(30.0, 100.0, [3.0, 3.0])
        ));
        assert!(pixels.iter().any(|pixel| pixel[0] > 1.0));
        assert!(pixels.iter().any(|pixel| pixel[1] < 0.0));
    }

    #[test]
    fn the_same_input_twice_produces_the_same_output() {
        let mut first = frame(9, 8, 13);
        let mut second = first.clone();
        let grade = grade(75.0, 80.0, [4.0, 4.0]);
        assert!(twirl_frame_f32(&mut first, 9, 8, grade));
        assert!(twirl_frame_f32(&mut second, 9, 8, grade));
        assert_eq!(first, second);
    }

    #[test]
    fn mismatched_dimensions_are_rejected() {
        let mut pixels = vec![[0.0_f32; 4]; 6];
        assert!(!twirl_frame_f32(
            &mut pixels,
            9,
            9,
            grade(45.0, 50.0, [4.0, 4.0])
        ));
    }

    #[test]
    fn gpu_controls_carry_the_centre_radius_and_angle() {
        let grade = grade(64.0, 25.0, [12.5, 7.25]);
        let controls = grade.gpu_controls();
        assert_eq!(controls, [12.5, 7.25, 0.25, 64.0]);
    }
}
