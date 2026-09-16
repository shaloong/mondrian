//! Canonical Float32 coverage algebra shared by effect and renderer execution.
//!
//! Public working frames carry straight RGB plus normalized coverage Alpha.
//! Positive coverage is never classified as transparent: doing so creates a
//! visible discontinuity for 16-bit graphics, filtered edges, and repeated
//! composition. Callers must validate finite samples before entering these
//! primitives.

/// Return whether a normalized coverage or opacity contributes any signal.
///
/// Values are expected to have been clamped to `[0, 1]`. Non-positive values
/// are the only transparent identity; every positive Float32 value remains
/// semantically observable.
#[inline]
pub const fn has_positive_coverage(coverage: f32) -> bool {
    coverage > 0.0
}

/// Convert one premultiplied Float32 sample to the public straight-alpha form.
///
/// Exact zero coverage canonicalizes hidden RGB to zero. Every positive Alpha,
/// including the smallest representable 16-bit UNORM code, is unassociated
/// without an arbitrary epsilon floor.
#[inline]
pub fn straight_rgba_from_premultiplied(sample: [f32; 4]) -> [f32; 4] {
    let alpha = sample[3].clamp(0.0, 1.0);
    if !has_positive_coverage(alpha) {
        return [0.0; 4];
    }
    [
        sample[0] / alpha,
        sample[1] / alpha,
        sample[2] / alpha,
        alpha,
    ]
}

/// Interpolate two straight-alpha samples through premultiplied coverage.
///
/// `right_weight` is clamped to `[0, 1]`. The result returns to the public
/// straight-alpha contract and canonicalizes RGB only when output coverage is
/// exactly zero.
#[inline]
pub fn mix_straight_rgba(left: [f32; 4], right: [f32; 4], right_weight: f32) -> [f32; 4] {
    let right_weight = right_weight.clamp(0.0, 1.0);
    let left_weight = 1.0 - right_weight;
    let left_alpha = left[3].clamp(0.0, 1.0);
    let right_alpha = right[3].clamp(0.0, 1.0);
    let alpha = left_alpha.mul_add(left_weight, right_alpha * right_weight);
    if !has_positive_coverage(alpha) {
        return [0.0; 4];
    }
    [
        (left[0] * left_alpha * left_weight + right[0] * right_alpha * right_weight) / alpha,
        (left[1] * left_alpha * left_weight + right[1] * right_alpha * right_weight) / alpha,
        (left[2] * left_alpha * left_weight + right[2] * right_alpha * right_weight) / alpha,
        alpha,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f64_mix_reference(left: [f32; 4], right: [f32; 4], right_weight: f32) -> [f64; 4] {
        let right_weight = f64::from(right_weight.clamp(0.0, 1.0));
        let left_weight = 1.0 - right_weight;
        let left_alpha = f64::from(left[3].clamp(0.0, 1.0));
        let right_alpha = f64::from(right[3].clamp(0.0, 1.0));
        let alpha = left_alpha * left_weight + right_alpha * right_weight;
        if alpha == 0.0 {
            return [0.0; 4];
        }
        [
            (f64::from(left[0]) * left_alpha * left_weight
                + f64::from(right[0]) * right_alpha * right_weight)
                / alpha,
            (f64::from(left[1]) * left_alpha * left_weight
                + f64::from(right[1]) * right_alpha * right_weight)
                / alpha,
            (f64::from(left[2]) * left_alpha * left_weight
                + f64::from(right[2]) * right_alpha * right_weight)
                / alpha,
            alpha,
        ]
    }

    #[test]
    fn exact_zero_is_the_only_transparent_float_coverage() {
        assert!(!has_positive_coverage(0.0));
        assert!(!has_positive_coverage(-0.0));
        assert!(has_positive_coverage(f32::MIN_POSITIVE));
        assert!(has_positive_coverage(1.0 / 65_535.0));
    }

    #[test]
    fn premultiplied_sixteen_bit_edge_round_trips_without_a_coverage_floor() {
        let alpha = 1.0 / 65_535.0;
        let straight = [1.25, -0.125, 0.5, alpha];
        let premultiplied = [
            straight[0] * alpha,
            straight[1] * alpha,
            straight[2] * alpha,
            alpha,
        ];
        let actual = straight_rgba_from_premultiplied(premultiplied);

        for channel in 0..4 {
            assert!(
                (actual[channel] - straight[channel]).abs() <= 2.0 * f32::EPSILON,
                "channel {channel}: expected {}, got {}",
                straight[channel],
                actual[channel]
            );
        }
    }

    #[test]
    fn low_coverage_mix_matches_independent_f64_reference() {
        let corpus = [
            (
                [1.25, -0.25, 0.5, 1.0 / 65_535.0],
                [-0.5, 0.75, 2.0, 1.0 / 32_768.0],
                0.375,
            ),
            ([8.0, -4.0, 0.125, f32::EPSILON], [0.0; 4], 0.5),
            ([0.0; 4], [0.2, 0.4, 0.6, 1.0e-8], 0.25),
        ];

        for (left, right, weight) in corpus {
            let actual = mix_straight_rgba(left, right, weight);
            let expected = f64_mix_reference(left, right, weight);
            for channel in 0..4 {
                let error = (f64::from(actual[channel]) - expected[channel]).abs();
                assert!(
                    error <= 8.0 * f64::from(f32::EPSILON) * expected[channel].abs().max(1.0),
                    "channel {channel}: expected {}, got {}, error {error}",
                    expected[channel],
                    actual[channel]
                );
            }
        }
    }
}
