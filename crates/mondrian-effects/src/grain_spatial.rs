//! Calibrated, edge-gated spatial grain reduction over straight Float32 RGB.

use crate::grain_calibration::GrainCalibrationRequest;

const DETAIL_GAIN: f32 = 0.800_390_54; // sqrt(41) / 8 for the default 3×3 binomial.
const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

/// Spatial shaping controls, independent of the source-frame statistics.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GrainSpatialShaping {
    /// Texture retention, zero to one.
    pub texture: f32,
    /// Grain size bias, negative one to one.
    pub size_bias: f32,
    /// Strength boost within the calibrated noise band, zero to one.
    pub solid_cleanup: f32,
    /// Chroma attenuation relative to luma, zero to ten. One is neutral.
    pub chroma: f32,
}

impl Default for GrainSpatialShaping {
    fn default() -> Self {
        Self {
            texture: 0.1,
            size_bias: 0.0,
            solid_cleanup: 0.0,
            chroma: 1.0,
        }
    }
}

/// Frozen calibrations and coefficients shared by both spatial backends.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GrainSpatialGrade {
    controls: [[f32; 4]; 3],
    calibration: Option<GrainCalibrationRequest>,
}

/// Invalid calibrated or authored controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("grain spatial requires finite sigma, amounts in [0, 100], and bounded shaping controls")]
pub struct GrainSpatialError;

impl GrainSpatialGrade {
    /// Freeze the three RGB noise deviations and per-channel amounts.
    ///
    /// The precomputed cells represent sigma and radius mix, detail threshold
    /// and center weight, and attenuation strength and chroma multiplier.
    pub fn new(
        sigma: [f32; 3],
        amount: [f32; 3],
        shaping: GrainSpatialShaping,
    ) -> Result<Self, GrainSpatialError> {
        if sigma.iter().any(|value| !value.is_finite() || *value < 0.0)
            || amount.iter().any(|value| !(0.0..=100.0).contains(value))
            || !(0.0..=1.0).contains(&shaping.texture)
            || !(-1.0..=1.0).contains(&shaping.size_bias)
            || !(0.0..=1.0).contains(&shaping.solid_cleanup)
            || !(0.0..=10.0).contains(&shaping.chroma)
        {
            return Err(GrainSpatialError);
        }
        let mut controls = [[0.0; 4]; 3];
        for channel in 0..3 {
            controls[0][channel] = sigma[channel];
            let texture_gain = 1.0 - (shaping.texture - 0.1) * 0.5;
            controls[1][channel] = (1.75 + 0.8 * (1.0 + amount[channel]).log2())
                * DETAIL_GAIN
                * texture_gain
                * sigma[channel];
            if amount[channel] > 0.0 && controls[1][channel] > 0.0 {
                let amount = amount[channel] / (amount[channel] + 0.5);
                controls[2][channel] = amount + (1.0 - amount) * shaping.solid_cleanup;
            }
        }
        controls[0][3] = shaping.size_bias.max(0.0);
        controls[1][3] = 2.0 * (-shaping.size_bias.min(0.0)).exp2();
        controls[2][3] = (2.0 * shaping.chroma) / (1.0 + shaping.chroma);
        if controls.iter().flatten().any(|value| !value.is_finite()) {
            return Err(GrainSpatialError);
        }
        Ok(Self { controls, calibration: None })
    }

    /// Declare host calibration while retaining the maximum halo and shaping.
    pub fn awaiting_source_calibration(
        calibration: GrainCalibrationRequest,
        amount: [f32; 3],
        shaping: GrainSpatialShaping,
    ) -> Result<Self, GrainSpatialError> {
        let mut grade = Self::new([1.0; 3], amount, shaping)?;
        grade.calibration = Some(calibration);
        Ok(grade)
    }

    pub(crate) const fn calibration(self) -> Option<GrainCalibrationRequest> {
        self.calibration
    }

    pub(crate) fn bind_calibration(mut self, sigma: [f32; 3]) -> Result<Self, GrainSpatialError> {
        for (channel, sigma) in sigma.into_iter().enumerate() {
            let threshold = self.controls[1][channel] * sigma;
            if sigma < 0.0 || !threshold.is_finite() {
                return Err(GrainSpatialError);
            }
            self.controls[0][channel] = sigma;
            self.controls[1][channel] = threshold;
            if threshold == 0.0 {
                self.controls[2][channel] = 0.0;
            }
        }
        self.calibration = None;
        Ok(self)
    }

    /// Shared three-cell uniform payload.
    pub const fn gpu_controls(self) -> [[f32; 4]; 3] {
        self.controls
    }

    /// Maximum tap offset in decoded pixels.
    pub const fn halo(self) -> u32 {
        if self.is_identity() {
            0
        } else if self.controls[0][3] > 0.0 {
            2
        } else {
            1
        }
    }

    /// Zero strength or zero calibrated deviation on every channel.
    pub const fn is_identity(self) -> bool {
        let [r, g, b, _] = self.controls[2];
        r == 0.0 && g == 0.0 && b == 0.0
    }

    pub(crate) fn hash_controls<H: std::hash::Hasher>(self, state: &mut H) {
        use std::hash::Hash;
        self.controls.map(|cell| cell.map(f32::to_bits)).hash(state);
        if let Some(request) = self.calibration {
            request.hash(state);
        }
    }

    fn filter_pixel(self, center: [f32; 4], sample: impl Fn(isize, isize) -> [f32; 4]) -> [f32; 4] {
        if center[3] <= 0.0 || !center.into_iter().all(f32::is_finite) {
            return center;
        }
        let [sigma, threshold, strength] = self.controls;
        let radius_mix = sigma[3];
        let mut detail = [0.0; 3];
        for (step, fraction) in [(1, 1.0 - radius_mix), (2, radius_mix)] {
            if fraction > 0.0 {
                let band = self.residual(center, &sample, step);
                for (detail, band) in detail.iter_mut().zip(band) {
                    *detail += band * fraction;
                }
            }
        }
        let correction: [f32; 3] = std::array::from_fn(|channel| {
            if strength[channel] == 0.0 || !detail[channel].is_finite() {
                return 0.0;
            }
            detail[channel]
                * strength[channel]
                * (1.0 - cubic_fade(detail[channel].abs() / threshold[channel] - 1.0))
        });
        let luma = (correction[0] * LUMA[0] + correction[1] * LUMA[1]) + correction[2] * LUMA[2];
        let mut result = center;
        for (index, channel) in result[..3].iter_mut().enumerate() {
            if strength[index] > 0.0 {
                let color_correction = if strength[3] == 1.0 {
                    correction[index]
                } else {
                    luma + (correction[index] - luma) * strength[3]
                };
                let value = *channel - color_correction;
                if value.is_finite() {
                    *channel = value;
                }
            }
        }
        result
    }

    fn residual(
        self,
        center: [f32; 4],
        sample: &impl Fn(isize, isize) -> [f32; 4],
        step: isize,
    ) -> [f32; 3] {
        let weights = [(-step, 1.0), (0, self.controls[1][3]), (step, 1.0)];
        let mut weighted_delta = [0.0; 3];
        let mut total = 0.0;
        for (y, wy) in weights {
            for (x, wx) in weights {
                let tap = sample(x, y);
                if tap[3] <= 0.0 || !tap.into_iter().all(f32::is_finite) {
                    continue;
                }
                let delta: [f32; 3] = std::array::from_fn(|c| center[c] - tap[c]);
                let distance = delta.iter().enumerate().fold(0.0_f32, |maximum, (c, delta)| {
                    let floor = 0.0001 * center[c].abs().max(tap[c].abs()).max(1.0);
                    maximum.max(delta.abs() / self.controls[0][c].max(floor))
                });
                let weight = wy * wx * tap[3].min(1.0) * (1.0 - cubic_fade((distance - 3.5) / 3.5));
                if weight > 0.0 {
                    total += weight;
                    for (sum, value) in weighted_delta.iter_mut().zip(delta) {
                        *sum += value * weight;
                    }
                }
            }
        }
        weighted_delta.map(|sum| if total > 0.0 { sum / total } else { 0.0 })
    }
}

fn cubic_fade(value: f32) -> f32 {
    let t = value.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Filter one tightly packed scene-linear straight RGBA Float32 raster.
/// Returns false if the storage and dimensions disagree or calibration is pending.
pub fn grain_spatial_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: GrainSpatialGrade,
) -> bool {
    if grade.calibration().is_some() || width.checked_mul(height) != Some(pixels.len()) {
        return false;
    }
    let Ok(()) = apply_grain_spatial_controlled(pixels, [width, height], grade, &mut || {
        Ok::<_, std::convert::Infallible>(())
    });
    true
}

pub(crate) fn apply_grain_spatial_controlled<E>(
    pixels: &mut [[f32; 4]],
    [width, height]: [usize; 2],
    grade: GrainSpatialGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), E> {
    checkpoint()?;
    if grade.is_identity() || width == 0 || height == 0 {
        return Ok(());
    }
    debug_assert_eq!(width.checked_mul(height), Some(pixels.len()));
    let mut output = Vec::with_capacity(pixels.len());
    for y in 0..height {
        for x in 0..width {
            if (y * width + x).is_multiple_of(4096) {
                checkpoint()?;
            }
            output.push(grade.filter_pixel(pixels[y * width + x], |dx, dy| {
                let sy = y.saturating_add_signed(dy).min(height - 1);
                let sx = x.saturating_add_signed(dx).min(width - 1);
                pixels[sy * width + sx]
            }));
        }
    }
    checkpoint()?;
    pixels.copy_from_slice(&output);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spatial_grain_reduces_noise_without_softening_an_hdr_step_or_alpha() {
        let (width, height) = (96usize, 64usize);
        let sigma = [0.012, 0.031, 0.017];
        let level = |x| {
            if x < width / 2 {
                [-0.35, 0.2, 0.08]
            } else {
                [1.6, 0.85, 0.42]
            }
        };
        let mut seed = 20261124_u64;
        let input: Vec<_> = (0..width * height)
            .map(|i| {
                let x = i % width;
                if x == 3 {
                    return [90.0, -64.0, 20.0, 0.0];
                }
                let levels = level(x);
                let noise = std::array::from_fn::<_, 3, _>(|channel| {
                    seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                    let uniform = (seed >> 40) as f32 / 16_777_216.0;
                    levels[channel] + (uniform - 0.5) * 3.464_101_6 * sigma[channel]
                });
                [
                    noise[0],
                    noise[1],
                    noise[2],
                    if i.is_multiple_of(7) { 0.56 } else { 1.0 },
                ]
            })
            .collect();
        let grade =
            GrainSpatialGrade::new(sigma, [1.0, 1.0, 0.0], GrainSpatialShaping::default()).unwrap();
        let mut output = input.clone();
        assert!(grain_spatial_frame_f32(&mut output, width, height, grade));
        for (source, filtered) in input.iter().zip(&output) {
            assert_eq!(source[3].to_bits(), filtered[3].to_bits());
            assert_eq!(source[2].to_bits(), filtered[2].to_bits());
            if source[3] == 0.0 {
                assert_eq!(source, filtered);
            }
        }
        let variance = |pixels: &[[f32; 4]], channel: usize| {
            pixels
                .iter()
                .enumerate()
                .filter(|(i, p)| p[3] > 0.0 && (i % width).abs_diff(width / 2) > 3)
                .map(|(i, p)| (p[channel] - level(i % width)[channel]).powi(2))
                .sum::<f32>()
        };
        for channel in 0..2 {
            let ratio = (variance(&output, channel) / variance(&input, channel)).sqrt();
            let edge = |pixels: &[[f32; 4]]| {
                pixels
                    .chunks_exact(width)
                    .map(|row| row[width / 2][channel] - row[width / 2 - 1][channel])
                    .sum::<f32>()
                    / height as f32
            };
            eprintln!(
                "Spatial grain channel {channel}: RMS ratio {ratio:.5}, edge delta {:.6}",
                edge(&output) - edge(&input)
            );
            assert!(ratio < 0.68);
            assert!((edge(&output) - edge(&input)).abs() < 0.008);
        }
    }

    #[test]
    fn spatial_grain_shaping_changes_the_support_and_preserves_the_disabled_channels() {
        let input = [
            [0.32, 0.30, 0.30, 1.0],
            [0.30, 0.30, 0.30, 1.0],
            [0.35, 0.30, 0.30, 1.0],
            [0.30, 0.30, 0.30, 1.0],
            [0.22, 0.30, 0.30, 1.0],
        ];
        let run = |shaping, amount| {
            let grade = GrainSpatialGrade::new([0.04; 3], amount, shaping).unwrap();
            let mut filtered = input;
            assert!(grain_spatial_frame_f32(&mut filtered, 5, 1, grade));
            (grade.halo(), filtered)
        };
        let (halo, narrow) = run(GrainSpatialShaping::default(), [1.0, 0.0, 0.0]);
        let (_, broad) = run(
            GrainSpatialShaping { size_bias: 1.0, ..Default::default() },
            [1.0, 0.0, 0.0],
        );
        let (wide, _) = run(
            GrainSpatialShaping { size_bias: 0.5, ..Default::default() },
            [1.0, 0.0, 0.0],
        );
        let (_, cleanup) = run(
            GrainSpatialShaping { solid_cleanup: 1.0, ..Default::default() },
            [1.0, 0.0, 0.0],
        );
        let (_, no_chroma) = run(
            GrainSpatialShaping { chroma: 0.0, ..Default::default() },
            [1.0, 0.0, 0.0],
        );
        assert_eq!((halo, wide), (1, 2));
        assert!((narrow[2][0] - broad[2][0]).abs() > 0.005);
        assert!(cleanup[2][0] < narrow[2][0]);
        assert!(no_chroma[2][0] > narrow[2][0]);
        assert_eq!(broad[2][1..], input[2][1..]);
        assert_eq!(run(GrainSpatialShaping::default(), [0.0; 3]), (0, input));
    }
}
