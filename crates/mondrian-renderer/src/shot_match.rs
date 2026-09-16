//! Deterministic working-linear statistics and affine Shot Match solution.
//!
//! This module intentionally does not author a Grade Graph. It measures the
//! renderer-owned working frame and produces a closed numeric result; the
//! authoring layer decides where that result becomes a Grade Version.

use mondrian_core::GalleryColorStatistics;

use crate::CpuColorFrame;

const MAX_ANALYSIS_SAMPLES: usize = 262_144;
const OPAQUE_SAMPLE_EPSILON: f32 = 1.0 / 65_535.0;
const SPAN_EPSILON: f32 = 1.0e-6;

/// Failure to derive trustworthy bounded statistics from one working frame.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ShotMatchAnalysisError {
    #[error("Shot Match requires a non-empty working frame")]
    EmptyFrame,
    #[error("Shot Match found no finite, visible working-linear samples")]
    NoVisibleSamples,
}

/// Deterministic affine correction suitable for ColorWheel Gain and Offset.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShotMatchSolution {
    pub gain_rgb: [f32; 3],
    pub offset_rgb: [f32; 3],
}

/// Measure bounded 5th/50th/95th per-channel percentiles in working-linear RGB.
///
/// Sampling is a deterministic regular stride, capped independently of image
/// resolution. Transparent and non-finite pixels do not influence the result.
pub fn analyze_shot_match_frame(
    frame: &CpuColorFrame,
) -> Result<GalleryColorStatistics, ShotMatchAnalysisError> {
    let pixels = &frame.rgba_f32().data;
    if pixels.is_empty() {
        return Err(ShotMatchAnalysisError::EmptyFrame);
    }
    let stride = pixels.len().div_ceil(MAX_ANALYSIS_SAMPLES).max(1);
    let mut channels = [Vec::new(), Vec::new(), Vec::new()];
    for pixel in pixels.iter().step_by(stride) {
        if !pixel[3].is_finite() || pixel[3] <= OPAQUE_SAMPLE_EPSILON {
            continue;
        }
        if pixel[..3].iter().any(|value| !value.is_finite()) {
            continue;
        }
        for channel in 0..3 {
            channels[channel].push(pixel[channel]);
        }
    }
    let sample_count = channels[0].len();
    if sample_count == 0 {
        return Err(ShotMatchAnalysisError::NoVisibleSamples);
    }
    for channel in &mut channels {
        channel.sort_by(f32::total_cmp);
    }
    let percentile = |channel: usize, numerator: usize| {
        let index = (sample_count - 1).saturating_mul(numerator).saturating_add(50) / 100;
        channels[channel][index]
    };
    Ok(GalleryColorStatistics {
        sample_count: u64::try_from(sample_count).unwrap_or(u64::MAX),
        low_rgb: [percentile(0, 5), percentile(1, 5), percentile(2, 5)],
        median_rgb: [percentile(0, 50), percentile(1, 50), percentile(2, 50)],
        high_rgb: [percentile(0, 95), percentile(1, 95), percentile(2, 95)],
    })
}

/// Solve one deterministic, bounded affine match from target to reference.
///
/// The 5th/95th percentile span determines Gain; the median determines Offset.
/// Degenerate channels retain unit gain. The result is clamped exactly to the
/// authored ColorWheel parameter contract.
pub fn solve_shot_match(
    reference: &GalleryColorStatistics,
    target: &GalleryColorStatistics,
) -> ShotMatchSolution {
    let mut gain_rgb = [1.0; 3];
    let mut offset_rgb = [0.0; 3];
    for channel in 0..3 {
        let reference_span = reference.high_rgb[channel] - reference.low_rgb[channel];
        let target_span = target.high_rgb[channel] - target.low_rgb[channel];
        let gain = if reference_span.is_finite()
            && target_span.is_finite()
            && reference_span >= 0.0
            && target_span > SPAN_EPSILON
        {
            reference_span / target_span
        } else {
            1.0
        };
        gain_rgb[channel] = gain.clamp(0.0, 4.0);
        offset_rgb[channel] = (reference.median_rgb[channel]
            - target.median_rgb[channel] * gain_rgb[channel])
            .clamp(-2.0, 2.0);
    }
    ShotMatchSolution { gain_rgb, offset_rgb }
}
