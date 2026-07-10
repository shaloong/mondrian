//! Accuracy contracts for renderer color-path validation.
//!
//! Scene-linear comparisons use numeric error metrics rather than display-referred
//! perceptual metrics. RGB and alpha have separate budgets because alpha is coverage,
//! not color, and must not be folded into an RGB error distribution.

use serde::Serialize;
use thiserror::Error;

/// Numeric error limits for one group of scene-linear channels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct LinearAccuracyBudget {
    /// Largest allowed absolute error for any finite sample.
    pub max_absolute_error: f64,
    /// Largest allowed root-mean-square error across finite samples.
    pub max_root_mean_square_error: f64,
    /// Largest allowed nearest-rank 99th-percentile absolute error.
    pub max_percentile_99_absolute_error: f64,
}

impl LinearAccuracyBudget {
    /// Construct a strict budget that rejects every non-finite sample.
    pub const fn finite(
        max_absolute_error: f64,
        max_root_mean_square_error: f64,
        max_percentile_99_absolute_error: f64,
    ) -> Self {
        Self {
            max_absolute_error,
            max_root_mean_square_error,
            max_percentile_99_absolute_error,
        }
    }

    fn validate(self, group: LinearAccuracyChannelGroup) -> Result<(), LinearAccuracyError> {
        for (metric, limit) in [
            ("max_absolute_error", self.max_absolute_error),
            (
                "max_root_mean_square_error",
                self.max_root_mean_square_error,
            ),
            (
                "max_percentile_99_absolute_error",
                self.max_percentile_99_absolute_error,
            ),
        ] {
            if !limit.is_finite() || limit < 0.0 {
                return Err(LinearAccuracyError::InvalidBudget { group, metric, limit });
            }
        }
        Ok(())
    }
}

/// Independent scene-linear accuracy limits for color and coverage channels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct LinearRgbaAccuracyBudget {
    /// Limits applied to the combined RGB sample distribution.
    pub rgb: LinearAccuracyBudget,
    /// Limits applied to alpha coverage samples.
    pub alpha: LinearAccuracyBudget,
}

/// Stable channel group used by accuracy diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LinearAccuracyChannelGroup {
    /// Scene-linear red, green, and blue samples.
    Rgb,
    /// Alpha coverage samples.
    Alpha,
}

/// Aggregate error statistics for one channel group.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct LinearAccuracyStatistics {
    /// Total compared samples, including non-finite samples.
    pub sample_count: u64,
    /// Samples where either the expected or observed value was non-finite.
    pub non_finite_sample_count: u64,
    /// Largest absolute error among finite samples.
    pub max_absolute_error: f64,
    /// Mean absolute error among finite samples.
    pub mean_absolute_error: f64,
    /// Root-mean-square error among finite samples.
    pub root_mean_square_error: f64,
    /// Nearest-rank 99th-percentile absolute error among finite samples.
    pub percentile_99_absolute_error: f64,
    /// Flattened sample index of the largest finite error.
    pub worst_sample_index: Option<u64>,
}

impl LinearAccuracyStatistics {
    /// Return whether all metrics satisfy the supplied budget.
    pub fn is_within(self, budget: LinearAccuracyBudget) -> bool {
        self.non_finite_sample_count == 0
            && self.max_absolute_error <= budget.max_absolute_error
            && self.root_mean_square_error <= budget.max_root_mean_square_error
            && self.percentile_99_absolute_error <= budget.max_percentile_99_absolute_error
    }
}

/// Statistics, budget, and verdict for one channel group.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct LinearAccuracyGroupReport {
    /// Compared channel group.
    pub group: LinearAccuracyChannelGroup,
    /// Observed error distribution.
    pub statistics: LinearAccuracyStatistics,
    /// Limits used to evaluate the distribution.
    pub budget: LinearAccuracyBudget,
    /// Whether every metric stayed within budget.
    pub within_budget: bool,
}

/// Complete scene-linear RGBA comparison report.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct LinearRgbaAccuracyReport {
    /// Number of compared pixels.
    pub pixel_count: u64,
    /// RGB error distribution and its verdict.
    pub rgb: LinearAccuracyGroupReport,
    /// Alpha error distribution and its verdict.
    pub alpha: LinearAccuracyGroupReport,
    /// True only when both RGB and alpha satisfy their independent budgets.
    pub within_budget: bool,
}

/// Failure to construct a scene-linear accuracy report.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum LinearAccuracyError {
    /// Expected and observed buffers contain different pixel counts.
    #[error("linear RGBA buffer length mismatch: expected {expected_pixels} pixels, observed {observed_pixels}")]
    LengthMismatch {
        /// Expected pixel count.
        expected_pixels: usize,
        /// Observed pixel count.
        observed_pixels: usize,
    },
    /// Empty comparisons cannot establish an accuracy result.
    #[error("linear RGBA accuracy comparison requires at least one pixel")]
    EmptyInput,
    /// A numeric budget must be finite and non-negative.
    #[error("invalid {group:?} accuracy budget {metric}={limit}; limits must be finite and non-negative")]
    InvalidBudget {
        /// Channel group containing the invalid limit.
        group: LinearAccuracyChannelGroup,
        /// Stable metric name.
        metric: &'static str,
        /// Invalid limit.
        limit: f64,
    },
}

/// Compare two scene-linear RGBA buffers using distribution-aware numeric limits.
///
/// RGB errors are flattened in pixel-major channel order. Alpha is evaluated
/// independently so exact coverage requirements can be stricter than shader color
/// arithmetic. NaN and infinity never participate in numeric aggregates and are
/// counted explicitly instead.
pub fn compare_linear_rgba(
    expected: &[[f32; 4]],
    observed: &[[f32; 4]],
    budget: LinearRgbaAccuracyBudget,
) -> Result<LinearRgbaAccuracyReport, LinearAccuracyError> {
    if expected.len() != observed.len() {
        return Err(LinearAccuracyError::LengthMismatch {
            expected_pixels: expected.len(),
            observed_pixels: observed.len(),
        });
    }
    if expected.is_empty() {
        return Err(LinearAccuracyError::EmptyInput);
    }
    budget.rgb.validate(LinearAccuracyChannelGroup::Rgb)?;
    budget.alpha.validate(LinearAccuracyChannelGroup::Alpha)?;

    let mut rgb = ErrorAccumulator::with_capacity(expected.len().saturating_mul(3));
    let mut alpha = ErrorAccumulator::with_capacity(expected.len());
    for (expected_pixel, observed_pixel) in expected.iter().zip(observed) {
        for channel in 0..3 {
            rgb.push(expected_pixel[channel], observed_pixel[channel]);
        }
        alpha.push(expected_pixel[3], observed_pixel[3]);
    }

    let rgb_statistics = rgb.finish();
    let alpha_statistics = alpha.finish();
    let rgb_within_budget = rgb_statistics.is_within(budget.rgb);
    let alpha_within_budget = alpha_statistics.is_within(budget.alpha);
    Ok(LinearRgbaAccuracyReport {
        pixel_count: expected.len() as u64,
        rgb: LinearAccuracyGroupReport {
            group: LinearAccuracyChannelGroup::Rgb,
            statistics: rgb_statistics,
            budget: budget.rgb,
            within_budget: rgb_within_budget,
        },
        alpha: LinearAccuracyGroupReport {
            group: LinearAccuracyChannelGroup::Alpha,
            statistics: alpha_statistics,
            budget: budget.alpha,
            within_budget: alpha_within_budget,
        },
        within_budget: rgb_within_budget && alpha_within_budget,
    })
}

struct ErrorAccumulator {
    errors: Vec<f64>,
    sample_count: u64,
    non_finite_sample_count: u64,
    sum_absolute_error: f64,
    sum_squared_error: f64,
    max_absolute_error: f64,
    worst_sample_index: Option<u64>,
}

impl ErrorAccumulator {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            errors: Vec::with_capacity(capacity),
            sample_count: 0,
            non_finite_sample_count: 0,
            sum_absolute_error: 0.0,
            sum_squared_error: 0.0,
            max_absolute_error: 0.0,
            worst_sample_index: None,
        }
    }

    fn push(&mut self, expected: f32, observed: f32) {
        let sample_index = self.sample_count;
        self.sample_count = self.sample_count.saturating_add(1);
        if !expected.is_finite() || !observed.is_finite() {
            self.non_finite_sample_count = self.non_finite_sample_count.saturating_add(1);
            return;
        }

        let error = (f64::from(expected) - f64::from(observed)).abs();
        self.errors.push(error);
        self.sum_absolute_error += error;
        self.sum_squared_error += error * error;
        if self.worst_sample_index.is_none() || error > self.max_absolute_error {
            self.max_absolute_error = error;
            self.worst_sample_index = Some(sample_index);
        }
    }

    fn finish(mut self) -> LinearAccuracyStatistics {
        let finite_count = self.errors.len() as f64;
        let (mean_absolute_error, root_mean_square_error, percentile_99_absolute_error) =
            if self.errors.is_empty() {
                (0.0, 0.0, 0.0)
            } else {
                self.errors.sort_by(f64::total_cmp);
                let percentile_index =
                    self.errors.len().saturating_mul(99).div_ceil(100).saturating_sub(1);
                (
                    self.sum_absolute_error / finite_count,
                    (self.sum_squared_error / finite_count).sqrt(),
                    self.errors[percentile_index],
                )
            };

        LinearAccuracyStatistics {
            sample_count: self.sample_count,
            non_finite_sample_count: self.non_finite_sample_count,
            max_absolute_error: self.max_absolute_error,
            mean_absolute_error,
            root_mean_square_error,
            percentile_99_absolute_error,
            worst_sample_index: self.worst_sample_index,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STRICT: LinearRgbaAccuracyBudget = LinearRgbaAccuracyBudget {
        rgb: LinearAccuracyBudget::finite(0.0625, 0.05, 0.0625),
        alpha: LinearAccuracyBudget::finite(0.01, 0.01, 0.01),
    };

    #[test]
    fn accuracy_report_keeps_rgb_and_alpha_semantics_separate() {
        let expected = [[0.0, 0.5, 1.0, 0.25], [1.5, -0.2, 0.3, 1.0]];
        let observed = [[0.0625, 0.4375, 0.9375, 0.255], [1.5, -0.2, 0.3, 1.0]];

        let report = compare_linear_rgba(&expected, &observed, STRICT).expect("valid report");

        assert_eq!(report.pixel_count, 2);
        assert_eq!(report.rgb.statistics.sample_count, 6);
        assert_eq!(report.alpha.statistics.sample_count, 2);
        assert_eq!(report.rgb.statistics.worst_sample_index, Some(0));
        assert_eq!(report.alpha.statistics.worst_sample_index, Some(0));
        assert!(report.within_budget, "{report:#?}");
    }

    #[test]
    fn distribution_limits_catch_broad_drift_below_the_peak_limit() {
        let expected = vec![[0.0; 4]; 100];
        let observed = vec![[0.05, 0.05, 0.05, 0.0]; 100];
        let budget = LinearRgbaAccuracyBudget {
            rgb: LinearAccuracyBudget::finite(0.1, 0.01, 0.1),
            alpha: LinearAccuracyBudget::finite(0.0, 0.0, 0.0),
        };

        let report = compare_linear_rgba(&expected, &observed, budget).expect("valid report");

        assert!(report.rgb.statistics.max_absolute_error < budget.rgb.max_absolute_error);
        assert!(
            report.rgb.statistics.root_mean_square_error > budget.rgb.max_root_mean_square_error
        );
        assert!(!report.within_budget);
    }

    #[test]
    fn non_finite_values_are_counted_and_fail_closed() {
        let expected = [[0.0, 0.0, 0.0, 1.0]];
        let observed = [[f32::NAN, f32::INFINITY, 0.0, f32::NEG_INFINITY]];

        let report = compare_linear_rgba(&expected, &observed, STRICT).expect("valid report");

        assert_eq!(report.rgb.statistics.non_finite_sample_count, 2);
        assert_eq!(report.alpha.statistics.non_finite_sample_count, 1);
        assert!(!report.within_budget);
    }

    #[test]
    fn invalid_budgets_and_shapes_are_structured_errors() {
        let invalid = LinearRgbaAccuracyBudget {
            rgb: LinearAccuracyBudget::finite(f64::NAN, 0.0, 0.0),
            alpha: LinearAccuracyBudget::finite(0.0, 0.0, 0.0),
        };
        assert!(matches!(
            compare_linear_rgba(&[[0.0; 4]], &[[0.0; 4]], invalid),
            Err(LinearAccuracyError::InvalidBudget {
                group: LinearAccuracyChannelGroup::Rgb,
                metric: "max_absolute_error",
                ..
            })
        ));
        assert!(matches!(
            compare_linear_rgba(&[[0.0; 4]], &[], STRICT),
            Err(LinearAccuracyError::LengthMismatch { .. })
        ));
        assert_eq!(
            compare_linear_rgba(&[], &[], STRICT),
            Err(LinearAccuracyError::EmptyInput)
        );
    }
}
