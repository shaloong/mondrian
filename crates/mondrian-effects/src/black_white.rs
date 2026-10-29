//! Hue-keyed black and white conversion.
//!
//! The effect list models black and white as six per-hue weights (red, yellow,
//! green, cyan, blue, magenta), a tint amount, and a tint color. The weights are
//! percentages of how bright a hue renders in the gray image: `100` keeps the
//! pixel's working-space lightness, `0` renders that hue black, negative weights
//! invert it, and values above `100` brighten it. The weighting is linear in the
//! pixel's HSL lightness, so it needs no display encoding and stays exact for
//! extended-range and negative channels.

use crate::hsl_space::{extremes, hue_turns, NEUTRAL_CHROMA};

/// Lowest authored weight for one hue stop.
pub const BLACK_WHITE_WEIGHT_MIN: f32 = -200.0;
/// Highest authored weight for one hue stop.
pub const BLACK_WHITE_WEIGHT_MAX: f32 = 300.0;
/// Number of hue stops around the wheel.
pub const BLACK_WHITE_HUE_STOPS: usize = 6;
/// Weight that leaves a hue's lightness unchanged.
pub const BLACK_WHITE_NEUTRAL_WEIGHT: f32 = 100.0;

/// Ordered hue stop names, matching the effect list menu.
pub const BLACK_WHITE_STOP_KEYS: [&str; BLACK_WHITE_HUE_STOPS] =
    ["red", "yellow", "green", "cyan", "blue", "magenta"];

/// Prepared black and white conversion.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlackAndWhiteGrade {
    weights: [f32; BLACK_WHITE_HUE_STOPS],
    tint_amount: f32,
    tint_color: [f32; 3],
}

impl BlackAndWhiteGrade {
    /// Validate authored controls.
    pub fn new(
        weights: [f32; BLACK_WHITE_HUE_STOPS],
        tint_amount: f32,
        tint_color: [f32; 3],
    ) -> Result<Self, BlackAndWhiteError> {
        for (index, weight) in weights.iter().enumerate() {
            if !weight.is_finite()
                || *weight < BLACK_WHITE_WEIGHT_MIN
                || *weight > BLACK_WHITE_WEIGHT_MAX
            {
                return Err(BlackAndWhiteError::Weight(index));
            }
        }
        if !tint_amount.is_finite() || !(0.0..=1.0).contains(&tint_amount) {
            return Err(BlackAndWhiteError::TintAmount);
        }
        if tint_color.iter().any(|channel| !channel.is_finite()) {
            return Err(BlackAndWhiteError::TintColor);
        }
        Ok(Self { weights, tint_amount, tint_color })
    }

    /// Authored per-hue weights in list order.
    pub const fn weights(self) -> [f32; BLACK_WHITE_HUE_STOPS] {
        self.weights
    }

    /// Authored tint amount.
    pub const fn tint_amount(self) -> f32 {
        self.tint_amount
    }

    /// Authored tint color.
    pub const fn tint_color(self) -> [f32; 3] {
        self.tint_color
    }

    /// Weight for a hue in turns, linearly interpolated between adjacent stops.
    pub fn weight_for_hue(self, hue: f32) -> f32 {
        let scaled = hue.rem_euclid(1.0) * BLACK_WHITE_HUE_STOPS as f32;
        let base = scaled.floor();
        let index = (base as usize) % BLACK_WHITE_HUE_STOPS;
        let next = (index + 1) % BLACK_WHITE_HUE_STOPS;
        let t = scaled - base;
        let from = self.weights[index];
        let to = self.weights[next];
        from + (to - from) * t
    }

    /// Convert one working-space pixel to gray.
    pub fn apply(self, rgb: [f32; 3]) -> [f32; 3] {
        let (high, low) = extremes(rgb);
        let chroma = high - low;
        let lightness = (high + low) * 0.5;
        // A pixel without a hue has nothing to key on, so it keeps its
        // lightness instead of collapsing to the wheel's zero-degree stop.
        let gain = if chroma.is_finite() && chroma > NEUTRAL_CHROMA {
            self.weight_for_hue(hue_turns(rgb, high, chroma)) / 100.0
        } else {
            1.0
        };
        let gray = lightness * gain;
        if self.tint_amount <= 0.0 {
            return [gray, gray, gray];
        }
        let tinted = [
            gray * self.tint_color[0],
            gray * self.tint_color[1],
            gray * self.tint_color[2],
        ];
        let amount = self.tint_amount;
        [
            gray + (tinted[0] - gray) * amount,
            gray + (tinted[1] - gray) * amount,
            gray + (tinted[2] - gray) * amount,
        ]
    }

    /// Exact controls for the fused GPU point path.
    ///
    /// Layout: six hue weights in list order, the tint amount, then the tint
    /// color. Sending the authored values verbatim keeps the two backends from
    /// re-deriving anything.
    pub fn gpu_controls(self) -> [f32; BLACK_WHITE_HUE_STOPS + 4] {
        [
            self.weights[0],
            self.weights[1],
            self.weights[2],
            self.weights[3],
            self.weights[4],
            self.weights[5],
            self.tint_amount,
            self.tint_color[0],
            self.tint_color[1],
            self.tint_color[2],
        ]
    }
}

/// Invalid authored black and white control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BlackAndWhiteError {
    /// One per-hue weight is non-finite or outside `[-200, 300]`.
    #[error("black and white hue weight {0} must be finite and within [-200, 300] percent")]
    Weight(usize),
    /// The tint amount is non-finite or outside `[0, 1]`.
    #[error("black and white tint amount must be finite and within [0, 1]")]
    TintAmount,
    /// One tint color channel is non-finite.
    #[error("black and white tint color must be finite")]
    TintColor,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grade(weights: [f32; 6], tint_amount: f32, tint: [f32; 3]) -> BlackAndWhiteGrade {
        BlackAndWhiteGrade::new(weights, tint_amount, tint).expect("valid grade")
    }

    fn gray_level(pixel: [f32; 3]) -> f32 {
        pixel[0]
    }

    #[test]
    fn hue_weight_interpolates_between_adjacent_stops() {
        let grade = grade([0.0, 100.0, 0.0, 0.0, 0.0, 0.0], 0.0, [1.0; 3]);
        assert_eq!(grade.weight_for_hue(0.0), 0.0);
        assert_eq!(grade.weight_for_hue(1.0 / 6.0), 100.0);
        assert_eq!(grade.weight_for_hue(1.0 / 12.0), 50.0);
        assert_eq!(grade.weight_for_hue(1.0 / 24.0), 25.0);
        // The wheel wraps, so a hue just below a full turn leans on the
        // magenta stop and then back toward red.
        assert_eq!(grade.weight_for_hue(1.0 - 1.0 / 12.0), 0.0);
    }

    #[test]
    fn neutral_pixels_keep_their_lightness() {
        let grade = grade([40.0, 60.0, 40.0, 60.0, 20.0, 80.0], 0.0, [1.0; 3]);
        assert_eq!(grade.apply([0.25, 0.25, 0.25]), [0.25, 0.25, 0.25]);
    }

    #[test]
    fn red_weight_scales_the_red_lightness() {
        let full = grade([100.0, 0.0, 0.0, 0.0, 0.0, 0.0], 0.0, [1.0; 3]);
        assert_eq!(full.apply([1.0, 0.0, 0.0]), [0.5, 0.5, 0.5]);
        let zero = grade([0.0; 6], 0.0, [1.0; 3]);
        assert_eq!(zero.apply([1.0, 0.0, 0.0]), [0.0, 0.0, 0.0]);
        let default_red = grade([40.0, 60.0, 40.0, 60.0, 20.0, 80.0], 0.0, [1.0; 3]);
        assert!((gray_level(default_red.apply([1.0, 0.0, 0.0])) - 0.2).abs() < 1.0e-6);
    }

    #[test]
    fn negative_weights_invert_the_hue_without_clamping() {
        let inverted = grade([-100.0, 0.0, 0.0, 0.0, 0.0, 0.0], 0.0, [1.0; 3]);
        assert_eq!(inverted.apply([1.0, 0.0, 0.0]), [-0.5, -0.5, -0.5]);
    }

    #[test]
    fn extended_range_lightness_is_preserved_exactly() {
        let full = grade([100.0, 0.0, 0.0, 0.0, 0.0, 0.0], 0.0, [1.0; 3]);
        // Lightness of (2.0, 0.5, 0.5) is 1.25, above the display range.
        assert_eq!(full.apply([2.0, 0.5, 0.5]), [1.25, 1.25, 1.25]);
    }

    #[test]
    fn orange_hue_uses_the_midpoint_between_red_and_yellow() {
        let grade = grade([0.0, 100.0, 0.0, 0.0, 0.0, 0.0], 0.0, [1.0; 3]);
        let orange = crate::hsl_space::from_hue_chroma_lightness(1.0 / 12.0, 1.0, 0.5);
        assert!((gray_level(grade.apply(orange)) - 0.25).abs() < 1.0e-6);
    }

    #[test]
    fn tint_ramps_between_gray_and_the_tint_color() {
        let plain = grade(
            [100.0, 100.0, 100.0, 100.0, 100.0, 100.0],
            0.0,
            [0.5, 0.25, 0.75],
        );
        assert_eq!(plain.apply([0.4, 0.4, 0.4]), [0.4, 0.4, 0.4]);

        let tinted = grade(
            [100.0, 100.0, 100.0, 100.0, 100.0, 100.0],
            0.5,
            [0.5, 0.25, 0.75],
        );
        let produced = tinted.apply([0.4, 0.4, 0.4]);
        assert!((produced[0] - 0.3).abs() < 1.0e-6);
        assert!((produced[1] - 0.25).abs() < 1.0e-6);
        assert!((produced[2] - 0.35).abs() < 1.0e-6);
    }

    #[test]
    fn gpu_controls_round_trip_the_grade_fields() {
        let authored = grade(
            [-10.0, 20.0, 30.0, 40.0, 250.0, 300.0],
            0.75,
            [0.9, 0.8, 0.7],
        );
        let controls = authored.gpu_controls();
        assert_eq!(controls[..6], authored.weights());
        assert_eq!(controls[6], 0.75);
        assert_eq!(controls[7..], authored.tint_color());
    }

    #[test]
    fn gpu_uniform_layout_reproduces_the_cpu_kernel() {
        // Mirrors the kind-15 WGSL kernel over the packed uniform layout:
        // params = weights[0..4], color = [weights[4], weights[5], tint_amount, 0],
        // extra0 = tint color. The test guards the packing layout against a
        // WGSL/kernel drift that a CPU-only test would not notice.
        fn weight_at(index: usize, head: [f32; 4], tail: [f32; 2]) -> f32 {
            match index {
                0 => head[0],
                1 => head[1],
                2 => head[2],
                3 => head[3],
                4 => tail[0],
                _ => tail[1],
            }
        }

        fn gpu_apply(
            rgb: [f32; 3],
            head: [f32; 4],
            tail: [f32; 2],
            tint_amount: f32,
            tint: [f32; 3],
        ) -> [f32; 3] {
            let (high, low) = crate::hsl_space::extremes(rgb);
            let chroma = high - low;
            let lightness = (high + low) * 0.5;
            let mut gain = 1.0;
            if chroma > crate::hsl_space::NEUTRAL_CHROMA {
                let hue = crate::hsl_space::hue_turns(rgb, high, chroma);
                let scaled = hue.rem_euclid(1.0) * 6.0;
                let base = scaled.floor();
                let index = (base as usize) % 6;
                let next = (index + 1) % 6;
                let amount = scaled - base;
                let low_weight = weight_at(index, head, tail);
                let high_weight = weight_at(next, head, tail);
                gain = (low_weight + (high_weight - low_weight) * amount) / 100.0;
            }
            let gray = lightness * gain;
            let mut tinted = [0.0; 3];
            for channel in 0..3 {
                tinted[channel] = gray + (gray * tint[channel] - gray) * tint_amount;
            }
            tinted
        }

        let grade =
            BlackAndWhiteGrade::new([40.0, 60.0, 40.0, 60.0, 20.0, 80.0], 0.35, [0.9, 0.4, 0.2])
                .expect("valid grade");
        let controls = grade.gpu_controls();
        let head = [controls[0], controls[1], controls[2], controls[3]];
        let tail = [controls[4], controls[5]];
        let tint_amount = controls[6];
        let tint = [controls[7], controls[8], controls[9]];

        for pixel in [
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 1.0],
            [0.5, 0.5, 0.5],
            [0.2, 0.6, 0.3],
            [2.0, 0.5, 0.5],
            [-0.25, 0.4, 0.1],
        ] {
            let cpu = grade.apply(pixel);
            let gpu = gpu_apply(pixel, head, tail, tint_amount, tint);
            for channel in 0..3 {
                assert!(
                    (cpu[channel] - gpu[channel]).abs() <= 1.0e-6,
                    "packed layout drifted for {pixel:?}: cpu {cpu:?} gpu {gpu:?}"
                );
            }
        }
    }

    #[test]
    fn invalid_controls_fail_closed() {
        assert_eq!(
            BlackAndWhiteGrade::new([0.0, 0.0, 0.0, 0.0, 0.0, 301.0], 0.0, [1.0; 3]),
            Err(BlackAndWhiteError::Weight(5))
        );
        assert_eq!(
            BlackAndWhiteGrade::new([f32::NAN; 6], 0.0, [1.0; 3]),
            Err(BlackAndWhiteError::Weight(0))
        );
        assert_eq!(
            BlackAndWhiteGrade::new([0.0; 6], 1.5, [1.0; 3]),
            Err(BlackAndWhiteError::TintAmount)
        );
        assert_eq!(
            BlackAndWhiteGrade::new([0.0; 6], 0.0, [1.0, f32::INFINITY, 1.0]),
            Err(BlackAndWhiteError::TintColor)
        );
    }
}
