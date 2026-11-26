//! Bounded noise estimation from a single, already-decoded calibration raster.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use crate::{EffectColorDomain, EffectFloatFrameView, EffectFrameExtent, EffectWorkingPrecision};
use mondrian_core::{
    ColorFrameAlpha, ExecutionCancellationToken, ImageSampling, MediaFileFingerprint,
    SourceSampleTarget,
};

const MAX_AXIS_SAMPLES: usize = 16;
const MAX_WINDOWS: usize = 10;
const MIN_VALID_BLOCKS: usize = 16;
const MAX_PHASE_COHERENCE: f64 = 0.55;
const NORMAL_MEDIAN_ABS: f64 = 0.674_489_750_196_081_7;

/// Window count, edge, and optional manual centers on the decoded raster's grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GrainCalibrationSettings {
    edge: u16,
    count: u8,
    manual_centers: Option<[[u32; 2]; MAX_WINDOWS]>,
}

impl Hash for GrainCalibrationSettings {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.edge.hash(state);
        self.count.hash(state);
        if let Some(centers) = self.manual_centers() {
            centers.hash(state);
        }
    }
}

impl GrainCalibrationSettings {
    /// Eight windows, each up to 24 pixels on either axis.
    pub const DEFAULT: Self = Self { edge: 24, count: 8, manual_centers: None };

    /// Validate the authored window edge (16–256) and count (1–10).
    pub fn new(edge: u16, count: u8) -> Result<Self, GrainCalibrationError> {
        if !(16..=256).contains(&edge) || !(1..=MAX_WINDOWS).contains(&usize::from(count)) {
            return Err(GrainCalibrationError::InvalidWindows);
        }
        Ok(Self { edge, count, manual_centers: None })
    }

    /// Use the given source-pixel centers instead of searching the image.
    ///
    /// Each square is shifted inside the decoded extent. Repeated origins are
    /// counted once, in author order.
    pub fn manual(edge: u16, centers: &[[u32; 2]]) -> Result<Self, GrainCalibrationError> {
        let count =
            u8::try_from(centers.len()).map_err(|_| GrainCalibrationError::InvalidWindows)?;
        let mut settings = Self::new(edge, count)?;
        let mut stored = [[0; 2]; MAX_WINDOWS];
        stored[..centers.len()].copy_from_slice(centers);
        settings.manual_centers = Some(stored);
        Ok(settings)
    }

    fn manual_centers(&self) -> Option<&[[u32; 2]]> {
        self.manual_centers.as_ref().map(|centers| &centers[..usize::from(self.count)])
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

/// Source evidence for the admitted calibration picture. Contains no pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GrainSamplePicture {
    /// Exact original-stream sample, independent of playback and Clip retiming.
    pub target: SourceSampleTarget,
    /// Physical revision of the file used to measure the windows.
    pub fingerprint: MediaFileFingerprint,
    /// Index of the measured video stream.
    pub video_stream_index: u32,
    /// Native decoded pixel extent.
    pub extent: EffectFrameExtent,
    /// Native decoded pixel shape and sampling rates.
    pub sampling: ImageSampling,
}

/// A measured source picture and its statistics, if any windows were usable.
#[derive(Debug, Clone)]
pub struct GrainCalibrationObservation {
    /// Evidence for retrieving the same picture for display.
    pub picture: GrainSamplePicture,
    /// Usable measurements. `None` still proves a picture was admitted.
    pub profile: Option<Arc<GrainNoiseProfile>>,
}

/// A fixed-source observation before or after native media admission.
#[derive(Debug, Clone, PartialEq)]
pub enum GrainSampleBoxPlacement {
    /// Author controls awaiting the source and its accepted windows.
    Pending {
        /// Exact authored source frame and sample settings.
        request: GrainCalibrationRequest,
        /// Straight scene-linear RGBA outline color.
        color: [f32; 4],
    },
    /// Overlay geometry ready to execute.
    Bound {
        /// Overlay on the actual admitted raster.
        boxes: Arc<GrainSampleBoxes>,
        /// Exact evidence for the host-owned raster.
        picture: GrainSamplePicture,
    },
}

impl GrainSampleBoxPlacement {
    pub(crate) const fn request(&self) -> Option<GrainCalibrationRequest> {
        match self {
            Self::Pending { request, .. } => Some(*request),
            Self::Bound { .. } => None,
        }
    }

    pub(crate) const fn boxes(&self) -> Option<&Arc<GrainSampleBoxes>> {
        match self {
            Self::Pending { .. } => None,
            Self::Bound { boxes, .. } => Some(boxes),
        }
    }

    pub(crate) fn hash_controls(&self, state: &mut impl Hasher) {
        match self {
            Self::Pending { request, color } => {
                0u8.hash(state);
                request.hash(state);
                color.map(f32::to_bits).hash(state);
            }
            Self::Bound { boxes, picture } => {
                1u8.hash(state);
                boxes.hash_controls(state);
                picture.hash(state);
            }
        }
    }
}

/// Draw the windows accepted by calibration on the fixed source's actual raster.
/// Holds only overlay geometry and never owns the source picture.
#[derive(Debug, Clone, PartialEq)]
pub struct GrainSampleBoxes {
    extent: crate::EffectFrameExtent,
    windows: Box<[GrainCalibrationWindow]>,
    color: [f32; 4],
}

impl GrainSampleBoxes {
    /// Reject windows which do not belong to the accompanying decoded grid.
    pub fn new(
        extent: crate::EffectFrameExtent,
        windows: &[GrainCalibrationWindow],
        color: [f32; 4],
    ) -> Option<Self> {
        if extent.width() == 0
            || extent.height() == 0
            || windows.len() > MAX_WINDOWS
            || !color.into_iter().all(f32::is_finite)
            || !(0.0..=1.0).contains(&color[3])
        {
            return None;
        }
        let valid = windows.iter().all(|window| {
            !window.size.contains(&0)
                && u64::from(window.origin[0]) + u64::from(window.size[0])
                    <= u64::from(extent.width())
                && u64::from(window.origin[1]) + u64::from(window.size[1])
                    <= u64::from(extent.height())
        });
        valid.then(|| Self { extent, windows: windows.into(), color })
    }

    /// Exact decoded geometry used for the measurements and these windows.
    pub const fn extent(&self) -> crate::EffectFrameExtent {
        self.extent
    }

    pub(crate) fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>() + std::mem::size_of_val(self.windows.as_ref())
    }

    /// One mode cell, one straight RGB color, and ten half-open pixel rectangles.
    pub fn gpu_controls(&self) -> [[f32; 4]; 12] {
        let mut cells = [[0.0; 4]; 12];
        cells[0] = [self.windows.len() as f32, 0.0, 0.0, 0.0];
        cells[1] = self.color;
        for (cell, window) in cells[2..].iter_mut().zip(&self.windows) {
            let [x, y] = window.origin;
            let [width, height] = window.size;
            *cell = [x as f32, y as f32, (x + width) as f32, (y + height) as f32];
        }
        cells
    }

    pub(crate) fn hash_controls(&self, state: &mut impl Hasher) {
        self.extent.hash(state);
        self.windows.hash(state);
        self.color.map(f32::to_bits).hash(state);
    }

    pub(crate) fn apply<E>(
        &self,
        pixels: &mut [[f32; 4]],
        region: crate::EffectPixelRoi,
        checkpoint: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<(), E> {
        let opacity = self.color[3];
        if opacity == 0.0 || self.windows.is_empty() || region.width() == 0 {
            return checkpoint();
        }
        for (dy, row) in pixels.chunks_mut(region.width() as usize).enumerate() {
            checkpoint()?;
            let y = region.y() + dy as u32;
            for (dx, pixel) in row.iter_mut().enumerate() {
                let x = region.x() + dx as u32;
                let on_border = self.windows.iter().any(|window| {
                    let [left, top] = window.origin;
                    let right = left + window.size[0];
                    let bottom = top + window.size[1];
                    (left..right).contains(&x)
                        && (top..bottom).contains(&y)
                        && (x == left || x == right - 1 || y == top || y == bottom - 1)
                });
                if on_border {
                    for (channel, color) in pixel[..3].iter_mut().zip(self.color) {
                        *channel = if opacity == 1.0 {
                            color
                        } else {
                            *channel * (1.0 - opacity) + color * opacity
                        };
                    }
                }
            }
        }
        checkpoint()
    }
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
    let manual_centers = settings.manual_centers();
    let mut manual_origins = Vec::new();
    if let Some(centers) = manual_centers {
        manual_origins.reserve_exact(centers.len());
        let window = [columns.edge, rows.edge];
        for &center in centers {
            let origin = std::array::from_fn(|i| {
                center[i].saturating_sub(window[i] / 2).min(dimensions[i] - window[i])
            });
            if !manual_origins.contains(&origin) {
                manual_origins.push(origin);
            }
        }
    }
    let automatic_rows = if manual_centers.is_none() {
        rows.origins.as_slice()
    } else {
        &[]
    };
    let automatic_origins = automatic_rows
        .iter()
        .flat_map(|&y| columns.origins.iter().map(move |&x| [x, y]));
    let mut candidates =
        Vec::with_capacity(manual_origins.len() + columns.origins.len() * automatic_rows.len());
    let mut energies = Vec::with_capacity(samples_per_window);
    for origin in manual_origins.into_iter().chain(automatic_origins) {
        checkpoint()?;
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
        let score = if manual_centers.is_none() {
            let (_, quartile, _) = energies.select_nth_unstable_by(valid * 3 / 4, f64::total_cmp);
            *quartile
        } else {
            0.0
        };
        candidates.push((score, origin));
    }
    if manual_centers.is_none() {
        candidates.sort_unstable_by(|(score_a, [ax, ay]), (score_b, [bx, by])| {
            score_a.total_cmp(score_b).then_with(|| (ay, ax).cmp(&(by, bx)))
        });
        candidates.truncate(usize::from(settings.count));
    }

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
    fn sample_boxes_draw_an_inset_border_once_and_keep_the_fixed_raster_alpha() {
        use crate::{EffectFrameExtent, EffectPixelRoi};

        let extent = EffectFrameExtent::new(6, 5);
        let windows = [
            GrainCalibrationWindow { origin: [1, 1], size: [4, 3] },
            GrainCalibrationWindow { origin: [2, 1], size: [3, 3] },
        ];
        let boxes = GrainSampleBoxes::new(extent, &windows, [0.5, 1.0, 0.0, 0.5]).unwrap();
        assert!(GrainSampleBoxes::new(EffectFrameExtent::new(4, 5), &windows, [1.0; 4]).is_none());
        let source = [-0.25, 0.5, 2.0, 0.625];
        let ink = [0.125, 0.75, 1.0, 0.625];
        let mut pixels = [source; 30];
        let mut checkpoint = || Ok::<_, std::convert::Infallible>(());
        boxes.apply(&mut pixels, extent.full_frame_roi(), &mut checkpoint).unwrap();
        assert_eq!(pixels[0], source);
        assert_eq!(pixels[8], ink);
        assert_eq!(pixels[15], source);
        assert_eq!(pixels[16], ink);
        let mut partial = [source; 6];
        boxes
            .apply(
                &mut partial,
                EffectPixelRoi::new(2, 1, 3, 2),
                &mut checkpoint,
            )
            .unwrap();
        assert_eq!(partial[..3], [ink; 3]);
        assert_eq!(partial[3..], [ink, source, ink]);
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

    #[test]
    fn manual_calibration_preserves_window_order_and_rejects_uncovered_samples() {
        let (width, height) = (64, 32);
        let samples: Vec<_> = (0..width * height)
            .map(|i| {
                let alpha = if (30..46).contains(&(i % width)) {
                    0.0
                } else {
                    1.0
                };
                [0.125, 0.25, 0.5, alpha]
            })
            .collect();
        let frame = view(&samples, width, height);
        let cancel = ExecutionCancellationToken::new();
        let settings = GrainCalibrationSettings::manual(
            16,
            &[[60, 30], [u32::MAX, u32::MAX], [12, 17], [38, 0]],
        )
        .unwrap();
        let measured = calibrate_grain(frame, settings, &cancel).unwrap().unwrap();
        assert_eq!(measured.sigma, [0.0; 3]);
        assert_eq!(measured.valid_blocks, 128);
        assert_eq!(
            &*measured.windows,
            &[
                GrainCalibrationWindow { origin: [48, 16], size: [16, 16] },
                GrainCalibrationWindow { origin: [4, 9], size: [16, 16] },
            ],
        );
        let uncovered = GrainCalibrationSettings::manual(16, &[[38, 0]]).unwrap();
        assert_eq!(calibrate_grain(frame, uncovered, &cancel), Ok(None));
        assert_eq!(
            GrainCalibrationSettings::manual(16, &[]),
            Err(GrainCalibrationError::InvalidWindows),
        );
        assert_eq!(
            GrainCalibrationSettings::manual(16, &[[0, 0]; MAX_WINDOWS + 1]),
            Err(GrainCalibrationError::InvalidWindows),
        );
    }

    #[test]
    fn manual_centers_participate_in_calibration_request_identity() {
        use std::hash::{BuildHasher, RandomState};

        let make_request = |settings| GrainCalibrationRequest::new(0, settings).unwrap();
        let auto = make_request(GrainCalibrationSettings::new(16, 2).unwrap());
        let first =
            make_request(GrainCalibrationSettings::manual(16, &[[0, 0], [24, 40]]).unwrap());
        let second =
            make_request(GrainCalibrationSettings::manual(16, &[[0, 0], [25, 40]]).unwrap());
        let hashing = RandomState::new();
        assert_ne!(auto, first);
        assert_ne!(first, second);
        assert_ne!(hashing.hash_one(auto), hashing.hash_one(first));
        assert_ne!(hashing.hash_one(first), hashing.hash_one(second));
    }
}
