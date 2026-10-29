//! Unsharp mask (钝化蒙版) kernel.
//!
//! The effect compares the source frame against a Gaussian-blurred copy of it
//! and amplifies the difference. Author-facing values follow the effect list:
//! an amount in percent (`0`–`500`), a Gaussian radius in pixels (`0.1`–`500`)
//! and a difference threshold (`0`–`1`).
//!
//! Everything here operates on straight-alpha scene-linear values. The blur
//! that feeds the comparison is produced by the shared Gaussian kernel, which
//! premultiplies internally and returns straight alpha, so fully transparent
//! neighbours never bleed color into the differences of visible pixels.

use crate::coverage::has_positive_coverage;

/// Smallest authorable blur radius, in pixels.
pub const UNSHARP_MASK_AUTHOR_MIN_RADIUS_PIXELS: f32 = 0.1;
/// Largest authorable blur radius, in pixels.
pub const UNSHARP_MASK_AUTHOR_MAX_RADIUS_PIXELS: f32 = 500.0;
/// Largest authorable amount, in percent.
pub const UNSHARP_MASK_AUTHOR_MAX_AMOUNT_PERCENT: f32 = 500.0;

/// Validates an author-facing blur radius.
pub(crate) fn valid_unsharp_mask_radius(radius: f32) -> Option<f32> {
    (radius.is_finite()
        && (UNSHARP_MASK_AUTHOR_MIN_RADIUS_PIXELS..=UNSHARP_MASK_AUTHOR_MAX_RADIUS_PIXELS)
            .contains(&radius))
    .then_some(radius)
}

/// Converts an author-facing amount in percent into a dimensionless gain.
///
/// The declared parameter range keeps authors inside `0`–`500`; the clamp here
/// only makes a persisted out-of-range value harmless instead of inverting the
/// sign of the difference.
pub fn unsharp_mask_gain(amount_percent: f32) -> f32 {
    if !amount_percent.is_finite() {
        return 0.0;
    }
    (amount_percent / 100.0).clamp(0.0, UNSHARP_MASK_AUTHOR_MAX_AMOUNT_PERCENT / 100.0)
}

/// Sharpens one channel value by a saturated difference.
///
/// Differences whose magnitude is at or below the threshold are ignored, which
/// is the dead-zone the list describes: flat areas keep their value and only
/// edges above the threshold are amplified. Values are not clamped, so HDR
/// highlights and negative scene-linear values keep their sign and magnitude.
pub fn sharpened_channel(base: f32, blurred: f32, gain: f32, threshold: f32) -> f32 {
    let threshold = threshold.max(0.0);
    let difference = base - blurred;
    if difference.abs() <= threshold {
        return base;
    }
    base + difference * gain
}

/// Applies the unsharp mask to straight-alpha f32 pixels in place.
///
/// `blurred` must be the Gaussian-blurred copy of `pixels` with the same
/// length. Pixels without positive coverage keep their value, mirroring the
/// other point operations.
pub fn apply_unsharp_mask_f32(
    pixels: &mut [[f32; 4]],
    blurred: &[[f32; 4]],
    gain: f32,
    threshold: f32,
) {
    let gain = gain.max(0.0);
    for (pixel, softened) in pixels.iter_mut().zip(blurred) {
        if !has_positive_coverage(pixel[3].clamp(0.0, 1.0)) {
            continue;
        }
        for channel in 0..3 {
            pixel[channel] = sharpened_channel(pixel[channel], softened[channel], gain, threshold);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(pixels: &mut [[f32; 4]], blurred: &[[f32; 4]], gain: f32, threshold: f32) {
        apply_unsharp_mask_f32(pixels, blurred, gain, threshold);
    }

    #[test]
    fn author_radius_range_is_enforced() {
        assert_eq!(valid_unsharp_mask_radius(1.0), Some(1.0));
        assert_eq!(
            valid_unsharp_mask_radius(UNSHARP_MASK_AUTHOR_MAX_RADIUS_PIXELS),
            Some(UNSHARP_MASK_AUTHOR_MAX_RADIUS_PIXELS)
        );
        assert_eq!(valid_unsharp_mask_radius(0.0), None);
        assert_eq!(valid_unsharp_mask_radius(0.05), None);
        assert_eq!(
            valid_unsharp_mask_radius(UNSHARP_MASK_AUTHOR_MAX_RADIUS_PIXELS + 0.1),
            None
        );
        assert_eq!(valid_unsharp_mask_radius(f32::NAN), None);
        assert_eq!(valid_unsharp_mask_radius(f32::INFINITY), None);
    }

    #[test]
    fn amount_percent_becomes_a_gain() {
        assert_eq!(unsharp_mask_gain(0.0), 0.0);
        assert_eq!(unsharp_mask_gain(50.0), 0.5);
        assert_eq!(unsharp_mask_gain(100.0), 1.0);
        assert_eq!(unsharp_mask_gain(500.0), 5.0);
        assert_eq!(unsharp_mask_gain(900.0), 5.0);
        assert_eq!(unsharp_mask_gain(-10.0), 0.0);
        assert_eq!(unsharp_mask_gain(f32::NAN), 0.0);
    }

    #[test]
    fn zero_gain_is_identity() {
        let mut pixels = [[0.2, 0.4, 0.6, 1.0]];
        let blurred = [[0.0, 0.0, 0.0, 1.0]];
        apply(&mut pixels, &blurred, 0.0, 0.0);
        assert_eq!(pixels, [[0.2, 0.4, 0.6, 1.0]]);
    }

    #[test]
    fn difference_is_amplified_without_clamping() {
        // 0.5 + (0.5 - 0.0) = 1.0, 0.25 + (0.25 - 0.75) = -0.25, 2.0 + 0.5 = 2.5
        let mut pixels = [[0.5, 0.25, 2.0, 1.0]];
        let blurred = [[0.0, 0.75, 1.5, 1.0]];
        apply(&mut pixels, &blurred, 1.0, 0.0);
        assert_eq!(pixels, [[1.0, -0.25, 2.5, 1.0]]);
    }

    #[test]
    fn negative_differences_are_sharpened_symmetrically() {
        let mut pixels = [[0.25, 0.25, 0.25, 1.0]];
        let blurred = [[0.75, 0.0, 0.25, 1.0]];
        apply(&mut pixels, &blurred, 2.0, 0.0);
        // 0.25 + (0.25 - 0.75) * 2 = -0.75, 0.25 + (0.25 - 0.0) * 2 = 0.75,
        // the last channel has no difference and keeps its value.
        assert_eq!(pixels, [[-0.75, 0.75, 0.25, 1.0]]);
    }

    #[test]
    fn threshold_suppresses_flat_differences() {
        let mut pixels = [[0.5, 0.5, 0.5, 1.0]];
        let blurred = [[0.375, 0.625, 0.5, 1.0]];
        apply(&mut pixels, &blurred, 1.0, 0.25);
        assert_eq!(pixels, [[0.5, 0.5, 0.5, 1.0]]);
    }

    #[test]
    fn differences_at_the_threshold_are_ignored() {
        let mut pixels = [[0.5, 0.5, 0.5, 1.0]];
        let blurred = [[0.25, 0.75, 0.5, 1.0]];
        apply(&mut pixels, &blurred, 1.0, 0.25);
        assert_eq!(pixels, [[0.5, 0.5, 0.5, 1.0]]);
    }

    #[test]
    fn differences_above_the_threshold_keep_their_full_difference() {
        let mut pixels = [[0.5, 0.5, 0.5, 1.0]];
        let blurred = [[0.125, 0.5, 0.5, 1.0]];
        apply(&mut pixels, &blurred, 1.0, 0.25);
        assert_eq!(pixels, [[0.875, 0.5, 0.5, 1.0]]);
    }

    #[test]
    fn alpha_is_left_untouched() {
        let mut pixels = [[0.25, 0.25, 0.25, 0.25]];
        let blurred = [[0.0, 0.0, 0.0, 0.9]];
        apply(&mut pixels, &blurred, 1.0, 0.0);
        assert_eq!(pixels, [[0.5, 0.5, 0.5, 0.25]]);
    }

    #[test]
    fn transparent_pixels_are_left_untouched() {
        let mut pixels = [[0.0, 0.0, 0.0, 0.0], [0.25, 0.25, 0.25, 1.0]];
        let blurred = [[0.5, 0.5, 0.5, 1.0], [0.0, 0.0, 0.0, 1.0]];
        apply(&mut pixels, &blurred, 1.0, 0.0);
        assert_eq!(pixels[0], [0.0, 0.0, 0.0, 0.0]);
        assert_eq!(pixels[1], [0.5, 0.5, 0.5, 1.0]);
    }

    #[test]
    fn hdr_values_stay_above_one() {
        let mut pixels = [[4.0, 4.0, 4.0, 1.0]];
        let blurred = [[2.0, 2.0, 2.0, 1.0]];
        apply(&mut pixels, &blurred, 0.5, 0.0);
        assert_eq!(pixels, [[5.0, 5.0, 5.0, 1.0]]);
    }
}
