//! Spherize distortion (DST-05 in the effect list).
//!
//! The effect bulges a circular region of the frame into a sphere: content is
//! magnified where the sphere faces the viewer and compressed towards the rim,
//! so the region reads as a lens rather than as a plain zoom.
//!
//! The mapping is a pure inverse resampling — destination pixel in, source
//! pixel out — so the kernel holds no state and its result is independent of
//! the processing order. For a pixel at distance `d` from the centre and a
//! sphere of radius `r`, the sample distance is
//!
//! ```text
//! d' = r * asin(clamp(d / r, 0, 1)) / (pi / 2)
//! ```
//!
//! which is monotone, fixes `d = 0` (the pole stays the pole) and fixes
//! `d = r` (the rim stays the rim), so the bulge joins the untouched image
//! without a seam. `asin` grows faster than the identity close to the centre,
//! which is exactly the magnification the effect describes, while the
//! derivative blows up at the rim, which is the compression.
//!
//! The kernel works on premultiplied colour in the working scene-linear space
//! and never clamps colour; coverage is capped at one by the shared
//! unpremultiply helper.

use crate::straight_alpha::{premultiply_rgba_controlled, unpremultiply_rgba_controlled};

/// Largest authored radius, in frame pixels.
pub const SPHERIZE_MAX_RADIUS_PIXELS: f32 = 2_500.0;

/// Smallest authored radius.
pub const SPHERIZE_MIN_RADIUS_PIXELS: f32 = 0.0;

/// Errors raised when author values cannot describe an executable spherize.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpherizeError {
    /// The radius is not finite or is outside the declared range.
    Radius,
    /// The centre is not a finite point.
    Centre,
}

impl std::fmt::Display for SpherizeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Radius => write!(
                formatter,
                "spherize radius must be within {SPHERIZE_MIN_RADIUS_PIXELS}..{SPHERIZE_MAX_RADIUS_PIXELS} pixels"
            ),
            Self::Centre => formatter.write_str("spherize centre must be a finite point"),
        }
    }
}

impl std::error::Error for SpherizeError {}

/// Authored spherize state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpherizeGrade {
    radius_pixels: f32,
    centre: [f32; 2],
}

impl SpherizeGrade {
    /// Builds a grade, rejecting state the declared ranges cannot express.
    pub fn new(radius_pixels: f32, centre: [f32; 2]) -> Result<Self, SpherizeError> {
        if !radius_pixels.is_finite()
            || !(SPHERIZE_MIN_RADIUS_PIXELS..=SPHERIZE_MAX_RADIUS_PIXELS).contains(&radius_pixels)
        {
            return Err(SpherizeError::Radius);
        }
        if !centre[0].is_finite() || !centre[1].is_finite() {
            return Err(SpherizeError::Centre);
        }
        Ok(Self { radius_pixels, centre })
    }

    /// Radius of the sphere in frame pixels.
    pub fn radius_pixels(&self) -> f32 {
        self.radius_pixels
    }

    /// Centre of the sphere in frame pixels.
    pub fn centre(&self) -> [f32; 2] {
        self.centre
    }

    /// `true` when the effect cannot change any pixel.
    pub fn is_identity(&self) -> bool {
        self.radius_pixels <= 1.0e-6
    }

    /// Uniform packing for the GPU pass: `[centre_x, centre_y, radius, 0]`.
    pub fn gpu_controls(&self) -> [f32; 4] {
        [self.centre[0], self.centre[1], self.radius_pixels, 0.0]
    }
}

/// Applies the spherize to a whole frame, returning `false` when the frame
/// cannot be processed.
pub fn spherize_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: SpherizeGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_spherize_f32_controlled(pixels, width, height, grade, &mut checkpoint).unwrap_or(false)
}

/// Applies the spherize in place with an explicit cancellation checkpoint.
pub(crate) fn apply_spherize_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: SpherizeGrade,
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
    let radius = f64::from(grade.radius_pixels);
    let half_pi = std::f64::consts::FRAC_PI_2;

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
                if distance <= f64::EPSILON {
                    continue;
                }
                let sample_distance = radius * (distance / radius).asin() / half_pi;
                let scale = sample_distance / distance;
                let sample_x = centre_x + delta_x * scale;
                let sample_y = centre_y + delta_y * scale;
                *pixel = sample_bilinear_clamped(&source, width, height, sample_x, sample_y);
            }
        }
    });

    unpremultiply_rgba_controlled(frame, checkpoint)?;
    checkpoint()?;
    Ok(true)
}

/// Bilinear sample whose integer coordinates clamp to the frame edges.
fn sample_bilinear_clamped(
    source: &[[f32; 4]],
    width: usize,
    height: usize,
    x: f64,
    y: f64,
) -> [f32; 4] {
    let limit_x = (width - 1) as f64;
    let limit_y = (height - 1) as f64;
    let clamped_x = x.clamp(0.0, limit_x);
    let clamped_y = y.clamp(0.0, limit_y);
    let base_x = clamped_x.floor();
    let base_y = clamped_y.floor();
    let frac_x = (clamped_x - base_x) as f32;
    let frac_y = (clamped_y - base_y) as f32;
    let x0 = base_x as usize;
    let y0 = base_y as usize;
    let x1 = (x0 + 1).min(width - 1);
    let y1 = (y0 + 1).min(height - 1);

    let top_left = source[y0 * width + x0];
    let top_right = source[y0 * width + x1];
    let bottom_left = source[y1 * width + x0];
    let bottom_right = source[y1 * width + x1];

    let mut sample = [0.0_f32; 4];
    for (channel, value) in sample.iter_mut().enumerate() {
        let top = top_left[channel] + (top_right[channel] - top_left[channel]) * frac_x;
        let bottom = bottom_left[channel] + (bottom_right[channel] - bottom_left[channel]) * frac_x;
        *value = top + (bottom - top) * frac_y;
    }
    sample
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grade(radius: f32, centre: [f32; 2]) -> SpherizeGrade {
        SpherizeGrade::new(radius, centre).expect("valid spherize grade")
    }

    fn frame(width: usize, height: usize) -> Vec<[f32; 4]> {
        let mut pixels = vec![[0.0_f32; 4]; width * height];
        for y in 0..height {
            for x in 0..width {
                let value = 0.04 * (x + 2 * y) as f32;
                pixels[y * width + x] = [value, 0.5 - value * 0.2, value * 1.4, 1.0];
            }
        }
        pixels
    }

    /// An independent implementation of the documented mapping.
    fn reference_sample(
        source: &[[f32; 4]],
        width: usize,
        height: usize,
        column: usize,
        row: usize,
        grade: SpherizeGrade,
    ) -> [f32; 4] {
        let centre_x = f64::from(grade.centre()[0]);
        let centre_y = f64::from(grade.centre()[1]);
        let radius = f64::from(grade.radius_pixels());
        let delta_x = column as f64 - centre_x;
        let delta_y = row as f64 - centre_y;
        let distance = delta_x.hypot(delta_y);
        if radius <= 0.0 || distance >= radius || distance <= f64::EPSILON {
            return source[row * width + column];
        }
        let sample_distance = radius * (distance / radius).asin() / std::f64::consts::FRAC_PI_2;
        let scale = sample_distance / distance;
        let x = (centre_x + delta_x * scale).clamp(0.0, (width - 1) as f64);
        let y = (centre_y + delta_y * scale).clamp(0.0, (height - 1) as f64);
        let x0 = x.floor();
        let y0 = y.floor();
        let x1 = (x0 as usize + 1).min(width - 1);
        let y1 = (y0 as usize + 1).min(height - 1);
        let fx = (x - x0) as f32;
        let fy = (y - y0) as f32;
        let mut sampled = [0.0_f32; 4];
        for channel in 0..4 {
            let a = source[y0 as usize * width + x0 as usize][channel];
            let b = source[y0 as usize * width + x1][channel];
            let c = source[y1 * width + x0 as usize][channel];
            let d = source[y1 * width + x1][channel];
            let top = a + (b - a) * fx;
            let bottom = c + (d - c) * fx;
            sampled[channel] = top + (bottom - top) * fy;
        }
        sampled
    }

    #[test]
    fn rejects_author_state_outside_the_declared_ranges() {
        assert_eq!(
            SpherizeGrade::new(f32::NAN, [1.0, 1.0]),
            Err(SpherizeError::Radius)
        );
        assert_eq!(
            SpherizeGrade::new(2_501.0, [1.0, 1.0]),
            Err(SpherizeError::Radius)
        );
        assert_eq!(
            SpherizeGrade::new(-1.0, [1.0, 1.0]),
            Err(SpherizeError::Radius)
        );
        assert_eq!(
            SpherizeGrade::new(10.0, [f32::INFINITY, 1.0]),
            Err(SpherizeError::Centre)
        );
    }

    #[test]
    fn zero_radius_is_identity() {
        assert!(grade(0.0, [4.0, 4.0]).is_identity());
        let mut pixels = frame(5, 5);
        let original = pixels.clone();
        assert!(spherize_frame_f32(
            &mut pixels,
            5,
            5,
            grade(0.0, [2.0, 2.0])
        ));
        assert_eq!(pixels, original);
    }

    #[test]
    fn the_centre_pixel_is_untouched() {
        let mut pixels = frame(9, 9);
        let original = pixels.clone();
        assert!(spherize_frame_f32(
            &mut pixels,
            9,
            9,
            grade(4.0, [4.0, 4.0])
        ));
        assert_eq!(pixels[4 * 9 + 4], original[4 * 9 + 4]);
    }

    #[test]
    fn pixels_at_and_beyond_the_rim_are_untouched() {
        let mut pixels = frame(11, 11);
        let original = pixels.clone();
        assert!(spherize_frame_f32(
            &mut pixels,
            11,
            11,
            grade(4.0, [5.0, 5.0])
        ));
        for y in 0..11 {
            for x in 0..11 {
                let distance = ((x as f64 - 5.0).powi(2) + (y as f64 - 5.0).powi(2)).sqrt();
                if distance >= 4.0 {
                    assert_eq!(pixels[y * 11 + x], original[y * 11 + x]);
                }
            }
        }
    }

    #[test]
    fn the_mapping_matches_an_independent_evaluation() {
        let mut pixels = frame(13, 11);
        let original = pixels.clone();
        let applied = grade(4.5, [5.5, 6.25]);
        assert!(spherize_frame_f32(&mut pixels, 13, 11, applied));
        for row in 0..11 {
            for column in 0..13 {
                let expected = reference_sample(&original, 13, 11, column, row, applied);
                let actual = pixels[row * 13 + column];
                for channel in 0..4 {
                    let difference = (expected[channel] - actual[channel]).abs();
                    assert!(
                        difference <= 2.0e-6,
                        "pixel ({column},{row}) channel {channel}: expected {expected:?} actual {actual:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_bulge_magnifies_the_centre() {
        // A single bright pixel one texel right of the centre must spread into
        // a wider footprint after the bulge magnifies that area.
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; 11 * 11];
        pixels[5 * 11 + 6] = [1.0, 1.0, 1.0, 1.0];
        assert!(spherize_frame_f32(
            &mut pixels,
            11,
            11,
            grade(4.0, [5.0, 5.0])
        ));
        let lit = pixels.iter().filter(|pixel| pixel[0] > 0.01 && pixel[3] > 0.0).count();
        assert!(lit > 1, "the bulge must spread the impulse, lit {lit}");
    }

    #[test]
    fn transparent_neighbours_do_not_stain_visible_pixels() {
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 0.0]; 9 * 9];
        for y in 3..6 {
            for x in 3..6 {
                pixels[y * 9 + x] = [0.4, 0.4, 0.4, 1.0];
            }
        }
        assert!(spherize_frame_f32(
            &mut pixels,
            9,
            9,
            grade(3.5, [4.0, 4.0])
        ));
        let centre = pixels[4 * 9 + 4];
        assert!((centre[0] - 0.4).abs() < 1.0e-5, "centre {centre:?}");
        assert!((centre[3] - 1.0).abs() < 1.0e-5, "centre {centre:?}");
    }

    #[test]
    fn extended_range_colour_is_not_clamped() {
        let mut pixels = vec![[0.0_f32; 4]; 9 * 9];
        for pixel in pixels.iter_mut() {
            *pixel = [4.0, -0.5, 2.5, 1.0];
        }
        assert!(spherize_frame_f32(
            &mut pixels,
            9,
            9,
            grade(3.0, [4.0, 4.0])
        ));
        for pixel in &pixels {
            assert!((pixel[0] - 4.0).abs() < 1.0e-5, "pixel {pixel:?}");
            assert!((pixel[1] + 0.5).abs() < 1.0e-5, "pixel {pixel:?}");
        }
    }

    #[test]
    fn the_effect_is_deterministic() {
        let applied = grade(3.0, [4.5, 4.0]);
        let mut first = frame(10, 10);
        let mut second = first.clone();
        assert!(spherize_frame_f32(&mut first, 10, 10, applied));
        assert!(spherize_frame_f32(&mut second, 10, 10, applied));
        assert_eq!(first, second);
    }

    #[test]
    fn frames_that_do_not_match_the_declared_size_fail() {
        let mut pixels = frame(4, 4);
        assert!(!spherize_frame_f32(
            &mut pixels,
            5,
            4,
            grade(2.0, [2.0, 2.0])
        ));
        assert!(!spherize_frame_f32(
            &mut pixels[..15],
            4,
            4,
            grade(2.0, [2.0, 2.0])
        ));
    }

    #[test]
    fn gpu_controls_carry_the_centre_and_the_radius() {
        let controls = grade(7.5, [12.0, -3.0]).gpu_controls();
        assert_eq!(controls, [12.0, -3.0, 7.5, 0.0]);
    }
}
