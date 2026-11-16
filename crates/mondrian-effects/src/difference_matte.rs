//! Difference matte (KEY-06): keys a flat-film plate against a reference frame.
//!
//! The authored controls are the auxiliary layer slot, the reference mapping
//! mode, the tolerance and softness thresholds, and a box-blur radius. The
//! matte compares premultiplied samples of the primary frame and the reference
//! frame, averages the per-channel difference horizontally then vertically, and
//! keeps the source color while scaling its coverage (`src.a * matte`).

use std::hash::{Hash, Hasher};

use crate::auxiliary::{sample_auxiliary_associated_f32, AuxiliaryFloatFrame};
use crate::graph::{AuxiliaryCoordinateMapping, AuxiliarySampling};

/// Largest blur radius the authoring UI accepts, in pixels.
pub const DIFFERENCE_MATTE_MAX_BLUR_PIXELS: f32 = 1000.0;
/// Smallest accepted tolerance, in percent.
pub const MIN_TOLERANCE: f32 = 0.0;
/// Largest accepted tolerance, in percent.
pub const MAX_TOLERANCE: f32 = 100.0;
/// Smallest accepted softness, in percent.
pub const MIN_SOFTNESS: f32 = 0.0;
/// Largest accepted softness, in percent.
pub const MAX_SOFTNESS: f32 = 100.0;
/// Lowest accepted auxiliary layer slot.
pub const MIN_LAYER: u32 = 1;
/// Highest accepted auxiliary layer slot.
pub const MAX_LAYER: u32 = 19;
/// Documented default auxiliary layer slot.
pub const DEFAULT_LAYER: u32 = 6;
/// Divide guard for the softness ramp; never applied to the hard threshold.
pub const EPSILON: f32 = 1e-6;
/// Documented default tolerance, in percent.
pub const DEFAULT_TOLERANCE: f32 = 15.0;
/// Checkpoint cadence of the CPU kernel, in pixels.
const CHECKPOINT_STRIDE: usize = 4_096;

/// How the reference frame is fitted onto the primary frame before sampling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DifferenceMatteSizeMode {
    /// Stretch the reference over the whole primary frame.
    Stretch,
    /// Preserve the reference aspect ratio and crop it to the primary frame.
    CenterCrop,
}

impl DifferenceMatteSizeMode {
    /// Every authored mode, in menu order.
    pub const ALL: [Self; 2] = [Self::Stretch, Self::CenterCrop];

    /// Stable menu code carried in the GPU uniform block.
    pub const fn code(self) -> u32 {
        match self {
            Self::Stretch => 1,
            Self::CenterCrop => 2,
        }
    }

    /// Inverse of [`Self::code`].
    pub const fn from_code(code: u32) -> Option<Self> {
        match code {
            1 => Some(Self::Stretch),
            2 => Some(Self::CenterCrop),
            _ => None,
        }
    }

    /// Stable schema key.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Stretch => "stretch",
            Self::CenterCrop => "center_crop",
        }
    }

    /// Inverse of [`Self::as_key`].
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "stretch" => Some(Self::Stretch),
            "center_crop" => Some(Self::CenterCrop),
            _ => None,
        }
    }

    /// Localized menu label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Stretch => "拉伸适配",
            Self::CenterCrop => "中心裁剪",
        }
    }
}

/// Why a set of authored difference matte controls was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DifferenceMatteError {
    /// The auxiliary layer slot left `1..=19`.
    #[error("difference matte layer must be within [1, 19]")]
    LayerOutOfRange,
    /// The tolerance left `0..=100` percent.
    #[error("difference matte tolerance must be within [0, 100]")]
    ToleranceOutOfRange,
    /// The softness left `0..=100` percent.
    #[error("difference matte softness must be within [0, 100]")]
    SoftnessOutOfRange,
    /// The blur radius left `0..=1000` pixels.
    #[error("difference matte blur must be within [0, 1000] pixels")]
    BlurOutOfRange,
}

impl DifferenceMatteError {
    /// Authored parameter that produced this rejection.
    pub const fn parameter(self) -> &'static str {
        match self {
            Self::LayerOutOfRange => "layer",
            Self::ToleranceOutOfRange => "tolerance",
            Self::SoftnessOutOfRange => "softness",
            Self::BlurOutOfRange => "blur",
        }
    }
}

/// Authored difference matte controls.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DifferenceMatteGrade {
    layer: u32,
    size_mode: DifferenceMatteSizeMode,
    tolerance: f32,
    softness: f32,
    blur: f32,
}

impl DifferenceMatteGrade {
    /// Validate authored controls.
    pub fn new(
        layer: u32,
        size_mode: DifferenceMatteSizeMode,
        tolerance: f32,
        softness: f32,
        blur: f32,
    ) -> Result<Self, DifferenceMatteError> {
        if !(MIN_LAYER..=MAX_LAYER).contains(&layer) {
            return Err(DifferenceMatteError::LayerOutOfRange);
        }
        if !(MIN_TOLERANCE..=MAX_TOLERANCE).contains(&tolerance) {
            return Err(DifferenceMatteError::ToleranceOutOfRange);
        }
        if !(MIN_SOFTNESS..=MAX_SOFTNESS).contains(&softness) {
            return Err(DifferenceMatteError::SoftnessOutOfRange);
        }
        if !(0.0..=DIFFERENCE_MATTE_MAX_BLUR_PIXELS).contains(&blur) {
            return Err(DifferenceMatteError::BlurOutOfRange);
        }
        Ok(Self { layer, size_mode, tolerance, softness, blur })
    }

    /// Controls a freshly authored effect starts with.
    pub const fn documented_defaults() -> Self {
        Self {
            layer: DEFAULT_LAYER,
            size_mode: DifferenceMatteSizeMode::Stretch,
            tolerance: DEFAULT_TOLERANCE,
            softness: 0.0,
            blur: 0.0,
        }
    }

    /// Auxiliary layer slot that supplies the reference frame.
    pub const fn layer(self) -> u32 {
        self.layer
    }

    /// How the reference frame is fitted onto the primary frame.
    pub const fn size_mode(self) -> DifferenceMatteSizeMode {
        self.size_mode
    }

    /// Tolerance in percent.
    pub const fn tolerance(self) -> f32 {
        self.tolerance
    }

    /// Softness in percent.
    pub const fn softness(self) -> f32 {
        self.softness
    }

    /// Blur radius in pixels, before rounding.
    pub const fn blur(self) -> f32 {
        self.blur
    }

    /// Tolerance in the normalized `0..=1` domain the kernel and GPU use.
    pub fn tolerance_normalized(self) -> f32 {
        self.tolerance / 100.0
    }

    /// Softness in the normalized `0..=1` domain the kernel and GPU use.
    pub fn softness_normalized(self) -> f32 {
        self.softness / 100.0
    }

    /// Rounded box-blur radius `R = clamp(round(blur), 0, 1000)`.
    pub fn blur_radius(self) -> u32 {
        self.blur.round().clamp(0.0, DIFFERENCE_MATTE_MAX_BLUR_PIXELS) as u32
    }

    /// Odd tap count `taps = 2R + 1` of the box average.
    pub fn taps(self) -> u32 {
        self.blur_radius().saturating_mul(2).saturating_add(1)
    }

    /// Packed controls shared with the GPU spatial pass.
    ///
    /// `params = [tolerance/100, softness/100, size_mode.code(), 0]` — the
    /// fourth lane is reserved: the renderer overwrites it with the linear
    /// filter flag.
    ///
    /// `extra = [R, 1/taps, stage_tag, layer]` — the renderer assigns the
    /// per-pass `stage_tag` (`0` single pass, `1` horizontal difference,
    /// `2` vertical average plus composite); this packing reports the
    /// single-pass form.
    pub fn gpu_controls(self) -> ([f32; 4], [f32; 4]) {
        let radius = self.blur_radius();
        let taps = self.taps() as f32;
        (
            [
                self.tolerance_normalized(),
                self.softness_normalized(),
                self.size_mode.code() as f32,
                0.0,
            ],
            [radius as f32, 1.0 / taps, 0.0, self.layer as f32],
        )
    }
}

impl Hash for DifferenceMatteGrade {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.layer.hash(state);
        self.size_mode.hash(state);
        self.tolerance.to_bits().hash(state);
        self.softness.to_bits().hash(state);
        self.blur.to_bits().hash(state);
    }
}

/// Reference uv for one primary pixel centre under the authored geometry.
///
/// The delivery affine is applied first, then the size mode fits the reference
/// aspect ratio into the primary frame.
pub(crate) fn reference_uv(
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    reference: AuxiliaryFloatFrame<'_>,
    sampling: AuxiliarySampling,
    size_mode: DifferenceMatteSizeMode,
) -> [f32; 2] {
    let mut uv = [
        (x as f32 + 0.5) / width as f32,
        (y as f32 + 0.5) / height as f32,
    ];
    if sampling.mapping == AuxiliaryCoordinateMapping::FrameTransform {
        let transform = reference.transform;
        uv = [
            transform[0] * uv[0] + transform[1] * uv[1] + transform[2],
            transform[3] * uv[0] + transform[4] * uv[1] + transform[5],
        ];
    }
    if size_mode == DifferenceMatteSizeMode::CenterCrop {
        let extent = reference.frame.extent();
        let primary_aspect = width as f32 / height as f32;
        let reference_aspect = extent.width() as f32 / extent.height() as f32;
        let crop_h = (reference_aspect / primary_aspect).min(1.0);
        let crop_w = (primary_aspect / reference_aspect).min(1.0);
        uv = [0.5 + (uv[0] - 0.5) / crop_w, 0.5 + (uv[1] - 0.5) / crop_h];
    }
    uv
}

/// Difference matte with an explicit cooperative checkpoint.
///
/// Returns `Ok(false)` when the buffers are inconsistent or a scratch plane
/// cannot be allocated, and `Ok(true)` after writing the whole frame.
pub fn apply_difference_matte_controlled<E>(
    primary: &mut [[f32; 4]],
    width: u32,
    height: u32,
    reference: AuxiliaryFloatFrame<'_>,
    sampling: AuxiliarySampling,
    grade: &DifferenceMatteGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    checkpoint()?;
    if (width as usize).checked_mul(height as usize) != Some(primary.len()) {
        return Ok(false);
    }
    let extent = reference.frame.extent();
    if extent.is_empty()
        || (extent.width() as usize).checked_mul(extent.height() as usize)
            != Some(reference.frame.pixels().len())
    {
        return Ok(false);
    }

    let mut source: Vec<[f32; 4]> = Vec::new();
    if source.try_reserve_exact(primary.len()).is_err() {
        return Ok(false);
    }
    source.extend_from_slice(primary);
    crate::straight_alpha::premultiply_rgba_controlled(&mut source, checkpoint)?;

    let radius = grade.blur_radius() as i32;
    let taps = grade.taps() as f32;
    let tolerance = grade.tolerance_normalized();
    let softness = grade.softness_normalized();
    let size_mode = grade.size_mode();
    let grid_sampling = AuxiliarySampling::new(
        AuxiliaryCoordinateMapping::PrimaryGrid,
        sampling.filter,
        sampling.out_of_bounds,
    );
    let width_i = width as i32;
    let height_i = height as i32;
    let stride = width as usize;

    let mut horizontal: Vec<[f32; 4]> = Vec::new();
    if radius > 0 {
        if horizontal.try_reserve_exact(primary.len()).is_err() {
            return Ok(false);
        }
        horizontal.resize(primary.len(), [0.0; 4]);
        for y in 0..height_i {
            let row = y as usize * stride;
            for x in 0..width_i {
                let mut sum = [0.0f32; 4];
                for offset in -radius..=radius {
                    let tap = (x + offset).clamp(0, width_i - 1) as u32;
                    let index = row + tap as usize;
                    let sampled = sample_auxiliary_associated_f32(
                        reference,
                        reference_uv(tap, y as u32, width, height, reference, sampling, size_mode),
                        grid_sampling,
                    );
                    let pixel = source[index];
                    sum[0] += pixel[0] - sampled[0];
                    sum[1] += pixel[1] - sampled[1];
                    sum[2] += pixel[2] - sampled[2];
                    sum[3] += pixel[3] - sampled[3];
                }
                horizontal[row + x as usize] =
                    [sum[0] / taps, sum[1] / taps, sum[2] / taps, sum[3] / taps];
                if (x as usize).is_multiple_of(CHECKPOINT_STRIDE) {
                    checkpoint()?;
                }
            }
        }
        for y in 0..height_i {
            let row = y as usize * stride;
            for x in 0..width_i {
                let mut sum = [0.0f32; 4];
                for offset in -radius..=radius {
                    let tap = (y + offset).clamp(0, height_i - 1) as usize;
                    let sampled = horizontal[tap * stride + x as usize];
                    sum[0] += sampled[0];
                    sum[1] += sampled[1];
                    sum[2] += sampled[2];
                    sum[3] += sampled[3];
                }
                let difference = [sum[0] / taps, sum[1] / taps, sum[2] / taps, sum[3] / taps];
                write_output(primary, row + x as usize, difference, tolerance, softness);
                if (x as usize).is_multiple_of(CHECKPOINT_STRIDE) {
                    checkpoint()?;
                }
            }
        }
    } else {
        for y in 0..height_i {
            let row = y as usize * stride;
            for x in 0..width_i {
                let index = row + x as usize;
                let sampled = sample_auxiliary_associated_f32(
                    reference,
                    reference_uv(
                        x as u32, y as u32, width, height, reference, sampling, size_mode,
                    ),
                    grid_sampling,
                );
                let pixel = source[index];
                let difference = [
                    pixel[0] - sampled[0],
                    pixel[1] - sampled[1],
                    pixel[2] - sampled[2],
                    pixel[3] - sampled[3],
                ];
                write_output(primary, index, difference, tolerance, softness);
                if index.is_multiple_of(CHECKPOINT_STRIDE) {
                    checkpoint()?;
                }
            }
        }
    }
    checkpoint()?;
    Ok(true)
}

/// Difference matte one Float32 frame against a reference frame.
///
/// Returns `false` when the buffers are inconsistent or a scratch plane could
/// not be allocated.
pub fn apply_difference_matte_f32(
    primary: &mut [[f32; 4]],
    width: u32,
    height: u32,
    reference: AuxiliaryFloatFrame<'_>,
    sampling: AuxiliarySampling,
    grade: &DifferenceMatteGrade,
) -> bool {
    apply_difference_matte_controlled(
        primary,
        width,
        height,
        reference,
        sampling,
        grade,
        &mut || Ok::<(), std::convert::Infallible>(()),
    )
    .unwrap_or(false)
}

#[inline]
fn write_output(
    primary: &mut [[f32; 4]],
    index: usize,
    difference: [f32; 4],
    tolerance: f32,
    softness: f32,
) {
    let straight = primary[index];
    let matte = matte_from_difference(difference, tolerance, softness);
    let alpha = straight[3].clamp(0.0, 1.0) * matte;
    primary[index] = if alpha > 0.0 {
        [straight[0], straight[1], straight[2], alpha]
    } else {
        [0.0; 4]
    };
}

/// Coverage multiplier for one averaged difference sample.
#[inline]
pub(crate) fn matte_from_difference(difference: [f32; 4], tolerance: f32, softness: f32) -> f32 {
    let distance = difference[0]
        .abs()
        .max(difference[1].abs())
        .max(difference[2].abs())
        .max(difference[3].abs());
    if softness == 0.0 {
        // Strictly greater: a sample that matches the reference stays back.
        if distance > tolerance {
            1.0
        } else {
            0.0
        }
    } else {
        ((distance - tolerance) / softness.max(EPSILON)).clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AuxiliaryPolicy, EffectColorDomain, EffectFloatFrameView, EffectFrameExtent,
        EffectValueFormat, EffectWorkingPrecision,
    };

    fn format() -> EffectValueFormat {
        EffectValueFormat::new(
            EffectWorkingPrecision::Float32,
            EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightCoverage,
        )
    }

    fn reference(pixels: &[[f32; 4]], width: u32, height: u32) -> AuxiliaryFloatFrame<'_> {
        assert_eq!(pixels.len(), (width as usize) * (height as usize));
        AuxiliaryFloatFrame::new(
            EffectFloatFrameView::new(
                EffectFrameExtent::new(width, height),
                format(),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                pixels,
            ),
            AuxiliaryPolicy::new(crate::AuxiliaryResolution::Authored),
        )
    }

    fn grade(
        layer: u32,
        size_mode: DifferenceMatteSizeMode,
        tolerance: f32,
        softness: f32,
        blur: f32,
    ) -> DifferenceMatteGrade {
        DifferenceMatteGrade::new(layer, size_mode, tolerance, softness, blur).unwrap()
    }

    fn solid(width: u32, height: u32, pixel: [f32; 4]) -> Vec<[f32; 4]> {
        vec![pixel; (width as usize) * (height as usize)]
    }

    fn run(
        primary: &mut [[f32; 4]],
        width: u32,
        height: u32,
        pixels: &[[f32; 4]],
        aux_width: u32,
        aux_height: u32,
        settings: DifferenceMatteGrade,
    ) -> bool {
        apply_difference_matte_f32(
            primary,
            width,
            height,
            reference(pixels, aux_width, aux_height),
            AuxiliarySampling::FRAME_TRANSFORM_LINEAR_CLAMP,
            &settings,
        )
    }

    #[test]
    fn size_modes_round_trip_through_their_codes_and_keys() {
        for (index, mode) in DifferenceMatteSizeMode::ALL.into_iter().enumerate() {
            assert_eq!(mode.code(), index as u32 + 1);
            assert_eq!(DifferenceMatteSizeMode::from_code(mode.code()), Some(mode));
            assert_eq!(DifferenceMatteSizeMode::from_key(mode.as_key()), Some(mode));
            assert!(!mode.label().is_empty());
        }
        assert_eq!(DifferenceMatteSizeMode::Stretch.as_key(), "stretch");
        assert_eq!(DifferenceMatteSizeMode::CenterCrop.as_key(), "center_crop");
        assert_eq!(DifferenceMatteSizeMode::Stretch.label(), "拉伸适配");
        assert_eq!(DifferenceMatteSizeMode::CenterCrop.label(), "中心裁剪");
        assert!(DifferenceMatteSizeMode::from_code(0).is_none());
        assert!(DifferenceMatteSizeMode::from_key("cover").is_none());
    }

    #[test]
    fn documented_defaults_round_trip() {
        let defaults = DifferenceMatteGrade::documented_defaults();
        assert_eq!(defaults.layer(), 6);
        assert_eq!(defaults.size_mode(), DifferenceMatteSizeMode::Stretch);
        assert_eq!(defaults.tolerance(), 15.0);
        assert_eq!(defaults.softness(), 0.0);
        assert_eq!(defaults.blur(), 0.0);
        assert_eq!(defaults.blur_radius(), 0);
        assert_eq!(defaults.taps(), 1);
        assert_eq!(
            defaults.gpu_controls(),
            ([0.15, 0.0, 1.0, 0.0], [0.0, 1.0, 0.0, 6.0])
        );
    }

    #[test]
    fn out_of_range_controls_are_rejected() {
        let mode = DifferenceMatteSizeMode::Stretch;
        for layer in [0, 20] {
            assert_eq!(
                DifferenceMatteGrade::new(layer, mode, 15.0, 0.0, 0.0),
                Err(DifferenceMatteError::LayerOutOfRange)
            );
        }
        for tolerance in [-1.0, 101.0, f32::NAN] {
            assert_eq!(
                DifferenceMatteGrade::new(6, mode, tolerance, 0.0, 0.0),
                Err(DifferenceMatteError::ToleranceOutOfRange)
            );
        }
        for softness in [-1.0, 101.0, f32::NAN] {
            assert_eq!(
                DifferenceMatteGrade::new(6, mode, 15.0, softness, 0.0),
                Err(DifferenceMatteError::SoftnessOutOfRange)
            );
        }
        for blur in [-1.0, 1001.0, f32::INFINITY] {
            assert_eq!(
                DifferenceMatteGrade::new(6, mode, 15.0, 0.0, blur),
                Err(DifferenceMatteError::BlurOutOfRange)
            );
        }
        assert_eq!(DifferenceMatteError::LayerOutOfRange.parameter(), "layer");
        assert_eq!(
            DifferenceMatteError::ToleranceOutOfRange.parameter(),
            "tolerance"
        );
        assert_eq!(
            DifferenceMatteError::SoftnessOutOfRange.parameter(),
            "softness"
        );
        assert_eq!(DifferenceMatteError::BlurOutOfRange.parameter(), "blur");
    }

    #[test]
    fn gpu_controls_pack_the_documented_layout() {
        let settings = grade(12, DifferenceMatteSizeMode::CenterCrop, 35.0, 20.0, 2.0);
        let (params, extra) = settings.gpu_controls();
        assert_eq!(params, [0.35, 0.2, 2.0, 0.0]);
        assert_eq!(extra[0], 2.0);
        assert_eq!(extra[1], 0.2);
        assert_eq!(extra[2], 0.0);
        assert_eq!(extra[3], 12.0);
        assert_eq!(settings.taps(), 5);
    }

    #[test]
    fn hash_is_stable_and_sensitive_to_every_control() {
        use std::collections::hash_map::DefaultHasher;

        fn hash_of(grade: DifferenceMatteGrade) -> u64 {
            let mut hasher = DefaultHasher::new();
            grade.hash(&mut hasher);
            hasher.finish()
        }

        let base = grade(6, DifferenceMatteSizeMode::Stretch, 15.0, 0.0, 0.0);
        assert_eq!(hash_of(base), hash_of(base));
        let variants = [
            grade(7, DifferenceMatteSizeMode::Stretch, 15.0, 0.0, 0.0),
            grade(6, DifferenceMatteSizeMode::CenterCrop, 15.0, 0.0, 0.0),
            grade(6, DifferenceMatteSizeMode::Stretch, 16.0, 0.0, 0.0),
            grade(6, DifferenceMatteSizeMode::Stretch, 15.0, 1.0, 0.0),
            grade(6, DifferenceMatteSizeMode::Stretch, 15.0, 0.0, 1.0),
        ];
        for variant in variants {
            assert_ne!(hash_of(base), hash_of(variant));
        }
    }

    #[test]
    fn radius_rounding_clamps_to_the_documented_bounds() {
        let settings = |blur: f32| grade(6, DifferenceMatteSizeMode::Stretch, 15.0, 0.0, blur);
        assert_eq!(settings(0.4).blur_radius(), 0);
        assert_eq!(settings(0.5).blur_radius(), 1);
        assert_eq!(settings(2.4).blur_radius(), 2);
        assert_eq!(settings(2.6).blur_radius(), 3);
        assert_eq!(settings(1000.0).blur_radius(), 1000);
        assert_eq!(settings(1000.0).taps(), 2001);
    }

    #[test]
    fn hard_threshold_keeps_a_matching_pixel_in_the_background() {
        // Premultiplied difference is exactly 0.5 on every channel.
        let mut primary = [[0.5, 0.5, 0.5, 1.0]];
        let aux = [[0.0, 0.0, 0.0, 0.5]];
        assert!(run(
            &mut primary,
            1,
            1,
            &aux,
            1,
            1,
            grade(6, DifferenceMatteSizeMode::Stretch, 50.0, 0.0, 0.0)
        ));
        assert_eq!(primary, [[0.0, 0.0, 0.0, 0.0]]);

        let mut primary = [[0.5, 0.5, 0.5, 1.0]];
        assert!(run(
            &mut primary,
            1,
            1,
            &aux,
            1,
            1,
            grade(6, DifferenceMatteSizeMode::Stretch, 49.0, 0.0, 0.0)
        ));
        assert_eq!(primary, [[0.5, 0.5, 0.5, 1.0]]);
    }

    #[test]
    fn softness_ramps_and_clamps_at_both_ends() {
        let expected = |difference: f32, tolerance: f32, softness: f32| {
            matte_from_difference([difference; 4], tolerance, softness)
        };
        assert!((expected(0.5, 0.0, 1.0) - 0.5).abs() < 1e-6);
        assert!((expected(0.25, 0.25, 0.5) - 0.0).abs() < 1e-6);
        assert!((expected(0.5, 0.25, 0.5) - 0.5).abs() < 1e-6);
        assert!((expected(0.6, 0.25, 0.5) - 0.7).abs() < 1e-6);
        assert!((expected(0.75, 0.25, 0.5) - 1.0).abs() < 1e-6);
        assert!((expected(1.0, 0.25, 0.5) - 1.0).abs() < 1e-6);
        assert_eq!(expected(0.5, 0.5, 0.0), 0.0);
        assert_eq!(expected(0.500_001, 0.5, 0.0), 1.0);
    }

    #[test]
    fn box_average_matches_the_explicit_two_dimensional_sum() {
        let width = 5usize;
        let height = 3usize;
        let settings = grade(6, DifferenceMatteSizeMode::Stretch, 0.0, 100.0, 2.0);
        let primary: Vec<[f32; 4]> = (0..width * height)
            .map(|index| {
                let value = (index as f32 * 0.13) % 1.0;
                [value, value * 0.5, 1.0 - value, 1.0]
            })
            .collect();
        let aux: Vec<[f32; 4]> = (0..width * height)
            .map(|index| {
                let value = (index as f32 * 0.07) % 1.0;
                [1.0 - value, value, 0.25, 1.0]
            })
            .collect();
        let mut expected = vec![0.0f32; width * height];
        for y in 0..height {
            for x in 0..width {
                let mut sum = [0.0f32; 4];
                for dy in -2i32..=2 {
                    for dx in -2i32..=2 {
                        let sx = (x as i32 + dx).clamp(0, width as i32 - 1) as usize;
                        let sy = (y as i32 + dy).clamp(0, height as i32 - 1) as usize;
                        let index = sy * width + sx;
                        let mut source = primary[index];
                        premultiply(&mut source);
                        let mut map = aux[index];
                        premultiply(&mut map);
                        sum[0] += source[0] - map[0];
                        sum[1] += source[1] - map[1];
                        sum[2] += source[2] - map[2];
                        sum[3] += source[3] - map[3];
                    }
                }
                let taps = 25.0;
                let difference = [sum[0] / taps, sum[1] / taps, sum[2] / taps, sum[3] / taps];
                expected[y * width + x] = matte_from_difference(
                    difference,
                    settings.tolerance_normalized(),
                    settings.softness_normalized(),
                );
            }
        }

        let mut output = primary.clone();
        assert!(run(
            &mut output,
            width as u32,
            height as u32,
            &aux,
            width as u32,
            height as u32,
            settings
        ));
        for (index, pixel) in output.iter().enumerate() {
            assert!(
                (pixel[3] - expected[index]).abs() < 1e-6,
                "pixel {index}: {} != {}",
                pixel[3],
                expected[index]
            );
        }
    }

    fn premultiply(pixel: &mut [f32; 4]) {
        crate::straight_alpha::premultiply_rgba_controlled(
            std::slice::from_mut(pixel),
            &mut || Ok::<(), std::convert::Infallible>(()),
        )
        .unwrap();
    }

    #[test]
    fn repeat_edge_taps_mirror_the_border() {
        // A single pixel can only ever compare against itself.
        let settings = grade(6, DifferenceMatteSizeMode::Stretch, 0.0, 100.0, 5.0);
        let mut primary = [[0.25, 0.5, 0.75, 1.0]];
        let aux = [[0.0, 0.0, 0.0, 1.0]];
        assert!(run(&mut primary, 1, 1, &aux, 1, 1, settings));
        let expected = matte_from_difference(
            [0.25, 0.5, 0.75, 0.0],
            settings.tolerance_normalized(),
            settings.softness_normalized(),
        );
        assert!((primary[0][3] - expected).abs() < 1e-6);
        assert_eq!(primary[0][0], 0.25);
        assert_eq!(primary[0][1], 0.5);
        assert_eq!(primary[0][2], 0.75);

        // Every tap of a 3 px row repeats the nearest border sample.
        let mut primary = vec![
            [0.0, 0.0, 0.0, 1.0],
            [1.0, 0.0, 0.0, 1.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let aux = solid(3, 1, [0.0, 0.0, 0.0, 1.0]);
        assert!(run(
            &mut primary,
            3,
            1,
            &aux,
            3,
            1,
            grade(6, DifferenceMatteSizeMode::Stretch, 0.0, 0.0, 1.0)
        ));
        // The middle sample averages [0, 1, 0] -> 1/3 over three taps.
        // Every pixel shares the same horizontal average here, so all three
        // outputs keep their own color and the shared matte is 1.
        assert_eq!(primary[0], [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(primary[1], [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(primary[2], [0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn stretch_maps_the_primary_grid_onto_the_reference_grid() {
        let aux = [[0.5, 0.5, 0.5, 1.0]];
        let frame = reference(&aux, 1, 1);
        let sampling = AuxiliarySampling::FRAME_TRANSFORM_LINEAR_CLAMP;
        let mode = DifferenceMatteSizeMode::Stretch;
        assert_eq!(
            reference_uv(0, 0, 16, 9, frame, sampling, mode),
            [0.5 / 16.0, 0.5 / 9.0]
        );
        assert_eq!(
            reference_uv(15, 8, 16, 9, frame, sampling, mode),
            [15.5 / 16.0, 8.5 / 9.0]
        );

        // A delivery affine shifts the primary centre before the reference is
        // sampled; the primary-grid mapping skips it.
        let shifted = AuxiliaryFloatFrame {
            transform: [1.0, 0.0, 0.25, 0.0, 1.0, 0.25],
            ..frame
        };
        assert_eq!(
            reference_uv(0, 0, 16, 9, shifted, sampling, mode),
            [0.5 / 16.0 + 0.25, 0.5 / 9.0 + 0.25]
        );
        assert_eq!(
            reference_uv(
                0,
                0,
                16,
                9,
                shifted,
                AuxiliarySampling::PRIMARY_GRID_LINEAR_CLAMP,
                mode
            ),
            [0.5 / 16.0, 0.5 / 9.0]
        );
    }

    #[test]
    fn centre_crop_maps_the_reference_aspect_into_the_primary_frame() {
        let aux_pixels = solid(4, 3, [0.5, 0.5, 0.5, 1.0]);
        let frame = reference(&aux_pixels, 4, 3);
        let sampling = AuxiliarySampling::FRAME_TRANSFORM_LINEAR_CLAMP;
        let mode = DifferenceMatteSizeMode::CenterCrop;
        // Primary 16x9 (Af = 1.778) against a 4x3 reference (Aa = 1.333) keeps
        // the full width and crops 25% of the reference height.
        let af: f32 = 16.0 / 9.0;
        let aa: f32 = 4.0 / 3.0;
        let crop_h = (aa / af).min(1.0);
        let crop_w = (af / aa).min(1.0);
        assert!((crop_w - 1.0).abs() < 1e-6);
        assert!((crop_h - 0.75).abs() < 1e-6);
        assert!((reference_uv(8, 4, 16, 9, frame, sampling, mode)[1] - 0.5).abs() < 1e-6);
        let top = reference_uv(8, 0, 16, 9, frame, sampling, mode);
        assert!((top[0] - 8.5 / 16.0).abs() < 1e-6);
        assert!((top[1] - (0.5 + (0.5 / 9.0 - 0.5) / crop_h)).abs() < 1e-6);

        // A wide reference inside a portrait primary crops horizontally.
        let wide_pixels = solid(16, 9, [0.5, 0.5, 0.5, 1.0]);
        let wide = reference(&wide_pixels, 16, 9);
        let af: f32 = 9.0 / 16.0;
        let aa: f32 = 16.0 / 9.0;
        let crop_w = (af / aa).min(1.0);
        let crop_h = (aa / af).min(1.0);
        assert!((crop_h - 1.0).abs() < 1e-6);
        let left = reference_uv(0, 8, 9, 16, wide, sampling, mode);
        assert!((left[0] - (0.5 + (0.5 / 9.0 - 0.5) / crop_w)).abs() < 1e-6);
        assert!((left[1] - (0.5 + (8.5 / 16.0 - 0.5) / crop_h)).abs() < 1e-6);

        // Behavioural difference: a black primary over a reference whose first
        // row is white. Under stretch, primary row 2 lands one third of the way
        // from reference row 0 to row 1 (two thirds white); the centre crop
        // stretches that row across a wider band and sees less white.
        let striped: Vec<[f32; 4]> = (0..12)
            .map(|index| {
                if index < 4 {
                    [1.0, 1.0, 1.0, 1.0]
                } else {
                    [0.0, 0.0, 0.0, 1.0]
                }
            })
            .collect();
        let mut cropped = vec![[0.0, 0.0, 0.0, 1.0]; 16 * 9];
        assert!(run(
            &mut cropped,
            16,
            9,
            &striped,
            4,
            3,
            grade(6, DifferenceMatteSizeMode::CenterCrop, 0.0, 100.0, 0.0)
        ));
        let mut stretched = vec![[0.0, 0.0, 0.0, 1.0]; 16 * 9];
        assert!(run(
            &mut stretched,
            16,
            9,
            &striped,
            4,
            3,
            grade(6, DifferenceMatteSizeMode::Stretch, 0.0, 100.0, 0.0)
        ));
        let sample = 2 * 16;
        assert!((stretched[sample][3] - 2.0 / 3.0).abs() < 1e-3);
        assert!((cropped[sample][3] - 8.0 / 9.0).abs() < 1e-3);
        assert!((cropped[sample][3] - stretched[sample][3]).abs() > 1e-2);
    }

    #[test]
    fn centre_crop_outside_the_reference_reads_transparent() {
        let settings = grade(6, DifferenceMatteSizeMode::CenterCrop, 15.0, 0.0, 0.0);
        let pixels = solid(4, 3, [0.5, 0.5, 0.5, 1.0]);
        let transparent = AuxiliarySampling::new(
            crate::AuxiliaryCoordinateMapping::FrameTransform,
            crate::AuxiliaryFilter::Linear,
            crate::AuxiliaryOutOfBounds::Transparent,
        );

        let mut clamped = solid(16, 9, [0.5, 0.5, 0.5, 1.0]);
        assert!(apply_difference_matte_f32(
            &mut clamped,
            16,
            9,
            reference(&pixels, 4, 3),
            AuxiliarySampling::FRAME_TRANSFORM_LINEAR_CLAMP,
            &settings,
        ));
        assert!(
            clamped.iter().all(|pixel| pixel[3] == 0.0),
            "a clamped border repeats a matching reference, so the whole frame stays background"
        );

        let mut cropped = solid(16, 9, [0.5, 0.5, 0.5, 1.0]);
        assert!(apply_difference_matte_f32(
            &mut cropped,
            16,
            9,
            reference(&pixels, 4, 3),
            transparent,
            &settings,
        ));
        for row in [0u32, 8] {
            for column in 0..16u32 {
                let pixel = cropped[(row * 16 + column) as usize];
                assert_eq!(
                    pixel[3], 1.0,
                    "row {row} column {column} maps outside the reference window"
                );
                assert_eq!(pixel[..3], [0.5, 0.5, 0.5], "the source rgb is untouched");
            }
        }
        for column in 2..10u32 {
            assert_eq!(
                cropped[(4 * 16 + column) as usize][3],
                0.0,
                "the interior of the centre row still matches the reference"
            );
        }
    }

    #[test]
    fn rgb_is_preserved_and_uncovered_pixels_become_transparent() {
        let mut primary = [[0.2, 0.4, 0.6, 1.0], [0.9, 0.8, 0.7, 1.0]];
        let aux = solid(2, 1, [0.0, 0.0, 0.0, 0.0]);
        assert!(run(
            &mut primary,
            2,
            1,
            &aux,
            2,
            1,
            grade(6, DifferenceMatteSizeMode::Stretch, 0.0, 0.0, 0.0)
        ));
        assert_eq!(primary[0], [0.2, 0.4, 0.6, 1.0]);
        assert_eq!(primary[1], [0.9, 0.8, 0.7, 1.0]);

        // A fully transparent source keeps no color at all.
        let mut primary = [[0.0, 0.0, 0.0, 0.0]];
        let transparent = solid(1, 1, [0.0, 0.0, 0.0, 0.0]);
        assert!(run(
            &mut primary,
            1,
            1,
            &transparent,
            1,
            1,
            grade(6, DifferenceMatteSizeMode::Stretch, 0.0, 0.0, 0.0)
        ));
        assert_eq!(primary, [[0.0, 0.0, 0.0, 0.0]]);
    }

    #[test]
    fn the_kernel_is_deterministic_at_the_maximum_radius() {
        let width = 3;
        let settings = grade(6, DifferenceMatteSizeMode::Stretch, 10.0, 20.0, 1000.0);
        let source = vec![
            [0.25, 0.5, 0.75, 1.0],
            [0.5, 0.5, 0.5, 1.0],
            [0.0, 1.0, 0.0, 1.0],
        ];
        let aux = vec![
            [0.1, 0.2, 0.3, 1.0],
            [0.5, 0.5, 0.5, 1.0],
            [0.9, 0.8, 0.7, 1.0],
        ];
        let mut first = source.clone();
        let mut second = source.clone();
        assert!(run(&mut first, width, 1, &aux, width, 1, settings));
        assert!(run(&mut second, width, 1, &aux, width, 1, settings));
        assert_eq!(first, second);
        assert_eq!(settings.blur_radius(), 1000);
    }

    #[test]
    fn inconsistent_buffers_and_an_empty_reference_fail_closed() {
        let aux = solid(1, 1, [0.0, 0.0, 0.0, 1.0]);
        let mut primary = [[0.5, 0.5, 0.5, 1.0], [0.5, 0.5, 0.5, 1.0]];
        assert!(!run(
            &mut primary,
            1,
            1,
            &aux,
            1,
            1,
            grade(6, DifferenceMatteSizeMode::Stretch, 15.0, 0.0, 0.0)
        ));
        let empty: Vec<[f32; 4]> = Vec::new();
        let mut primary = [[0.5, 0.5, 0.5, 1.0]];
        assert!(!apply_difference_matte_f32(
            &mut primary,
            1,
            1,
            reference(&empty, 0, 0),
            AuxiliarySampling::FRAME_TRANSFORM_LINEAR_CLAMP,
            &grade(6, DifferenceMatteSizeMode::Stretch, 15.0, 0.0, 0.0),
        ));
    }
}
