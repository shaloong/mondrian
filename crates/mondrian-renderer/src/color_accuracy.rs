//! Accuracy contracts for renderer color-path validation.
//!
//! Scene-linear comparisons use numeric error metrics rather than display-referred
//! perceptual metrics. RGB and alpha have separate budgets because alpha is coverage,
//! not color, and must not be folded into an RGB error distribution.

use mondrian_core::{
    bt2100_display_linear_rgb_to_itp, bt2100_pq_to_display_linear_rgb, delta_e_2000_d50,
    delta_e_itp, srgb_to_cie_lab_d50, ColorScienceError,
};
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

/// Perceptual CIEDE2000 limits for an SDR sRGB display boundary.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct SrgbDisplayAccuracyBudget {
    /// Largest allowed CIEDE2000 difference for any pixel.
    pub max_delta_e_2000: f64,
    /// Largest allowed mean CIEDE2000 difference across the frame.
    pub max_mean_delta_e_2000: f64,
    /// Largest allowed nearest-rank 99th-percentile CIEDE2000 difference.
    pub max_percentile_99_delta_e_2000: f64,
    /// Largest allowed encoded alpha code-value difference.
    pub max_alpha_code_value_delta: u8,
}

impl SrgbDisplayAccuracyBudget {
    /// Construct an SDR sRGB display-accuracy budget.
    pub const fn new(
        max_delta_e_2000: f64,
        max_mean_delta_e_2000: f64,
        max_percentile_99_delta_e_2000: f64,
        max_alpha_code_value_delta: u8,
    ) -> Self {
        Self {
            max_delta_e_2000,
            max_mean_delta_e_2000,
            max_percentile_99_delta_e_2000,
            max_alpha_code_value_delta,
        }
    }

    fn validate(self) -> Result<(), SrgbDisplayAccuracyError> {
        for (metric, limit) in [
            ("max_delta_e_2000", self.max_delta_e_2000),
            ("max_mean_delta_e_2000", self.max_mean_delta_e_2000),
            (
                "max_percentile_99_delta_e_2000",
                self.max_percentile_99_delta_e_2000,
            ),
        ] {
            if !limit.is_finite() || limit < 0.0 {
                return Err(SrgbDisplayAccuracyError::InvalidBudget { metric, limit });
            }
        }
        Ok(())
    }
}

/// Perceptual color-error distribution for an SDR sRGB display boundary.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct SrgbDisplayAccuracyStatistics {
    /// Number of compared pixels.
    pub pixel_count: u64,
    /// Largest observed CIEDE2000 difference.
    pub max_delta_e_2000: f64,
    /// Mean observed CIEDE2000 difference.
    pub mean_delta_e_2000: f64,
    /// Nearest-rank 99th-percentile CIEDE2000 difference.
    pub percentile_99_delta_e_2000: f64,
    /// Pixel index of the largest CIEDE2000 difference.
    pub worst_pixel_index: u64,
    /// Largest encoded alpha code-value difference.
    pub max_alpha_code_value_delta: u8,
    /// Pixel index of the largest alpha code-value difference.
    pub worst_alpha_pixel_index: u64,
}

/// Complete SDR sRGB display-boundary accuracy report.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct SrgbDisplayAccuracyReport {
    /// Observed perceptual color and alpha statistics.
    pub statistics: SrgbDisplayAccuracyStatistics,
    /// Limits used to evaluate the boundary.
    pub budget: SrgbDisplayAccuracyBudget,
    /// True only when color distribution and alpha coverage satisfy the budget.
    pub within_budget: bool,
}

/// Failure to construct an SDR sRGB display-accuracy report.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum SrgbDisplayAccuracyError {
    /// Expected and observed buffers have different byte lengths.
    #[error(
        "sRGB RGBA8 length mismatch: expected {expected_bytes} bytes, observed {observed_bytes}"
    )]
    LengthMismatch {
        /// Expected byte count.
        expected_bytes: usize,
        /// Observed byte count.
        observed_bytes: usize,
    },
    /// RGBA8 buffers must contain complete four-channel pixels.
    #[error("sRGB RGBA8 byte length {bytes} is not divisible by four")]
    InvalidRgbaLength {
        /// Invalid byte count.
        bytes: usize,
    },
    /// Empty comparisons cannot establish an accuracy result.
    #[error("sRGB display accuracy comparison requires at least one pixel")]
    EmptyInput,
    /// A perceptual budget must be finite and non-negative.
    #[error("invalid sRGB display accuracy budget {metric}={limit}; limits must be finite and non-negative")]
    InvalidBudget {
        /// Stable metric name.
        metric: &'static str,
        /// Invalid limit.
        limit: f64,
    },
    /// Device-independent conversion or CIEDE2000 evaluation failed.
    #[error("sRGB display accuracy failed at pixel {pixel_index}: {source}")]
    ColorScience {
        /// Pixel containing the invalid sample.
        pixel_index: usize,
        /// Underlying color-science failure.
        #[source]
        source: ColorScienceError,
    },
}

/// Compare two encoded sRGB RGBA8 display-boundary buffers perceptually.
///
/// RGB code values are converted explicitly to D50 CIELAB before CIEDE2000 is
/// evaluated. Alpha remains a coverage channel and is compared independently in
/// encoded code values. This function must not be used for Rec.709, Display P3,
/// PQ, HLG, scene-linear, or working-space buffers.
pub fn compare_srgb_display_rgba8(
    expected: &[u8],
    observed: &[u8],
    budget: SrgbDisplayAccuracyBudget,
) -> Result<SrgbDisplayAccuracyReport, SrgbDisplayAccuracyError> {
    if expected.len() != observed.len() {
        return Err(SrgbDisplayAccuracyError::LengthMismatch {
            expected_bytes: expected.len(),
            observed_bytes: observed.len(),
        });
    }
    if !expected.len().is_multiple_of(4) {
        return Err(SrgbDisplayAccuracyError::InvalidRgbaLength { bytes: expected.len() });
    }
    if expected.is_empty() {
        return Err(SrgbDisplayAccuracyError::EmptyInput);
    }
    budget.validate()?;

    let pixel_count = expected.len() / 4;
    let mut delta_e_values = Vec::with_capacity(pixel_count);
    let mut sum_delta_e = 0.0;
    let mut max_delta_e = 0.0;
    let mut worst_pixel_index = 0;
    let mut max_alpha_delta = 0;
    let mut worst_alpha_pixel_index = 0;
    for (pixel_index, (expected, observed)) in
        expected.chunks_exact(4).zip(observed.chunks_exact(4)).enumerate()
    {
        let expected_lab = srgb_to_cie_lab_d50(encoded_rgb(expected))
            .map_err(|source| SrgbDisplayAccuracyError::ColorScience { pixel_index, source })?;
        let observed_lab = srgb_to_cie_lab_d50(encoded_rgb(observed))
            .map_err(|source| SrgbDisplayAccuracyError::ColorScience { pixel_index, source })?;
        let delta_e = delta_e_2000_d50(expected_lab, observed_lab)
            .map_err(|source| SrgbDisplayAccuracyError::ColorScience { pixel_index, source })?;
        delta_e_values.push(delta_e);
        sum_delta_e += delta_e;
        if pixel_index == 0 || delta_e > max_delta_e {
            max_delta_e = delta_e;
            worst_pixel_index = pixel_index;
        }

        let alpha_delta = expected[3].abs_diff(observed[3]);
        if pixel_index == 0 || alpha_delta > max_alpha_delta {
            max_alpha_delta = alpha_delta;
            worst_alpha_pixel_index = pixel_index;
        }
    }

    let percentile_index = delta_e_values.len().saturating_mul(99).div_ceil(100).saturating_sub(1);
    let percentile_99_delta_e_2000 =
        *delta_e_values.select_nth_unstable_by(percentile_index, f64::total_cmp).1;
    let statistics = SrgbDisplayAccuracyStatistics {
        pixel_count: pixel_count as u64,
        max_delta_e_2000: max_delta_e,
        mean_delta_e_2000: sum_delta_e / pixel_count as f64,
        percentile_99_delta_e_2000,
        worst_pixel_index: worst_pixel_index as u64,
        max_alpha_code_value_delta: max_alpha_delta,
        worst_alpha_pixel_index: worst_alpha_pixel_index as u64,
    };
    let within_budget = statistics.max_delta_e_2000 <= budget.max_delta_e_2000
        && statistics.mean_delta_e_2000 <= budget.max_mean_delta_e_2000
        && statistics.percentile_99_delta_e_2000 <= budget.max_percentile_99_delta_e_2000
        && statistics.max_alpha_code_value_delta <= budget.max_alpha_code_value_delta;
    Ok(SrgbDisplayAccuracyReport { statistics, budget, within_budget })
}

/// Perceptual and coverage limits for a display-referred BT.2100 PQ boundary.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct PqHdrDisplayAccuracyBudget {
    /// Largest allowed BT.2124 ΔEITP for any pixel.
    pub max_delta_e_itp: f64,
    /// Largest allowed mean ΔEITP across the frame.
    pub max_mean_delta_e_itp: f64,
    /// Largest allowed nearest-rank 99th-percentile ΔEITP.
    pub max_percentile_99_delta_e_itp: f64,
    /// Largest allowed absolute alpha coverage error.
    pub max_alpha_absolute_error: f64,
}

impl PqHdrDisplayAccuracyBudget {
    /// Construct a BT.2100 PQ display-accuracy budget.
    pub const fn new(
        max_delta_e_itp: f64,
        max_mean_delta_e_itp: f64,
        max_percentile_99_delta_e_itp: f64,
        max_alpha_absolute_error: f64,
    ) -> Self {
        Self {
            max_delta_e_itp,
            max_mean_delta_e_itp,
            max_percentile_99_delta_e_itp,
            max_alpha_absolute_error,
        }
    }
}

/// Error distribution for a display-referred BT.2100 PQ boundary.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct PqHdrDisplayAccuracyStatistics {
    /// Number of compared pixels.
    pub pixel_count: u64,
    /// Largest observed BT.2124 ΔEITP.
    pub max_delta_e_itp: f64,
    /// Mean observed BT.2124 ΔEITP.
    pub mean_delta_e_itp: f64,
    /// Nearest-rank 99th-percentile BT.2124 ΔEITP.
    pub percentile_99_delta_e_itp: f64,
    /// Pixel index containing the largest perceptual error.
    pub worst_pixel_index: u64,
    /// Largest absolute alpha coverage error.
    pub max_alpha_absolute_error: f64,
    /// Pixel index containing the largest alpha error.
    pub worst_alpha_pixel_index: u64,
}

/// Complete BT.2100 PQ display-boundary accuracy report.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct PqHdrDisplayAccuracyReport {
    /// Observed perceptual color and alpha statistics.
    pub statistics: PqHdrDisplayAccuracyStatistics,
    /// Limits used to evaluate the boundary.
    pub budget: PqHdrDisplayAccuracyBudget,
    /// True only when color and coverage satisfy every limit.
    pub within_budget: bool,
}

/// Failure to compare display-referred BT.2100 PQ frames.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum PqHdrDisplayAccuracyError {
    /// Expected and observed buffers contain different pixel counts.
    #[error("PQ HDR RGBA length mismatch: expected {expected_pixels} pixels, observed {observed_pixels}")]
    LengthMismatch {
        /// Expected pixel count.
        expected_pixels: usize,
        /// Observed pixel count.
        observed_pixels: usize,
    },
    /// Empty comparisons cannot establish an accuracy result.
    #[error("PQ HDR display accuracy comparison requires at least one pixel")]
    EmptyInput,
    /// A budget component was not finite or was negative.
    #[error("invalid PQ HDR display accuracy budget {metric}={limit}; limits must be finite and non-negative")]
    InvalidBudget {
        /// Stable metric name.
        metric: &'static str,
        /// Invalid limit.
        limit: f64,
    },
    /// A PQ RGB or alpha sample was invalid.
    #[error("PQ HDR display accuracy failed at pixel {pixel_index}: {source}")]
    ColorScience {
        /// Pixel containing the invalid sample.
        pixel_index: usize,
        /// Underlying color-science failure.
        #[source]
        source: ColorScienceError,
    },
    /// Alpha coverage was NaN or infinite.
    #[error("non-finite PQ HDR alpha at pixel {pixel_index}")]
    NonFiniteAlpha {
        /// Pixel containing invalid alpha.
        pixel_index: usize,
    },
}

/// Compare normalized full-range BT.2100 PQ RGBA float frames using ΔEITP.
///
/// RGB is decoded to absolute display luminance before conversion to ITP.
/// Alpha remains independent coverage and is never included in ΔEITP.
pub fn compare_pq_hdr_display_rgba(
    expected: &[[f32; 4]],
    observed: &[[f32; 4]],
    budget: PqHdrDisplayAccuracyBudget,
) -> Result<PqHdrDisplayAccuracyReport, PqHdrDisplayAccuracyError> {
    if expected.len() != observed.len() {
        return Err(PqHdrDisplayAccuracyError::LengthMismatch {
            expected_pixels: expected.len(),
            observed_pixels: observed.len(),
        });
    }
    if expected.is_empty() {
        return Err(PqHdrDisplayAccuracyError::EmptyInput);
    }
    for (metric, limit) in [
        ("max_delta_e_itp", budget.max_delta_e_itp),
        ("max_mean_delta_e_itp", budget.max_mean_delta_e_itp),
        (
            "max_percentile_99_delta_e_itp",
            budget.max_percentile_99_delta_e_itp,
        ),
        ("max_alpha_absolute_error", budget.max_alpha_absolute_error),
    ] {
        if !limit.is_finite() || limit < 0.0 {
            return Err(PqHdrDisplayAccuracyError::InvalidBudget { metric, limit });
        }
    }

    let mut deltas = Vec::with_capacity(expected.len());
    let mut sum = 0.0;
    let mut max_delta = 0.0;
    let mut worst_pixel = 0;
    let mut max_alpha = 0.0;
    let mut worst_alpha = 0;
    for (pixel_index, (expected, observed)) in expected.iter().zip(observed).enumerate() {
        let expected_itp = pq_pixel_to_itp(*expected, pixel_index)?;
        let observed_itp = pq_pixel_to_itp(*observed, pixel_index)?;
        let delta = delta_e_itp(expected_itp, observed_itp)
            .map_err(|source| PqHdrDisplayAccuracyError::ColorScience { pixel_index, source })?;
        deltas.push(delta);
        sum += delta;
        if pixel_index == 0 || delta > max_delta {
            max_delta = delta;
            worst_pixel = pixel_index;
        }

        if !expected[3].is_finite() || !observed[3].is_finite() {
            return Err(PqHdrDisplayAccuracyError::NonFiniteAlpha { pixel_index });
        }
        let alpha = (f64::from(expected[3]) - f64::from(observed[3])).abs();
        if pixel_index == 0 || alpha > max_alpha {
            max_alpha = alpha;
            worst_alpha = pixel_index;
        }
    }

    let percentile_index = deltas.len().saturating_mul(99).div_ceil(100).saturating_sub(1);
    let percentile = *deltas.select_nth_unstable_by(percentile_index, f64::total_cmp).1;
    let statistics = PqHdrDisplayAccuracyStatistics {
        pixel_count: expected.len() as u64,
        max_delta_e_itp: max_delta,
        mean_delta_e_itp: sum / expected.len() as f64,
        percentile_99_delta_e_itp: percentile,
        worst_pixel_index: worst_pixel as u64,
        max_alpha_absolute_error: max_alpha,
        worst_alpha_pixel_index: worst_alpha as u64,
    };
    let within_budget = max_delta <= budget.max_delta_e_itp
        && statistics.mean_delta_e_itp <= budget.max_mean_delta_e_itp
        && percentile <= budget.max_percentile_99_delta_e_itp
        && max_alpha <= budget.max_alpha_absolute_error;
    Ok(PqHdrDisplayAccuracyReport { statistics, budget, within_budget })
}

fn pq_pixel_to_itp(
    pixel: [f32; 4],
    pixel_index: usize,
) -> Result<mondrian_core::ItpDisplay, PqHdrDisplayAccuracyError> {
    let rgb = [
        f64::from(pixel[0]),
        f64::from(pixel[1]),
        f64::from(pixel[2]),
    ];
    let linear = bt2100_pq_to_display_linear_rgb(rgb)
        .map_err(|source| PqHdrDisplayAccuracyError::ColorScience { pixel_index, source })?;
    bt2100_display_linear_rgb_to_itp(linear)
        .map_err(|source| PqHdrDisplayAccuracyError::ColorScience { pixel_index, source })
}

fn encoded_rgb(pixel: &[u8]) -> [f64; 3] {
    [
        f64::from(pixel[0]) / 255.0,
        f64::from(pixel[1]) / 255.0,
        f64::from(pixel[2]) / 255.0,
    ]
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
                let percentile_index =
                    self.errors.len().saturating_mul(99).div_ceil(100).saturating_sub(1);
                let percentile =
                    *self.errors.select_nth_unstable_by(percentile_index, f64::total_cmp).1;
                (
                    self.sum_absolute_error / finite_count,
                    (self.sum_squared_error / finite_count).sqrt(),
                    percentile,
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
    use half::f16;

    const STRICT: LinearRgbaAccuracyBudget = LinearRgbaAccuracyBudget {
        rgb: LinearAccuracyBudget::finite(0.0625, 0.05, 0.0625),
        alpha: LinearAccuracyBudget::finite(0.01, 0.01, 0.01),
    };

    const PQ_STRICT: PqHdrDisplayAccuracyBudget =
        PqHdrDisplayAccuracyBudget::new(1.0, 0.25, 0.75, 0.001);

    #[test]
    fn pq_hdr_report_uses_delta_e_itp_and_separate_alpha_coverage() {
        let expected = [[0.25, 0.35, 0.50, 1.0], [0.58, 0.40, 0.20, 0.5]];
        let observed = expected;
        let report =
            compare_pq_hdr_display_rgba(&expected, &observed, PQ_STRICT).expect("valid PQ report");

        assert_eq!(report.statistics.pixel_count, 2);
        assert_eq!(report.statistics.max_delta_e_itp, 0.0);
        assert_eq!(report.statistics.max_alpha_absolute_error, 0.0);
        assert!(report.within_budget);
    }

    #[test]
    fn pq_hdr_report_fails_closed_for_invalid_signal_and_distribution() {
        let expected = vec![[0.5, 0.5, 0.5, 1.0]; 100];
        let observed = vec![[0.5005, 0.5005, 0.5005, 1.0]; 100];
        let budget = PqHdrDisplayAccuracyBudget::new(10.0, 0.01, 10.0, 0.0);
        let report =
            compare_pq_hdr_display_rgba(&expected, &observed, budget).expect("valid PQ report");
        assert!(report.statistics.max_delta_e_itp < budget.max_delta_e_itp);
        assert!(report.statistics.mean_delta_e_itp > budget.max_mean_delta_e_itp);
        assert!(!report.within_budget);

        let invalid = [[1.01, 0.0, 0.0, 1.0]];
        assert!(matches!(
            compare_pq_hdr_display_rgba(&invalid, &invalid, PQ_STRICT),
            Err(PqHdrDisplayAccuracyError::ColorScience { pixel_index: 0, .. })
        ));
    }

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

    #[test]
    fn srgb_display_report_separates_perceptual_color_from_alpha_coverage() {
        let expected = [0, 0, 0, 255, 128, 128, 128, 128, 255, 255, 255, 0];
        let observed = [1, 1, 1, 255, 128, 128, 128, 126, 254, 255, 255, 0];
        let budget = SrgbDisplayAccuracyBudget::new(0.5, 0.25, 0.5, 1);

        let report =
            compare_srgb_display_rgba8(&expected, &observed, budget).expect("valid report");

        assert_eq!(report.statistics.pixel_count, 3);
        assert!(report.statistics.max_delta_e_2000 > 0.0);
        assert_eq!(report.statistics.max_alpha_code_value_delta, 2);
        assert!(!report.within_budget);
    }

    #[test]
    fn srgb_display_distribution_budget_detects_broad_low_level_drift() {
        let expected = vec![128; 4 * 100];
        let mut observed = expected.clone();
        for pixel in observed.chunks_exact_mut(4) {
            pixel[0] = 129;
        }
        let permissive = SrgbDisplayAccuracyBudget::new(1.0, 1.0, 1.0, 0);
        let baseline =
            compare_srgb_display_rgba8(&expected, &observed, permissive).expect("baseline report");
        let budget = SrgbDisplayAccuracyBudget::new(
            baseline.statistics.max_delta_e_2000 + 0.01,
            baseline.statistics.mean_delta_e_2000 - 0.01,
            baseline.statistics.percentile_99_delta_e_2000 + 0.01,
            0,
        );

        let report =
            compare_srgb_display_rgba8(&expected, &observed, budget).expect("valid report");

        assert!(!report.within_budget);
        assert!(report.statistics.max_delta_e_2000 <= budget.max_delta_e_2000);
        assert!(report.statistics.mean_delta_e_2000 > budget.max_mean_delta_e_2000);
    }

    #[test]
    fn srgb_display_comparison_rejects_invalid_shapes_and_budgets() {
        let valid_budget = SrgbDisplayAccuracyBudget::new(1.0, 1.0, 1.0, 0);
        assert!(matches!(
            compare_srgb_display_rgba8(&[0; 4], &[0; 8], valid_budget),
            Err(SrgbDisplayAccuracyError::LengthMismatch { .. })
        ));
        assert!(matches!(
            compare_srgb_display_rgba8(&[0; 3], &[0; 3], valid_budget),
            Err(SrgbDisplayAccuracyError::InvalidRgbaLength { .. })
        ));
        assert!(matches!(
            compare_srgb_display_rgba8(&[], &[], valid_budget),
            Err(SrgbDisplayAccuracyError::EmptyInput)
        ));
        assert!(matches!(
            compare_srgb_display_rgba8(
                &[0; 4],
                &[0; 4],
                SrgbDisplayAccuracyBudget::new(f64::NAN, 1.0, 1.0, 0)
            ),
            Err(SrgbDisplayAccuracyError::InvalidBudget { metric: "max_delta_e_2000", .. })
        ));
    }

    fn f16_round_trip(value: f32) -> f32 {
        f16::from_f32(value).to_f32()
    }

    #[test]
    fn f16_encoded_boundary_error_fits_one_twelve_bit_code() {
        let mut max_absolute_error = 0.0_f32;
        let mut sum_squared_error = 0.0_f64;
        for code in 0..=65_535_u32 {
            let value = code as f32 / 65_535.0;
            let error = (f16_round_trip(value) - value).abs();
            max_absolute_error = max_absolute_error.max(error);
            sum_squared_error += f64::from(error).powi(2);
        }
        let rms = (sum_squared_error / 65_536.0).sqrt();
        let twelve_bit_codes = max_absolute_error * 4_095.0;

        eprintln!(
            "f16 encoded boundary: max_abs={max_absolute_error:.9} rms={rms:.9} max_12bit_codes={twelve_bit_codes:.4}"
        );
        assert!(max_absolute_error <= 1.0 / 4_096.0);
        assert!(twelve_bit_codes <= 1.0);
    }

    #[test]
    fn f16_scene_linear_range_has_bounded_relative_error() {
        let mut max_absolute_error = 0.0_f32;
        let mut max_relative_error = 0.0_f32;
        for index in -65_536_i32..=65_536_i32 {
            let value = index as f32 / 4_096.0;
            let error = (f16_round_trip(value) - value).abs();
            max_absolute_error = max_absolute_error.max(error);
            if value.abs() >= f32::from(f16::MIN_POSITIVE) {
                max_relative_error = max_relative_error.max(error / value.abs());
            }
        }

        eprintln!(
            "f16 scene-linear [-16,16]: max_abs={max_absolute_error:.9} max_relative={max_relative_error:.9}"
        );
        assert!(max_absolute_error <= 1.0 / 256.0);
        assert!(max_relative_error <= 1.0 / 2_048.0);
    }

    #[test]
    fn hypothetical_f16_working_composite_exceeds_boundary_code_budget() {
        let mut reference = [0.0_f32; 3];
        let mut quantized = [0.0_f32; 3];
        for layer in 0..256_u32 {
            let alpha = ((layer * 17 % 79) + 1) as f32 / 100.0;
            let color = [
                (layer * 37 % 257) as f32 / 16.0,
                (layer * 53 % 193) as f32 / 12.0,
                (layer * 29 % 149) as f32 / 10.0,
            ];
            for channel in 0..3 {
                reference[channel] = color[channel] * alpha + reference[channel] * (1.0 - alpha);
                let blended = color[channel] * alpha + quantized[channel] * (1.0 - alpha);
                quantized[channel] = f16_round_trip(blended);
            }
        }
        let max_absolute_error = reference
            .iter()
            .zip(quantized.iter())
            .map(|(expected, observed)| (expected - observed).abs())
            .fold(0.0_f32, f32::max);

        eprintln!(
            "hypothetical 256-layer f16 working composite: reference={reference:?} quantized={quantized:?} max_abs={max_absolute_error:.9}"
        );
        assert!(max_absolute_error > 1.0 / 4_096.0);
        assert!(max_absolute_error < 0.02);
    }

    #[test]
    fn rgba32_working_precision_cost_is_explicit_at_4k() {
        let pixels = 3_840_u64 * 2_160;
        let rgba16_bytes = pixels * 8;
        let rgba32_bytes = pixels * 16;

        eprintln!(
            "4K texture precision cost: rgba16={:.2} MiB rgba32={:.2} MiB ping_pong_rgba16={:.2} MiB ping_pong_rgba32={:.2} MiB",
            rgba16_bytes as f64 / 1_048_576.0,
            rgba32_bytes as f64 / 1_048_576.0,
            (rgba16_bytes * 2) as f64 / 1_048_576.0,
            (rgba32_bytes * 2) as f64 / 1_048_576.0,
        );
        assert_eq!(rgba16_bytes, 66_355_200);
        assert_eq!(rgba32_bytes, 132_710_400);
        assert_eq!(rgba32_bytes, rgba16_bytes * 2);
    }
}
