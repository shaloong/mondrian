//! Bulge: magnifies an elliptical region of the frame.
//!
//! The effect list names the horizontal and vertical radius, the centre, a
//! height between -4 and 4, a taper radius, an antialias switch and a pin
//! switch. It does not define how those numbers map to pixels, so the mapping
//! is defined here and nowhere else:
//!
//! * the height is a power of two magnification, so `height = 0` is the exact
//!   identity and `height = 1` doubles the middle of the ellipse,
//! * the magnification falls off as `(1 - d^2)^2` inside the ellipse, where
//!   `d` is the ellipse-normalised distance, which keeps the rim continuous
//!   and its slope zero,
//! * beyond the rim the taper radius fades the remaining magnification out
//!   over that many pixels,
//! * pinning pulls samples back onto the rim, so the ellipse never reaches
//!   content outside its own footprint, and
//! * the antialias switch averages four sub-pixel samples instead of one.

use crate::resample::sample_bilinear_clamped;

/// Smallest accepted ellipse radius, in pixels.
pub const BULGE_MIN_RADIUS_PIXELS: f32 = 0.0;
/// Largest accepted ellipse radius, in pixels.
pub const BULGE_MAX_RADIUS_PIXELS: f32 = 8_000.0;
/// Smallest accepted taper radius, in pixels.
pub const BULGE_MIN_TAPER_PIXELS: f32 = 0.0;
/// Largest accepted taper radius, in pixels.
pub const BULGE_MAX_TAPER_PIXELS: f32 = 8_000.0;
/// Smallest accepted height.
pub const BULGE_MIN_HEIGHT: f32 = -4.0;
/// Largest accepted height.
pub const BULGE_MAX_HEIGHT: f32 = 4.0;

const EPSILON: f32 = 1.0e-6;
const CHECKPOINT_PIXELS: usize = 4_096;

/// Reason a [`BulgeGrade`] could not be built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BulgeError {
    /// A radius, the taper radius or the height is outside its declared range.
    Range,
    /// A radius, the centre or the taper radius is not finite.
    NonFinite,
}

impl std::fmt::Display for BulgeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Range => formatter.write_str("bulge parameter outside its declared range"),
            Self::NonFinite => formatter.write_str("bulge parameter is not finite"),
        }
    }
}

impl std::error::Error for BulgeError {}

/// Authored bulge parameters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BulgeGrade {
    radius: [f32; 2],
    centre: [f32; 2],
    height: f32,
    taper_radius: f32,
    antialias: bool,
    pinned: bool,
}

impl BulgeGrade {
    /// Builds a grade, rejecting values outside the declared ranges.
    pub fn new(
        radius: [f32; 2],
        centre: [f32; 2],
        height: f32,
        taper_radius: f32,
        antialias: bool,
        pinned: bool,
    ) -> Result<Self, BulgeError> {
        for value in [
            radius[0],
            radius[1],
            centre[0],
            centre[1],
            height,
            taper_radius,
        ] {
            if !value.is_finite() {
                return Err(BulgeError::NonFinite);
            }
        }
        for value in radius {
            if !(BULGE_MIN_RADIUS_PIXELS..=BULGE_MAX_RADIUS_PIXELS).contains(&value) {
                return Err(BulgeError::Range);
            }
        }
        if !(BULGE_MIN_TAPER_PIXELS..=BULGE_MAX_TAPER_PIXELS).contains(&taper_radius) {
            return Err(BulgeError::Range);
        }
        if !(BULGE_MIN_HEIGHT..=BULGE_MAX_HEIGHT).contains(&height) {
            return Err(BulgeError::Range);
        }
        Ok(Self {
            radius,
            centre,
            height,
            taper_radius,
            antialias,
            pinned,
        })
    }

    /// Ellipse semi-axes, in pixels.
    pub fn radius(&self) -> [f32; 2] {
        self.radius
    }

    /// Ellipse centre, in pixels.
    pub fn centre(&self) -> [f32; 2] {
        self.centre
    }

    /// Authored height, in powers of two.
    pub fn height(&self) -> f32 {
        self.height
    }

    /// Fade-out distance beyond the rim, in pixels.
    pub fn taper_radius(&self) -> f32 {
        self.taper_radius
    }

    /// Whether sub-pixel averaging is enabled.
    pub fn antialias(&self) -> bool {
        self.antialias
    }

    /// Whether samples are pinned to the rim.
    pub fn pinned(&self) -> bool {
        self.pinned
    }

    /// Whether the grade leaves the frame untouched.
    pub fn is_identity(&self) -> bool {
        self.height.abs() <= EPSILON
            || self.radius[0].abs() <= EPSILON
            || self.radius[1].abs() <= EPSILON
    }

    /// Magnification applied at one ellipse-normalised distance.
    fn magnification(&self, distance: f64, rim_pixels: f64) -> f64 {
        let factor = f64::from(self.height).exp2();
        if distance <= 1.0 {
            let falloff = 1.0 - distance * distance;
            return 1.0 + (factor - 1.0) * falloff * falloff;
        }
        if self.taper_radius <= EPSILON || rim_pixels <= f64::EPSILON {
            return 1.0;
        }
        let beyond = (distance - 1.0) * rim_pixels;
        if beyond >= f64::from(self.taper_radius) {
            return 1.0;
        }
        let falloff = 1.0 - beyond / f64::from(self.taper_radius);
        1.0 + (factor - 1.0) * falloff * falloff
    }

    /// Maps one destination coordinate back into the source frame.
    fn source_position(&self, column: f64, row: f64) -> (f64, f64) {
        let centre_x = f64::from(self.centre[0]);
        let centre_y = f64::from(self.centre[1]);
        let delta_x = column - centre_x;
        let delta_y = row - centre_y;
        let radius_x = f64::from(self.radius[0].max(EPSILON));
        let radius_y = f64::from(self.radius[1].max(EPSILON));
        let normalized_x = delta_x / radius_x;
        let normalized_y = delta_y / radius_y;
        let distance = normalized_x.hypot(normalized_y);
        if distance <= f64::EPSILON {
            return (column, row);
        }
        if self.pinned && distance > 1.0 {
            // Pull the sample back onto the rim so the ellipse never reaches
            // content outside its own footprint.
            return (
                centre_x + normalized_x / distance * radius_x,
                centre_y + normalized_y / distance * radius_y,
            );
        }
        let magnification = self.magnification(distance, radius_x.min(radius_y)).max(1.0e-6);
        (
            centre_x + delta_x / magnification,
            centre_y + delta_y / magnification,
        )
    }

    /// Applies the bulge to one straight-alpha pixel of a frame snapshot.
    pub fn apply(
        &self,
        frame: &[[f32; 4]],
        width: usize,
        height: usize,
        column: usize,
        row: usize,
    ) -> [f32; 4] {
        let offsets: &[[f64; 2]] = if self.antialias {
            &[[-0.25, -0.25], [0.25, -0.25], [-0.25, 0.25], [0.25, 0.25]]
        } else {
            &[[0.0, 0.0]]
        };
        let mut accumulated = [0.0_f64; 4];
        for offset in offsets {
            let (sample_x, sample_y) =
                self.source_position(column as f64 + offset[0], row as f64 + offset[1]);
            let sample = sample_bilinear_clamped(frame, width, height, sample_x, sample_y);
            for channel in 0..4 {
                accumulated[channel] += f64::from(sample[channel]);
            }
        }
        let divisor = offsets.len() as f64;
        [
            (accumulated[0] / divisor) as f32,
            (accumulated[1] / divisor) as f32,
            (accumulated[2] / divisor) as f32,
            (accumulated[3] / divisor) as f32,
        ]
    }

    /// Packs the grade for the GPU spatial uniforms.
    pub fn gpu_controls(&self) -> ([f32; 4], [f32; 4]) {
        (
            [
                self.centre[0],
                self.centre[1],
                self.radius[0],
                self.radius[1],
            ],
            [
                self.height,
                self.taper_radius,
                f32::from(u8::from(self.antialias)),
                f32::from(u8::from(self.pinned)),
            ],
        )
    }
}

/// Applies the bulge to a whole frame, returning whether anything changed.
pub fn bulge_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: BulgeGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_bulge_f32_controlled(pixels, width, height, grade, &mut checkpoint).unwrap_or(false)
}

pub(crate) fn apply_bulge_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: BulgeGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let expected = width.checked_mul(height).unwrap_or(0);
    if width == 0 || height == 0 || pixels.len() != expected {
        return Ok(false);
    }
    if grade.is_identity() {
        return Ok(false);
    }
    crate::straight_alpha::premultiply_rgba_controlled(pixels, checkpoint)?;
    let source = pixels.to_vec();
    let written = std::sync::atomic::AtomicBool::new(false);
    crate::parallel::for_each_row_band(pixels, width, 0..height, |first_row, band| {
        for (offset, pixel) in band.chunks_mut(width).enumerate() {
            let row = first_row + offset;
            for (column, destination) in pixel.iter_mut().enumerate() {
                *destination = grade.apply(&source, width, height, column, row);
            }
        }
        written.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = CHECKPOINT_PIXELS;
    });
    checkpoint()?;
    crate::straight_alpha::unpremultiply_rgba_controlled(pixels, checkpoint)?;
    Ok(written.load(std::sync::atomic::Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKING: [f32; 3] = [0.2126, 0.7152, 0.0722];

    fn grade(radius: [f32; 2], height: f32) -> BulgeGrade {
        BulgeGrade::new(radius, [4.0, 4.0], height, 0.0, false, false).expect("grade")
    }

    fn frame(width: usize, height: usize) -> Vec<[f32; 4]> {
        let mut pixels = vec![[0.0_f32; 4]; width * height];
        for row in 0..height {
            for column in 0..width {
                pixels[row * width + column] = [column as f32, row as f32, 1.0, 1.0];
            }
        }
        pixels
    }

    #[test]
    fn parameters_outside_the_declared_ranges_are_rejected() {
        assert_eq!(
            BulgeGrade::new([8_001.0, 10.0], [0.0, 0.0], 0.0, 0.0, false, false),
            Err(BulgeError::Range)
        );
        assert_eq!(
            BulgeGrade::new([10.0, 10.0], [0.0, 0.0], 4.5, 0.0, false, false),
            Err(BulgeError::Range)
        );
        assert_eq!(
            BulgeGrade::new([10.0, 10.0], [0.0, 0.0], 0.0, -1.0, false, false),
            Err(BulgeError::Range)
        );
        assert_eq!(
            BulgeGrade::new([f32::NAN, 10.0], [0.0, 0.0], 0.0, 0.0, false, false),
            Err(BulgeError::NonFinite)
        );
    }

    #[test]
    fn zero_height_is_the_identity() {
        assert!(grade([4.0, 4.0], 0.0).is_identity());
        let mut pixels = frame(9, 9);
        let original = pixels.clone();
        assert!(!bulge_frame_f32(&mut pixels, 9, 9, grade([4.0, 4.0], 0.0)));
        assert_eq!(pixels, original);
    }

    #[test]
    fn the_centre_pixel_keeps_its_value() {
        let pixels = frame(9, 9);
        let sampled = grade([3.0, 3.0], 2.0).apply(&pixels, 9, 9, 4, 4);
        assert_eq!(sampled, pixels[4 * 9 + 4]);
    }

    #[test]
    fn the_rim_and_beyond_are_untouched_without_a_taper() {
        let pixels = frame(11, 11);
        let bulge = grade([3.0, 3.0], 2.0);
        assert_eq!(bulge.apply(&pixels, 11, 11, 8, 5), pixels[5 * 11 + 8]);
        assert_eq!(bulge.apply(&pixels, 11, 11, 0, 0), pixels[0]);
    }

    #[test]
    fn magnifying_pulls_the_sample_towards_the_centre() {
        let pixels = frame(9, 9);
        let sampled = grade([4.0, 4.0], 1.0).apply(&pixels, 9, 9, 6, 4);
        // Positive height magnifies, so the sample sits closer to the centre
        // than the destination column does.
        assert!(sampled[0] > 4.0 && sampled[0] < 6.0, "sampled {sampled:?}");
    }

    #[test]
    fn minifying_pushes_the_sample_away_from_the_centre() {
        let pixels = frame(9, 9);
        let sampled = grade([4.0, 4.0], -1.0).apply(&pixels, 9, 9, 6, 4);
        assert!(sampled[0] > 6.0, "sampled {sampled:?}");
    }

    #[test]
    fn the_taper_extends_the_reach_beyond_the_rim() {
        let pixels = frame(11, 11);
        let plain = grade([3.0, 3.0], 2.0);
        let tapered =
            BulgeGrade::new([3.0, 3.0], [5.0, 5.0], 2.0, 3.0, false, false).expect("grade");
        assert_eq!(tapered.centre(), [5.0, 5.0]);
        // Two pixels outside the rim the taper still moves the sample.
        let plain_sample = plain.apply(&pixels, 11, 11, 10, 5);
        let tapered_sample = tapered.apply(&pixels, 11, 11, 10, 5);
        let untouched = pixels[5 * 11 + 10];
        assert_eq!(plain_sample, untouched);
        assert_ne!(tapered_sample, untouched);
    }

    #[test]
    fn pinning_keeps_samples_inside_the_ellipse() {
        let pixels = frame(13, 13);
        let unpinned =
            BulgeGrade::new([3.0, 3.0], [6.5, 6.5], -2.0, 0.0, false, false).expect("grade");
        let pinned =
            BulgeGrade::new([3.0, 3.0], [6.5, 6.5], -2.0, 0.0, false, true).expect("grade");
        // Just outside the rim in a diagonal direction, where an unpinned
        // minification samples the surrounding content.
        let free = unpinned.apply(&pixels, 13, 13, 9, 9);
        let held = pinned.apply(&pixels, 13, 13, 9, 9);
        assert_ne!(free, held, "free {free:?} held {held:?}");
        assert!(held[0] > 6.5 && held[0] <= 9.0, "held {held:?}");
    }

    #[test]
    fn antialias_averages_four_samples() {
        let pixels = frame(9, 9);
        let single =
            BulgeGrade::new([4.0, 4.0], [4.0, 4.0], 2.0, 0.0, false, false).expect("grade");
        let averaged =
            BulgeGrade::new([4.0, 4.0], [4.0, 4.0], 2.0, 0.0, true, false).expect("grade");
        let one = single.apply(&pixels, 9, 9, 5, 5);
        let four = averaged.apply(&pixels, 9, 9, 5, 5);
        assert_ne!(one, four);
        assert!(four[0] > 4.0 && four[0] < 5.0, "averaged {four:?}");
    }

    #[test]
    fn transparent_neighbours_do_not_stain_visible_pixels() {
        let mut pixels = frame(9, 9);
        for pixel in &mut pixels {
            pixel[3] = 0.0;
        }
        pixels[4 * 9 + 4] = [1.0, 0.5, 0.25, 1.0];
        let mut processed = pixels.clone();
        assert!(bulge_frame_f32(
            &mut processed,
            9,
            9,
            grade([4.0, 4.0], 1.0)
        ));
        let sampled = processed[4 * 9 + 5];
        // Coverage is partial, but the premultiplied sample keeps the visible
        // colour instead of darkening it towards the transparent neighbours.
        assert!(sampled[3] > 0.0 && sampled[3] < 1.0, "sampled {sampled:?}");
        assert!((sampled[0] - 1.0).abs() <= 1.0e-5, "sampled {sampled:?}");
        assert!((sampled[1] - 0.5).abs() <= 1.0e-5, "sampled {sampled:?}");
    }

    #[test]
    fn extended_range_values_are_not_clamped() {
        let mut pixels = frame(9, 9);
        pixels[4 * 9 + 4] = [2.5, -0.75, 1.25, 1.0];
        let sampled = grade([4.0, 4.0], 1.0).apply(&pixels, 9, 9, 4, 4);
        assert_eq!(sampled, pixels[4 * 9 + 4]);
        assert!(sampled[1] < 0.0);
    }

    #[test]
    fn frames_with_the_wrong_length_are_rejected() {
        let mut pixels = vec![[0.0_f32; 4]; 5];
        assert!(!bulge_frame_f32(&mut pixels, 4, 4, grade([2.0, 2.0], 1.0)));
    }

    #[test]
    fn the_frame_entry_point_is_deterministic() {
        let mut first = frame(9, 9);
        let mut second = frame(9, 9);
        assert!(bulge_frame_f32(&mut first, 9, 9, grade([4.0, 4.0], 1.5)));
        assert!(bulge_frame_f32(&mut second, 9, 9, grade([4.0, 4.0], 1.5)));
        assert_eq!(first, second);
    }

    #[test]
    fn gpu_controls_carry_the_authored_parameters() {
        let bulge = BulgeGrade::new([3.0, 2.0], [1.5, 2.5], 1.25, 4.0, true, true).expect("grade");
        let (params, extra) = bulge.gpu_controls();
        assert_eq!(params, [1.5, 2.5, 3.0, 2.0]);
        assert_eq!(extra, [1.25, 4.0, 1.0, 1.0]);
        let _ = WORKING;
    }
}
