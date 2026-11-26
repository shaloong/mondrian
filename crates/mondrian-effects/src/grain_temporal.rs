//! Finite motion-gated temporal grain reduction on straight Float32 pixels.

use crate::{grain_calibration::GrainCalibrationRequest, EffectTemporalInputExtent};
use mondrian_core::TimelineTime;

/// Prepared per-frame operation with frozen calibration and zero to two taps.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GrainTemporalGrade {
    offsets: [Option<TimelineTime>; 2],
    sigma: [f32; 3],
    mix: f32,
    motion_sensitivity: f32,
    calibration: Option<GrainCalibrationRequest>,
}

/// Rejected temporal input addresses or controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GrainTemporalError {
    /// Each present neighbor must be on its declared side of zero.
    #[error(
        "grain temporal neighbors require a negative previous and a positive following offset"
    )]
    InvalidOffsets,
    /// Noise bounds and unit controls must be finite.
    #[error("grain temporal requires finite nonnegative sigma and unit mix and sensitivity")]
    InvalidControls,
}

impl GrainTemporalGrade {
    /// Bind exact previous/following author-domain offsets and a common RGB
    /// profile. A missing neighbor contributes zero weight. A zero mix omits
    /// all neighbor requests.
    pub fn new(
        [previous, following]: [Option<TimelineTime>; 2],
        sigma: [f32; 3],
        mix: f32,
        motion_sensitivity: f32,
    ) -> Result<Self, GrainTemporalError> {
        if previous.is_some_and(|time| !time.is_negative())
            || following.is_some_and(|time| time.is_negative() || time.is_zero())
        {
            return Err(GrainTemporalError::InvalidOffsets);
        }
        if !sigma.iter().all(|value| value.is_finite() && *value >= 0.0)
            || !(0.0..=1.0).contains(&mix)
            || !(0.0..=1.0).contains(&motion_sensitivity)
        {
            return Err(GrainTemporalError::InvalidControls);
        }
        let offsets = if mix == 0.0 {
            [None, None]
        } else {
            [previous, following]
        };
        Ok(Self {
            offsets,
            sigma,
            mix,
            motion_sensitivity,
            calibration: None,
        })
    }

    /// Declare host calibration with a conservative window for graph validation.
    pub fn awaiting_source_calibration(
        offsets: [Option<TimelineTime>; 2],
        calibration: GrainCalibrationRequest,
        mix: f32,
        motion_sensitivity: f32,
    ) -> Result<Self, GrainTemporalError> {
        let mut grade = Self::new(offsets, [0.0; 3], mix, motion_sensitivity)?;
        grade.calibration = Some(calibration);
        Ok(grade)
    }

    pub(crate) const fn calibration(self) -> Option<GrainCalibrationRequest> {
        self.calibration
    }

    pub(crate) fn bind_calibration(
        self,
        sigma: Option<[f32; 3]>,
        mut admit_offset: impl FnMut(TimelineTime) -> Result<bool, String>,
    ) -> Result<Self, String> {
        let mut offsets = [None; 2];
        if sigma.is_some() {
            for (bound, pending) in offsets.iter_mut().zip(self.offsets) {
                if let Some(offset) = pending
                    && admit_offset(offset)?
                {
                    *bound = Some(offset);
                }
            }
        }
        Self::new(
            offsets,
            sigma.unwrap_or([0.0; 3]),
            self.mix,
            self.motion_sensitivity,
        )
        .map_err(|error| error.to_string())
    }

    /// Exact read offsets in previous/following order.
    pub const fn offsets(self) -> [Option<TimelineTime>; 2] {
        self.offsets
    }

    /// Required past and future extents of the prepared operation.
    pub fn temporal_extent(self) -> EffectTemporalInputExtent {
        self.offsets.into_iter().flatten().fold(
            EffectTemporalInputExtent::CURRENT_FRAME,
            |extent, offset| {
                extent.merge(crate::graph::temporal_input_extent_for_sample_offset(
                    offset,
                ))
            },
        )
    }

    /// Pack the two uniform cells for a three-texture draw.
    pub fn gpu_controls(self) -> [[f32; 4]; 2] {
        let [r, g, b] = self.sigma;
        let [previous, following] =
            self.offsets.map(|offset| f32::from(u8::from(offset.is_some())));
        [
            [r, g, b, self.mix],
            [self.motion_scale(), previous, following, 0.0],
        ]
    }

    pub(crate) fn hash_controls<H: std::hash::Hasher>(self, hasher: &mut H) {
        use std::hash::Hash;
        self.offsets.hash(hasher);
        self.sigma.map(f32::to_bits).hash(hasher);
        self.mix.to_bits().hash(hasher);
        self.motion_sensitivity.to_bits().hash(hasher);
        if let Some(request) = self.calibration {
            request.hash(hasher);
        }
    }

    fn motion_scale(&self) -> f32 {
        3.0 + 3.0 * (1.0 - self.motion_sensitivity)
    }

    fn apply_pixel(&self, center: [f32; 4], neighbors: [Option<[f32; 4]>; 2]) -> [f32; 4] {
        if self.mix == 0.0 || center[3] <= 0.0 || !center.into_iter().all(f32::is_finite) {
            return center;
        }

        let mut correction = [0.0; 3];
        let mut total_weight = 0.0;
        for tap in neighbors.into_iter().flatten() {
            if tap[3] <= 0.0 || !tap.into_iter().all(f32::is_finite) {
                continue;
            }
            let mut delta = [0.0; 3];
            let mut distance = 0.0_f32;
            for channel in 0..3 {
                delta[channel] = tap[channel] - center[channel];
                let noise_floor = 0.0001 * center[channel].abs().max(tap[channel].abs()).max(1.0);
                distance =
                    distance.max(delta[channel].abs() / self.sigma[channel].max(noise_floor));
            }
            let t = (distance / self.motion_scale() - 1.0).clamp(0.0, 1.0);
            let weight = self.mix * tap[3].min(1.0) * (1.0 - t * t * (3.0 - 2.0 * t));
            if weight > 0.0 {
                for (sum, delta) in correction.iter_mut().zip(delta) {
                    *sum += weight * delta;
                }
                total_weight += weight;
            }
        }
        let divisor = 1.0 + total_weight;
        if divisor == 1.0 {
            return center;
        }
        let mut filtered = center;
        for channel in 0..3 {
            let value = center[channel] + correction[channel] / divisor;
            if value.is_finite() {
                filtered[channel] = value;
            }
        }
        filtered
    }

    pub(crate) fn apply_controlled<E>(
        self,
        current: &mut [[f32; 4]],
        neighbors: [Option<&[[f32; 4]]>; 2],
        checkpoint: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<(), E> {
        for (i, pixel) in current.iter_mut().enumerate() {
            if i.is_multiple_of(4096) {
                checkpoint()?;
            }
            *pixel = self.apply_pixel(*pixel, neighbors.map(|frame| frame.map(|frame| frame[i])));
        }
        checkpoint()
    }
}
