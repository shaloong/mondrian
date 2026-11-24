//! Bounded noise estimation from a single, already-decoded calibration raster.

use crate::{EffectColorDomain, EffectFloatFrameView, EffectWorkingPrecision};
use mondrian_core::{ColorFrameAlpha, ExecutionCancellationToken};

const MAX_AXIS_SAMPLES: usize = 16;
const MIN_VALID_BLOCKS: usize = 16;
const MAX_PHASE_COHERENCE: f64 = 0.55;
const NORMAL_MEDIAN_ABS: f64 = 0.674_489_750_196_081_7;

/// Window count and window edge, expressed on the decoded raster's grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GrainCalibrationSettings {
    edge: u16,
    count: u8,
}

impl GrainCalibrationSettings {
    /// Eight windows, each up to 24 pixels on either axis.
    pub const DEFAULT: Self = Self { edge: 24, count: 8 };

    /// Validate the authored window edge (16–256) and count (1–10).
    pub fn new(edge: u16, count: u8) -> Result<Self, GrainCalibrationError> {
        if !(16..=256).contains(&edge) || !(1..=10).contains(&count) {
            return Err(GrainCalibrationError::InvalidWindows);
        }
        Ok(Self { edge, count })
    }
}

/// Exact frame on the original media grid and authored statistics settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GrainCalibrationRequest {
    frame_index: u32,
    settings: GrainCalibrationSettings,
}

impl GrainCalibrationRequest {
    /// Create a fixed source request. The authoring contract admits frames 0–100,000.
    pub const fn new(frame_index: u32, settings: GrainCalibrationSettings) -> Option<Self> {
        if frame_index <= 100_000 {
            Some(Self { frame_index, settings })
        } else {
            None
        }
    }

    /// Zero-based frame on the source grid.
    pub const fn frame_index(self) -> u32 {
        self.frame_index
    }

    /// Analysis window settings for this request.
    pub const fn settings(self) -> GrainCalibrationSettings {
        self.settings
    }
}

/// A chosen half-open statistics window in source-raster pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GrainCalibrationWindow {
    /// Upper left pixel.
    pub origin: [u32; 2],
    /// Number of columns and rows.
    pub size: [u32; 2],
}

/// Frozen per-channel noise statistics and the evidence used to estimate them.
#[derive(Debug, Clone, PartialEq)]
pub struct GrainNoiseProfile {
    /// Standard deviations in scene-linear RGB.
    pub sigma: [f32; 3],
    /// Accepted windows in rank order, with deterministic spatial ties.
    pub windows: Box<[GrainCalibrationWindow]>,
    /// Number of usable non-overlapping 2×2 blocks across those windows.
    pub valid_blocks: usize,
}

/// Invalid input or an interrupted calibration attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GrainCalibrationError {
    /// At least one authored window setting was outside the admitted interval.
    #[error("grain calibration requires 1–10 windows with edges of 16–256 pixels")]
    InvalidWindows,
    /// The source is not a straight-coverage scene-linear Float32 raster.
    #[error("grain calibration requires straight-coverage or opaque scene-linear Float32")]
    UnsupportedFormat,
    /// Pixel storage differs from the declared extent.
    #[error("grain calibration raster extent does not match its storage")]
    InvalidRaster,
    /// The owning request stopped during estimation.
    #[error("grain calibration canceled")]
    Canceled,
}

/// Estimate noise on the actual decoded grid, without copying the frame.
///
/// At most 256 windows are ranked using 256 blocks each. Up to ten finalists
/// are revisited, for at most 68,096 block visits and 7,680 retained coefficients.
/// No usable flat, covered sample yields `Ok(None)`. A measured zero-noise
/// profile is still a valid profile. Callers can freeze either result.
pub fn calibrate_grain(
    frame: EffectFloatFrameView<'_>,
    settings: GrainCalibrationSettings,
    cancellation: &ExecutionCancellationToken,
) -> Result<Option<GrainNoiseProfile>, GrainCalibrationError> {
    let checkpoint = || {
        if cancellation.is_canceled() {
            Err(GrainCalibrationError::Canceled)
        } else {
            Ok(())
        }
    };
    checkpoint()?;
    let format = frame.format();
    if format.precision() != EffectWorkingPrecision::Float32
        || format.domain() != EffectColorDomain::SceneLinearRgb
        || !matches!(
            format.alpha(),
            ColorFrameAlpha::StraightCoverage | ColorFrameAlpha::Opaque
        )
    {
        return Err(GrainCalibrationError::UnsupportedFormat);
    }
    let dimensions = [frame.extent().width(), frame.extent().height()];
    let [width, height] = dimensions.map(|size| size as usize);
    let pixels = frame.pixels();
    if width == 0 || height == 0 || width.checked_mul(height) != Some(pixels.len()) {
        return Err(GrainCalibrationError::InvalidRaster);
    }
    let [columns, rows] = dimensions.map(|size| CalibrationAxis::new(size, settings.edge));
    let samples_per_window = columns.blocks.len() * rows.blocks.len();
    if samples_per_window < MIN_VALID_BLOCKS {
        return Ok(None);
    }

    let read_bands = |origin: [u32; 2], visit: &mut dyn FnMut([f64; 9])| {
        for &dy in &rows.blocks {
            let start = (origin[1] + dy) as usize * width + origin[0] as usize;
            for &dx in &columns.blocks {
                let i = start + dx as usize;
                let block = [
                    pixels[i],
                    pixels[i + 1],
                    pixels[i + width],
                    pixels[i + width + 1],
                ];
                if let Some(bands) = block_bands(block, format.alpha()) {
                    visit(bands);
                }
            }
        }
    };
    let mut candidates = Vec::with_capacity(columns.origins.len() * rows.origins.len());
    let mut energies = Vec::with_capacity(samples_per_window);
    for &y in &rows.origins {
        for &x in &columns.origins {
            checkpoint()?;
            let origin = [x, y];
            let mut moments = [BandMoments::default(); 9];
            energies.clear();
            read_bands(origin, &mut |bands| {
                let mut energy = 0.0_f64;
                for (moment, coefficient) in moments.iter_mut().zip(bands) {
                    moment.add(coefficient);
                    energy = energy.max(coefficient.abs());
                }
                energies.push(energy);
            });
            let valid = energies.len();
            if valid * 4 < samples_per_window * 3
                || moments.iter().any(|moment| moment.is_coherent(valid))
            {
                continue;
            }
            let (_, quartile, _) = energies.select_nth_unstable_by(valid * 3 / 4, f64::total_cmp);
            candidates.push((*quartile, origin));
        }
    }
    candidates.sort_unstable_by(|(score_a, [ax, ay]), (score_b, [bx, by])| {
        score_a.total_cmp(score_b).then_with(|| (ay, ax).cmp(&(by, bx)))
    });
    candidates.truncate(usize::from(settings.count));

    let mut residuals = std::array::from_fn::<_, 3, _>(|_| {
        Vec::with_capacity(candidates.len() * samples_per_window)
    });
    let mut windows = Vec::with_capacity(candidates.len());
    for (_, origin) in candidates {
        checkpoint()?;
        windows.push(GrainCalibrationWindow { origin, size: [columns.edge, rows.edge] });
        read_bands(origin, &mut |bands| {
            for (values, coefficient) in residuals.iter_mut().zip(bands[6..].iter()) {
                values.push(coefficient.abs());
            }
        });
    }
    let valid_blocks = residuals[0].len();
    if valid_blocks < MIN_VALID_BLOCKS {
        return Ok(None);
    }
    let sigma = residuals.map(|mut values| {
        let (lower, midpoint, _) = values.select_nth_unstable_by(valid_blocks / 2, f64::total_cmp);
        let median = if valid_blocks.is_multiple_of(2) {
            let lower = lower.iter().copied().fold(0.0, f64::max);
            (lower + *midpoint) * 0.5
        } else {
            *midpoint
        };
        (median / NORMAL_MEDIAN_ABS) as f32
    });
    checkpoint()?;
    Ok(
        sigma.into_iter().all(f32::is_finite).then(|| GrainNoiseProfile {
            sigma,
            windows: windows.into_boxed_slice(),
            valid_blocks,
        }),
    )
}

struct CalibrationAxis {
    edge: u32,
    origins: Vec<u32>,
    blocks: Vec<u32>,
}

impl CalibrationAxis {
    fn new(size: u32, requested_edge: u16) -> Self {
        let edge = size.min(u32::from(requested_edge));
        let windows = (size / edge).min(MAX_AXIS_SAMPLES as u32);
        let origins = if windows == 1 {
            vec![(size - edge) / 2]
        } else {
            (0..windows)
                .map(|i| (u64::from(i) * u64::from(size - edge) / u64::from(windows - 1)) as u32)
                .collect()
        };
        let block_count = edge / 2;
        let samples = block_count.min(MAX_AXIS_SAMPLES as u32);
        let blocks =
            (0..samples).map(|i| 2 * ((2 * i + 1) * block_count / (2 * samples))).collect();
        Self { edge, origins, blocks }
    }
}

#[derive(Clone, Copy, Default)]
struct BandMoments {
    sum: f64,
    squares: f64,
}

impl BandMoments {
    fn add(&mut self, value: f64) {
        self.sum += value;
        self.squares += value * value;
    }

    fn is_coherent(self, count: usize) -> bool {
        self.sum * self.sum > MAX_PHASE_COHERENCE.powi(2) * count as f64 * self.squares
    }
}

fn block_bands(pixels: [[f32; 4]; 4], alpha: ColorFrameAlpha) -> Option<[f64; 9]> {
    if !pixels.iter().all(|pixel| {
        pixel[3] >= 0.5 && alpha.admits_alpha(pixel[3]) && pixel[..3].iter().all(|v| v.is_finite())
    }) {
        return None;
    }
    let mut bands = [0.0; 9];
    for channel in 0..3 {
        let [a, b, c, d] = pixels.map(|pixel| f64::from(pixel[channel]));
        let top = a - b;
        let bottom = c - d;
        bands[channel] = (top + bottom) * 0.5;
        bands[3 + channel] = ((a - c) + (b - d)) * 0.5;
        bands[6 + channel] = (top - bottom) * 0.5;
    }
    Some(bands)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EffectFrameExtent, EffectValueFormat};
    use mondrian_core::ImageSampling;

    fn view(pixels: &[[f32; 4]], width: u32, height: u32) -> EffectFloatFrameView<'_> {
        EffectFloatFrameView::new(
            EffectFrameExtent::new(width, height),
            EffectValueFormat::new(
                EffectWorkingPrecision::Float32,
                EffectColorDomain::SceneLinearRgb,
                ColorFrameAlpha::StraightCoverage,
            ),
            ImageSampling::UNIT_SQUARE,
            pixels,
        )
    }

    #[test]
    fn rejects_structured_or_uncovered_blocks_and_finds_rgb_noise() {
        let mut seed = 320_001_u64;
        let mut uniform = || {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            ((seed >> 40) as f32 + 1.0) / 16_777_217.0
        };
        let mut normal =
            || (-2.0 * uniform().ln()).sqrt() * (std::f32::consts::TAU * uniform()).cos();
        let (width, height) = (256_u32, 192);
        let sigma = [0.013, 0.030, 0.018];
        let pixels: Vec<_> = (0..width * height)
            .map(|i| {
                let (x, y) = (i % width, i / width);
                if (128..192).contains(&x) {
                    return [99.0, -50.0, 10.0, 0.0];
                }
                let texture = if x < 128 {
                    ((x + y) % 2) as f32 * 0.5
                } else {
                    0.0
                };
                [
                    1.7 + texture + sigma[0] * normal(),
                    -0.24 + texture + sigma[1] * normal(),
                    0.62 + texture + sigma[2] * normal(),
                    0.8,
                ]
            })
            .collect();
        let settings = GrainCalibrationSettings::new(32, 8).unwrap();
        let profile = calibrate_grain(
            view(&pixels, width, height),
            settings,
            &ExecutionCancellationToken::new(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(profile.valid_blocks, 2048);
        assert!(profile.windows.iter().all(|window| window.origin[0] >= 192));
        for (measured, expected) in profile.sigma.into_iter().zip(sigma) {
            assert!((measured / expected - 1.0).abs() < 0.10);
        }
    }

    #[test]
    fn distinguishes_zero_noise_from_unusable_rasters_or_cancellation() {
        let token = ExecutionCancellationToken::new();
        let settings = GrainCalibrationSettings::DEFAULT;
        let mut pixels = vec![[f32::MAX, -f32::MAX, 0.0, 0.5]; 19 * 17];
        pixels[19 * 3 + 2][0] = f32::NAN;
        let flat = calibrate_grain(view(&pixels, 19, 17), settings, &token).unwrap().unwrap();
        assert_eq!(flat.sigma, [0.0; 3]);
        assert_eq!(flat.valid_blocks, 71);
        pixels.fill([99.0, -10.0, 0.0, 0.0]);
        assert_eq!(
            calibrate_grain(view(&pixels, 19, 17), settings, &token),
            Ok(None)
        );
        assert_eq!(
            calibrate_grain(view(&pixels, 17, 17), settings, &token),
            Err(GrainCalibrationError::InvalidRaster)
        );
        token.cancel();
        assert_eq!(
            calibrate_grain(view(&pixels, 19, 17), settings, &token),
            Err(GrainCalibrationError::Canceled)
        );
    }
}
