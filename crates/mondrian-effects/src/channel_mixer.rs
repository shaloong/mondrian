//! Channel mixer (COL-13) and inverse color matrix (CMT-13).
//!
//! Builds each output channel from a weighted sum of the three input channels plus a constant.
//! The weights are authored as percentages, which is how the effect list describes them: `100`
//! keeps a channel, `0` removes it, `-100` subtracts it and `±200` doubles it with the opposite
//! sign. The mix is a plain linear combination, so it is well defined in the scene-linear working
//! domain of the effect graph and never needs a display-referred detour.

/// Number of authored weights: three outputs times four inputs (red, green, blue, constant).
pub const CHANNEL_MIXER_WEIGHT_COUNT: usize = 12;

/// Authored weight bounds in percent.
pub const CHANNEL_MIXER_WEIGHT_MIN: f32 = -200.0;

/// Authored weight bounds in percent.
pub const CHANNEL_MIXER_WEIGHT_MAX: f32 = 200.0;

/// The resolved channel mixer transform.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChannelMixerGrade {
    /// Weights in percent, ordered red output (red, green, blue, constant), then green, then blue.
    weights: [f32; CHANNEL_MIXER_WEIGHT_COUNT],
    monochrome: bool,
}

/// Rejected channel mixer authoring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ChannelMixerError {
    /// A weight was not finite or outside the authored bounds.
    #[error("channel mixer weight {index} must be finite and within [-200, 200] percent")]
    Weight {
        /// Zero-based weight index.
        index: usize,
    },
    /// Repeating a single output removes two independent color channels.
    #[error("channel mixer cannot invert a monochrome output")]
    MonochromeInverse,
    /// The authored matrix has no stable Float32 inverse.
    #[error("channel mixer matrix is singular or too ill-conditioned to invert in Float32")]
    UnstableInverse,
}

impl ChannelMixerGrade {
    /// Validates one authored channel mixer setup.
    pub fn new(
        weights: [f32; CHANNEL_MIXER_WEIGHT_COUNT],
        monochrome: bool,
    ) -> Result<Self, ChannelMixerError> {
        for (index, weight) in weights.iter().enumerate() {
            if !weight.is_finite()
                || !(CHANNEL_MIXER_WEIGHT_MIN..=CHANNEL_MIXER_WEIGHT_MAX).contains(weight)
            {
                return Err(ChannelMixerError::Weight { index });
            }
        }
        Ok(Self { weights, monochrome })
    }

    /// Resolve the inverse of the authored affine transform before pixel execution.
    pub fn inverse(self) -> Result<Self, ChannelMixerError> {
        use glam::{DMat3, DVec3};

        if self.monochrome {
            return Err(ChannelMixerError::MonochromeInverse);
        }
        let w = self.weights.map(|weight| f64::from(weight) / 100.0);
        let matrix =
            DMat3::from_cols_array(&[w[0], w[4], w[8], w[1], w[5], w[9], w[2], w[6], w[10]]);
        if matrix.determinant() == 0.0 {
            return Err(ChannelMixerError::UnstableInverse);
        }
        let inverse = matrix.inverse();
        let norm = |matrix: DMat3| {
            (matrix.x_axis.abs() + matrix.y_axis.abs() + matrix.z_axis.abs()).max_element()
        };
        // The bound is scale-relative: uniformly small but well-conditioned
        // transforms remain invertible. Reject unstable cancellation in Float32.
        if norm(matrix) * norm(inverse) > 1.0e6 {
            return Err(ChannelMixerError::UnstableInverse);
        }
        let offset = -(inverse * DVec3::new(w[3], w[7], w[11]));
        let [a, b, c, d, e, f, g, h, i] = inverse.to_cols_array();
        let weights = [a, d, g, offset.x, b, e, h, offset.y, c, f, i, offset.z]
            .map(|value| (value * 100.0) as f32);
        if weights.iter().any(|value| !value.is_finite()) {
            return Err(ChannelMixerError::UnstableInverse);
        }
        Ok(Self { weights, monochrome: false })
    }

    /// Resolved weights in percent. An inverse may exceed the authoring bounds.
    pub const fn weights(self) -> [f32; CHANNEL_MIXER_WEIGHT_COUNT] {
        self.weights
    }

    /// Whether every output channel receives the red output instead of its own mix.
    pub const fn monochrome(self) -> bool {
        self.monochrome
    }

    /// Whether the setup leaves every pixel untouched.
    pub fn is_identity(self) -> bool {
        !self.monochrome
            && self.weights
                == [
                    100.0, 0.0, 0.0, 0.0, 0.0, 100.0, 0.0, 0.0, 0.0, 0.0, 100.0, 0.0,
                ]
    }

    /// Mixes one straight-alpha scene-linear pixel.
    pub fn apply(self, rgb: [f32; 3]) -> [f32; 3] {
        let red = self.channel(0, rgb);
        if self.monochrome {
            return [red, red, red];
        }
        [red, self.channel(4, rgb), self.channel(8, rgb)]
    }

    /// Packs the grade for the fused GPU point path.
    ///
    /// The rows carry the red output weights, the green output weights, the blue output weights
    /// and finally the monochrome flag in `x`.
    pub fn gpu_controls(self) -> ([f32; 4], [f32; 4], [f32; 4], [f32; 4]) {
        let w = self.weights;
        (
            [w[0], w[1], w[2], w[3]],
            [w[4], w[5], w[6], w[7]],
            [w[8], w[9], w[10], w[11]],
            [if self.monochrome { 1.0 } else { 0.0 }, 0.0, 0.0, 0.0],
        )
    }

    fn channel(self, base: usize, rgb: [f32; 3]) -> f32 {
        let weights = &self.weights[base..base + 4];
        (weights[0] * rgb[0] + weights[1] * rgb[1] + weights[2] * rgb[2] + weights[3] * 1.0) / 100.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDENTITY: [f32; 12] = [
        100.0, 0.0, 0.0, 0.0, 0.0, 100.0, 0.0, 0.0, 0.0, 0.0, 100.0, 0.0,
    ];

    fn grade(weights: [f32; 12]) -> ChannelMixerGrade {
        ChannelMixerGrade::new(weights, false).expect("valid weights")
    }

    #[test]
    fn identity_weights_are_the_identity() {
        assert!(grade(IDENTITY).is_identity());
        assert_eq!(grade(IDENTITY).apply([0.25, -0.5, 2.0]), [0.25, -0.5, 2.0]);
    }

    #[test]
    fn inverse_rejects_unrecoverable_or_ill_conditioned_matrices() {
        let mut weights = IDENTITY;
        weights[10] = 0.0;
        assert_eq!(
            grade(weights).inverse(),
            Err(ChannelMixerError::UnstableInverse)
        );
        weights[10] = 1.0e-6;
        assert_eq!(
            grade(weights).inverse(),
            Err(ChannelMixerError::UnstableInverse)
        );
        let monochrome = ChannelMixerGrade::new(IDENTITY, true).unwrap();
        assert_eq!(
            monochrome.inverse(),
            Err(ChannelMixerError::MonochromeInverse)
        );
    }

    #[test]
    fn swapping_output_rows_swaps_channels() {
        let swapped = grade([
            0.0, 100.0, 0.0, 0.0, 100.0, 0.0, 0.0, 0.0, 0.0, 0.0, 100.0, 0.0,
        ]);
        assert!(!swapped.is_identity());
        assert_eq!(swapped.apply([0.1, 0.2, 0.3]), [0.2, 0.1, 0.3]);
    }

    #[test]
    fn constant_weights_add_a_scaled_offset() {
        let lifted = grade([
            100.0, 0.0, 0.0, 25.0, 0.0, 100.0, 0.0, -50.0, 0.0, 0.0, 100.0, 0.0,
        ]);
        assert_eq!(lifted.apply([0.5, 0.5, 0.5]), [0.75, 0.0, 0.5]);
    }

    #[test]
    fn negative_and_out_of_range_values_are_not_clamped() {
        let inverted = grade([
            -100.0, 0.0, 0.0, 0.0, 0.0, -100.0, 0.0, 0.0, 0.0, 0.0, 200.0, 0.0,
        ]);
        assert_eq!(inverted.apply([0.25, 2.0, -0.5]), [-0.25, -2.0, -1.0]);
    }

    #[test]
    fn monochrome_repeats_the_red_output() {
        let muted = ChannelMixerGrade::new(
            [
                30.0, 59.0, 11.0, 0.0, 0.0, 100.0, 0.0, 0.0, 0.0, 0.0, 100.0, 0.0,
            ],
            true,
        )
        .expect("valid weights");
        let gray = muted.apply([1.0, 0.0, 0.0]);
        assert_eq!(gray, [0.3, 0.3, 0.3]);
        assert!(!muted.is_identity());
    }

    #[test]
    fn weights_outside_the_authored_bounds_fail_closed() {
        let mut weights = IDENTITY;
        weights[5] = 200.5;
        assert_eq!(
            ChannelMixerGrade::new(weights, false),
            Err(ChannelMixerError::Weight { index: 5 })
        );
        let mut not_finite = IDENTITY;
        not_finite[0] = f32::NAN;
        assert_eq!(
            ChannelMixerGrade::new(not_finite, false),
            Err(ChannelMixerError::Weight { index: 0 })
        );
    }

    #[test]
    fn gpu_controls_round_trip_the_authored_weights() {
        let weights = [
            12.0, -34.0, 56.0, 7.0, 89.0, -10.0, 11.0, 12.0, 13.0, 14.0, 15.0, 16.0,
        ];
        let grade = ChannelMixerGrade::new(weights, true).expect("valid weights");
        let (params, color, extra0, extra1) = grade.gpu_controls();
        assert_eq!(params, [12.0, -34.0, 56.0, 7.0]);
        assert_eq!(color, [89.0, -10.0, 11.0, 12.0]);
        assert_eq!(extra0, [13.0, 14.0, 15.0, 16.0]);
        assert_eq!(extra1, [1.0, 0.0, 0.0, 0.0]);
        assert_eq!(grade.weights(), weights);
        assert!(grade.monochrome());
    }
}
