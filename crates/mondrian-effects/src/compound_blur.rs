//! Compound blur (BLR-09): blurs the frame with a second layer as the radius map.
//!
//! The authored controls are the auxiliary layer slot that carries the radius
//! map, the largest radius in pixels, how a differently sized map is fitted onto
//! the primary frame, and whether the map is inverted.
//!
//! The map contributes one number per pixel: the Rec. 709 luminance of its
//! straight sample, clamped to the coverage range. The radius is
//! `max_blur * luminance` (or `max_blur * (1 - luminance)` when the map is
//! inverted), so a black map leaves the frame unchanged and a white map applies
//! the full radius. A pixel whose radius is below half a pixel is copied from
//! the source instead of being resampled.
//!
//! Every other pixel is recomputed as the equal-weight mean of 25 premultiplied
//! taps: the pixel itself plus 24 taps on the disc, placed at the golden angle so
//! the tap set is never aligned with the axes. The primary working buffer is
//! premultiplied, matching the blend and compound-arithmetic kernels: the kernel
//! premultiplies the frame before sampling, filters coverage and colour together,
//! and writes the premultiplied mean back. A mean coverage of zero or below is
//! normalized to transparent black; no value is clamped otherwise.

use std::hash::{Hash, Hasher};

use crate::auxiliary::{sample_auxiliary_f32, AuxiliaryFloatFrame};
use crate::graph::{AuxiliaryCoordinateMapping, AuxiliarySampling};

/// Largest accepted blur radius, in pixels.
pub const COMPOUND_BLUR_MAX_BLUR_PIXELS: f32 = 4000.0;
/// Lowest accepted auxiliary layer slot; `0` means no radius map.
pub const COMPOUND_BLUR_MIN_LAYER: u32 = 0;
/// Highest accepted auxiliary layer slot.
pub const COMPOUND_BLUR_MAX_LAYER: u32 = 19;
/// Documented default auxiliary layer slot.
pub const COMPOUND_BLUR_DEFAULT_LAYER: u32 = 6;
/// Documented default largest radius, in pixels.
pub const COMPOUND_BLUR_DEFAULT_MAX_BLUR: f32 = 20.0;
/// Disc taps besides the centre sample, for 25 samples in total.
pub const COMPOUND_BLUR_TAPS: u32 = 24;
/// Golden angle in radians, which keeps the disc taps off the axes.
pub const COMPOUND_BLUR_GOLDEN_ANGLE: f32 = 2.399_963_2;

/// Authored fit of a differently sized radius map onto the primary frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CompoundBlurSizeMode {
    /// Stretch the map onto the primary frame.
    Stretch,
    /// Fit the map aspect ratio onto the primary frame, centred.
    CenterCrop,
}

impl CompoundBlurSizeMode {
    /// Every authored size mode, in menu order.
    pub const ALL: [Self; 2] = [Self::Stretch, Self::CenterCrop];

    /// Stable code used by the graph identity byte stream.
    pub const fn code(self) -> u8 {
        match self {
            Self::Stretch => 1,
            Self::CenterCrop => 2,
        }
    }

    /// The size mode for one identity code.
    pub const fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::Stretch),
            2 => Some(Self::CenterCrop),
            _ => None,
        }
    }

    /// Stable key used by the authored properties.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Stretch => "stretch",
            Self::CenterCrop => "center_crop",
        }
    }

    /// The size mode for one authored key.
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "stretch" => Some(Self::Stretch),
            "center_crop" => Some(Self::CenterCrop),
            _ => None,
        }
    }

    /// Localized menu label, matching the authored properties.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Stretch => "拉伸适配",
            Self::CenterCrop => "中心裁剪",
        }
    }
}

/// One authored compound-blur control outside its documented range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompoundBlurError {
    parameter: &'static str,
}

impl CompoundBlurError {
    const fn new(parameter: &'static str) -> Self {
        Self { parameter }
    }

    /// The authored property that was rejected.
    pub const fn parameter(self) -> &'static str {
        self.parameter
    }
}

impl std::fmt::Display for CompoundBlurError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "compound_blur.{} must be finite and within its documented range",
            self.parameter
        )
    }
}

impl std::error::Error for CompoundBlurError {}

/// Validated compound-blur controls.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CompoundBlurGrade {
    blur_layer: u32,
    max_blur: f32,
    size_mode: CompoundBlurSizeMode,
    invert_blur: bool,
}

impl CompoundBlurGrade {
    /// Validate one authored control set.
    pub fn new(
        blur_layer: u32,
        max_blur: f32,
        size_mode: CompoundBlurSizeMode,
        invert_blur: bool,
    ) -> Result<Self, CompoundBlurError> {
        if !(COMPOUND_BLUR_MIN_LAYER..=COMPOUND_BLUR_MAX_LAYER).contains(&blur_layer) {
            return Err(CompoundBlurError::new("blur_layer"));
        }
        if !max_blur.is_finite() || !(0.0..=COMPOUND_BLUR_MAX_BLUR_PIXELS).contains(&max_blur) {
            return Err(CompoundBlurError::new("max_blur"));
        }
        Ok(Self { blur_layer, max_blur, size_mode, invert_blur })
    }

    /// The documented defaults: slot 6, 20 px, stretch, not inverted.
    pub const fn documented_defaults() -> Self {
        Self {
            blur_layer: COMPOUND_BLUR_DEFAULT_LAYER,
            max_blur: COMPOUND_BLUR_DEFAULT_MAX_BLUR,
            size_mode: CompoundBlurSizeMode::Stretch,
            invert_blur: false,
        }
    }

    /// Auxiliary layer slot that carries the radius map.
    pub const fn blur_layer(self) -> u32 {
        self.blur_layer
    }

    /// Largest radius, in pixels, reached by a fully bright map.
    pub const fn max_blur(self) -> f32 {
        self.max_blur
    }

    /// Authored fit of a differently sized map.
    pub const fn size_mode(self) -> CompoundBlurSizeMode {
        self.size_mode
    }

    /// Whether the map is inverted before it drives the radius.
    pub const fn invert_blur(self) -> bool {
        self.invert_blur
    }

    /// Whether the authored controls leave the frame untouched.
    pub const fn is_identity(self) -> bool {
        self.blur_layer == 0 || self.max_blur <= 0.0
    }

    /// Packed controls shared with the GPU spatial pass.
    ///
    /// `params = [max_blur, invert_flag, size_mode.code(), 0]` — the fourth lane
    /// is reserved: the renderer overwrites it with the linear filter flag.
    ///
    /// `extra = [crop_w, crop_h, blur_layer, 0]`. The crop window depends on both
    /// rasters' extents, which the grade does not know, so the two crop lanes are
    /// placeholders the renderer fills from the bound textures; the kernel
    /// derives the same window from the frame extents.
    pub fn gpu_controls(self) -> ([f32; 4], [f32; 4]) {
        (
            [
                self.max_blur,
                if self.invert_blur { 1.0 } else { 0.0 },
                self.size_mode.code() as f32,
                0.0,
            ],
            [0.0, 0.0, self.blur_layer as f32, 0.0],
        )
    }

    /// Hash every authored control into the render operation's signature.
    pub fn hash_state<H: Hasher>(&self, state: &mut H) {
        self.blur_layer.hash(state);
        self.max_blur.to_bits().hash(state);
        self.size_mode.hash(state);
        self.invert_blur.hash(state);
    }
}

impl Hash for CompoundBlurGrade {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.hash_state(state);
    }
}

const CHECKPOINT_STRIDE: usize = 4096;

/// Applies the compound blur to one premultiplied rgba f32 frame.
///
/// Returns `false` only when the frame or the radius map cannot be addressed;
/// the grade is assumed to be validated already.
pub fn compound_blur_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    blur_map: AuxiliaryFloatFrame<'_>,
    sampling: AuxiliarySampling,
    grade: &CompoundBlurGrade,
) -> bool {
    let (Ok(width), Ok(height)) = (u32::try_from(width), u32::try_from(height)) else {
        return false;
    };
    apply_compound_blur_f32(pixels, width, height, blur_map, sampling, grade)
}

/// Applies the compound blur to one Float32 frame with a cooperative checkpoint.
///
/// Returns `Ok(false)` when the buffers are inconsistent, and `Ok(true)` after
/// writing the whole frame.
pub fn apply_compound_blur_controlled<E>(
    primary: &mut [[f32; 4]],
    width: u32,
    height: u32,
    blur_map: AuxiliaryFloatFrame<'_>,
    sampling: AuxiliarySampling,
    grade: &CompoundBlurGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    checkpoint()?;
    let Some(pixel_count) = (width as usize).checked_mul(height as usize) else {
        return Ok(false);
    };
    if pixel_count == 0 || primary.len() != pixel_count {
        return Ok(false);
    }
    let extent = blur_map.frame.extent();
    if extent.is_empty()
        || (extent.width() as usize).checked_mul(extent.height() as usize)
            != Some(blur_map.frame.pixels().len())
    {
        return Ok(false);
    }

    // The reference uv already contains the delivery affine and the size-mode
    // fit, so the sampler must read it on the primary grid rather than applying
    // the affine a second time.
    let grid_sampling = AuxiliarySampling::new(
        AuxiliaryCoordinateMapping::PrimaryGrid,
        sampling.filter,
        sampling.out_of_bounds,
    );

    // The working buffer is straight when it arrives and premultiplied from here
    // on: filtering colour without its coverage would let transparent neighbours
    // darken an opaque pixel.
    crate::straight_alpha::premultiply_rgba_controlled(primary, checkpoint)?;

    // The 25 taps read the premultiplied source frame while the destination is
    // written, so the source has to survive until the last pixel.
    let mut source = Vec::with_capacity(pixel_count);
    for (index, pixel) in primary.iter().enumerate() {
        if index.is_multiple_of(CHECKPOINT_STRIDE) {
            checkpoint()?;
        }
        source.push(*pixel);
    }
    checkpoint()?;

    let frame_width = width as usize;
    let frame_height = height as usize;
    let rows = frame_height;
    let max_blur = grade.max_blur();
    let invert = grade.invert_blur();
    let size_mode = grade.size_mode();
    let transform = blur_map.transform;
    let mapping = sampling.mapping;
    let sample_count = (COMPOUND_BLUR_TAPS + 1) as f32;
    crate::parallel::for_each_row_band(primary, frame_width, 0..rows, |row_start, band| {
        for (row_offset, row_pixels) in band.chunks_mut(frame_width).enumerate() {
            let y = row_start + row_offset;
            for (column, pixel) in row_pixels.iter_mut().enumerate() {
                let index = y * frame_width + column;
                let uv = compound_blur_reference_uv(
                    column as u32,
                    y as u32,
                    width,
                    height,
                    transform,
                    extent,
                    mapping,
                    size_mode,
                );
                let map = sample_auxiliary_f32(blur_map, uv, grid_sampling);
                let mut luminance =
                    (0.2126 * map[0] + 0.7152 * map[1] + 0.0722 * map[2]).clamp(0.0, 1.0);
                if invert {
                    luminance = 1.0 - luminance;
                }
                let radius = max_blur * luminance;
                let centre = source[index];
                if radius < 0.5 {
                    *pixel = centre;
                    continue;
                }
                let mut sum = [0.0_f32; 4];
                for tap in 0..=COMPOUND_BLUR_TAPS {
                    // The centre sample is followed by the equal-radius disc
                    // taps; `sqrt(k / 24)` equalizes the area each tap covers.
                    let (distance, angle) = if tap == 0 {
                        (0.0, 0.0)
                    } else {
                        (
                            radius * (tap as f32 / COMPOUND_BLUR_TAPS as f32).sqrt(),
                            0.5 + tap as f32 * COMPOUND_BLUR_GOLDEN_ANGLE,
                        )
                    };
                    let sample = crate::resample::sample_bilinear_clamped(
                        &source,
                        frame_width,
                        frame_height,
                        column as f64 + f64::from(distance * angle.cos()),
                        y as f64 + f64::from(distance * angle.sin()),
                    );
                    for (channel, value) in sum.iter_mut().enumerate() {
                        *value += sample[channel];
                    }
                }
                let average = [
                    sum[0] / sample_count,
                    sum[1] / sample_count,
                    sum[2] / sample_count,
                    sum[3] / sample_count,
                ];
                *pixel = if average[3] <= 0.0 { [0.0; 4] } else { average };
            }
        }
    });
    checkpoint()?;
    Ok(true)
}

/// Applies the compound blur to one Float32 frame.
///
/// Returns `false` when the buffers are inconsistent.
pub fn apply_compound_blur_f32(
    primary: &mut [[f32; 4]],
    width: u32,
    height: u32,
    blur_map: AuxiliaryFloatFrame<'_>,
    sampling: AuxiliarySampling,
    grade: &CompoundBlurGrade,
) -> bool {
    apply_compound_blur_controlled(
        primary,
        width,
        height,
        blur_map,
        sampling,
        grade,
        &mut || Ok::<(), std::convert::Infallible>(()),
    )
    .unwrap_or(false)
}

/// Reference uv for one primary pixel centre under the authored geometry.
///
/// The delivery affine is applied first (only when the sampling declaration is
/// `FrameTransform`), then the size mode fits the map aspect ratio into the
/// primary frame. The kernel samples the result on the primary grid, so the
/// affine is never applied twice.
pub(crate) fn compound_blur_reference_uv(
    x: u32,
    y: u32,
    frame_width: u32,
    frame_height: u32,
    transform: [f32; 6],
    extent: crate::EffectFrameExtent,
    sampling_mapping: AuxiliaryCoordinateMapping,
    size_mode: CompoundBlurSizeMode,
) -> [f32; 2] {
    let mut uv = [
        (x as f32 + crate::auxiliary::PIXEL_CENTRE) / frame_width as f32,
        (y as f32 + crate::auxiliary::PIXEL_CENTRE) / frame_height as f32,
    ];
    if sampling_mapping == AuxiliaryCoordinateMapping::FrameTransform {
        uv = [
            transform[0] * uv[0] + transform[1] * uv[1] + transform[2],
            transform[3] * uv[0] + transform[4] * uv[1] + transform[5],
        ];
    }
    if size_mode == CompoundBlurSizeMode::CenterCrop {
        let primary_aspect = frame_width as f32 / frame_height as f32;
        let reference_aspect = extent.width() as f32 / extent.height() as f32;
        let crop_h = (reference_aspect / primary_aspect).min(1.0);
        let crop_w = (primary_aspect / reference_aspect).min(1.0);
        uv = [0.5 + (uv[0] - 0.5) / crop_w, 0.5 + (uv[1] - 0.5) / crop_h];
    }
    uv
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

    fn sampling() -> AuxiliarySampling {
        AuxiliarySampling::PRIMARY_GRID_LINEAR_CLAMP
    }

    fn transparent_sampling() -> AuxiliarySampling {
        AuxiliarySampling::new(
            AuxiliaryCoordinateMapping::PrimaryGrid,
            crate::AuxiliaryFilter::Linear,
            crate::AuxiliaryOutOfBounds::Transparent,
        )
    }

    fn grade(
        max_blur: f32,
        size_mode: CompoundBlurSizeMode,
        invert_blur: bool,
    ) -> CompoundBlurGrade {
        CompoundBlurGrade::new(6, max_blur, size_mode, invert_blur).unwrap()
    }

    fn solid(pixel: [f32; 4], width: u32, height: u32) -> Vec<[f32; 4]> {
        vec![pixel; (width as usize) * (height as usize)]
    }

    fn run(
        primary: &mut [[f32; 4]],
        width: u32,
        height: u32,
        map: &[[f32; 4]],
        map_width: u32,
        map_height: u32,
        grade: CompoundBlurGrade,
    ) -> bool {
        apply_compound_blur_f32(
            primary,
            width,
            height,
            reference(map, map_width, map_height),
            sampling(),
            &grade,
        )
    }

    fn assert_close(actual: f32, expected: f32, context: &str) {
        assert!(
            (actual - expected).abs() <= 1.0e-5,
            "{context}: expected {expected}, got {actual}"
        );
    }

    fn assert_pixel_close(actual: [f32; 4], expected: [f32; 4], context: &str) {
        for (channel, (actual, expected)) in actual.iter().zip(expected).enumerate() {
            assert!(
                (actual - expected).abs() <= 1.0e-5,
                "{context} channel {channel}: expected {expected}, got {actual}"
            );
        }
    }

    #[test]
    fn documented_defaults_are_the_authored_defaults() {
        let defaults = CompoundBlurGrade::documented_defaults();
        assert_eq!(defaults.blur_layer(), 6);
        assert_eq!(defaults.max_blur(), 20.0);
        assert_eq!(defaults.size_mode(), CompoundBlurSizeMode::Stretch);
        assert!(!defaults.invert_blur());
        assert!(!defaults.is_identity());
        assert_eq!(COMPOUND_BLUR_MIN_LAYER, 0);
        assert_eq!(COMPOUND_BLUR_MAX_LAYER, 19);
        assert_eq!(COMPOUND_BLUR_DEFAULT_LAYER, 6);
        assert_eq!(COMPOUND_BLUR_DEFAULT_MAX_BLUR, 20.0);
        assert_eq!(COMPOUND_BLUR_MAX_BLUR_PIXELS, 4000.0);
        assert_eq!(COMPOUND_BLUR_TAPS, 24);
        assert_eq!(COMPOUND_BLUR_GOLDEN_ANGLE, 2.399_963_2);
        assert!(!grade(20.0, CompoundBlurSizeMode::Stretch, false).is_identity());
        assert!(
            CompoundBlurGrade::new(0, 20.0, CompoundBlurSizeMode::Stretch, false)
                .unwrap()
                .is_identity()
        );
        assert!(
            CompoundBlurGrade::new(6, 0.0, CompoundBlurSizeMode::Stretch, false)
                .unwrap()
                .is_identity()
        );
    }

    #[test]
    fn size_modes_round_trip_through_their_codes_and_keys() {
        assert_eq!(CompoundBlurSizeMode::ALL.len(), 2);
        for mode in CompoundBlurSizeMode::ALL {
            assert_eq!(CompoundBlurSizeMode::from_code(mode.code()), Some(mode));
            assert_eq!(CompoundBlurSizeMode::from_key(mode.as_key()), Some(mode));
            assert!(!mode.label().is_empty());
        }
        assert_eq!(CompoundBlurSizeMode::from_code(0), None);
        assert_eq!(CompoundBlurSizeMode::from_key("fit"), None);
        assert_eq!(CompoundBlurSizeMode::Stretch.code(), 1);
        assert_eq!(CompoundBlurSizeMode::CenterCrop.code(), 2);
        assert_eq!(CompoundBlurSizeMode::Stretch.as_key(), "stretch");
        assert_eq!(CompoundBlurSizeMode::CenterCrop.as_key(), "center_crop");
        assert_eq!(CompoundBlurSizeMode::Stretch.label(), "拉伸适配");
        assert_eq!(CompoundBlurSizeMode::CenterCrop.label(), "中心裁剪");
    }

    #[test]
    fn params_outside_their_documented_ranges_error() {
        for layer in [20_u32, 1000] {
            let error = CompoundBlurGrade::new(layer, 20.0, CompoundBlurSizeMode::Stretch, false)
                .unwrap_err();
            assert_eq!(error.parameter(), "blur_layer");
            assert!(error.to_string().contains("blur_layer"));
        }
        for max_blur in [-1.0, -0.5, f32::NAN, f32::INFINITY, 4000.5] {
            let error = CompoundBlurGrade::new(6, max_blur, CompoundBlurSizeMode::Stretch, false)
                .unwrap_err();
            assert_eq!(error.parameter(), "max_blur");
        }
        assert!(CompoundBlurGrade::new(19, 4000.0, CompoundBlurSizeMode::CenterCrop, true).is_ok());
    }

    #[test]
    fn gpu_controls_pack_the_documented_layout() {
        let (params, extra) = grade(40.0, CompoundBlurSizeMode::CenterCrop, true).gpu_controls();
        assert_eq!(params, [40.0, 1.0, 2.0, 0.0]);
        assert_eq!(extra, [0.0, 0.0, 6.0, 0.0]);
        let (params, _) = CompoundBlurGrade::documented_defaults().gpu_controls();
        assert_eq!(params, [20.0, 0.0, 1.0, 0.0]);
    }

    #[test]
    fn black_map_is_the_identity() {
        let (width, height) = (8_u32, 6_u32);
        let mut frame: Vec<[f32; 4]> = (0..width * height)
            .map(|index| {
                let value = index as f32 / (width * height) as f32;
                [value, 1.0 - value, value * 0.5, 1.0]
            })
            .collect();
        let original = frame.clone();
        assert!(run(
            &mut frame,
            width,
            height,
            &solid([0.0, 0.0, 0.0, 1.0], width, height),
            width,
            height,
            grade(64.0, CompoundBlurSizeMode::Stretch, false),
        ));
        assert_eq!(frame, original);
    }

    #[test]
    fn max_blur_zero_is_the_identity() {
        let (width, height) = (8_u32, 6_u32);
        let mut frame: Vec<[f32; 4]> = (0..width * height)
            .map(|index| {
                let value = index as f32 / (width * height) as f32;
                [value, 1.0 - value, value * 0.5, 1.0]
            })
            .collect();
        let original = frame.clone();
        assert!(run(
            &mut frame,
            width,
            height,
            &solid([1.0, 1.0, 1.0, 1.0], width, height),
            width,
            height,
            grade(0.0, CompoundBlurSizeMode::Stretch, false),
        ));
        assert_eq!(frame, original);
    }

    #[test]
    fn radius_scales_linearly_with_the_map_luminance() {
        // A linear ramp makes every bilinear tap an exact linear function of its
        // offset, so the mean offset (and therefore the deviation from the
        // source) is exactly proportional to the radius.
        const WIDTH: u32 = 128;
        const HEIGHT: u32 = 128;
        let ramp: Vec<[f32; 4]> = (0..WIDTH * HEIGHT)
            .map(|index| {
                let x = (index % WIDTH) as f32;
                [x / (WIDTH - 1) as f32, 0.0, 0.0, 1.0]
            })
            .collect();
        let source = ramp.clone();
        let mut quarter = ramp.clone();
        assert!(run(
            &mut quarter,
            WIDTH,
            HEIGHT,
            &solid([0.25, 0.25, 0.25, 1.0], WIDTH, HEIGHT),
            WIDTH,
            HEIGHT,
            grade(64.0, CompoundBlurSizeMode::Stretch, false),
        ));
        let mut half = ramp.clone();
        assert!(run(
            &mut half,
            WIDTH,
            HEIGHT,
            &solid([0.5, 0.5, 0.5, 1.0], WIDTH, HEIGHT),
            WIDTH,
            HEIGHT,
            grade(64.0, CompoundBlurSizeMode::Stretch, false),
        ));
        // The 0.25 map reaches 16 px and the 0.5 map 32 px, so the deviation from
        // the source doubles even though the tap weights are unchanged. Both
        // columns stay far enough inside the frame that no tap is clamped; the
        // ramp is linear in x, so each bilinear tap is exact.
        for column in [64_usize, 70] {
            let index = 64 * WIDTH as usize + column;
            let quarter_delta = quarter[index][0] - source[index][0];
            let half_delta = half[index][0] - source[index][0];
            assert!(
                quarter_delta.abs() > 1.0e-4,
                "the blur must move the ramp at column {column}"
            );
            assert_close(
                half_delta,
                2.0 * quarter_delta,
                &format!("column {column} radius scales linearly with the map luminance"),
            );
        }
    }

    #[test]
    fn invert_flips_the_radius() {
        const WIDTH: u32 = 32;
        const HEIGHT: u32 = 32;
        let ramp: Vec<[f32; 4]> = (0..WIDTH * HEIGHT)
            .map(|index| {
                let x = (index % WIDTH) as f32;
                [x / (WIDTH - 1) as f32, 0.0, 0.0, 1.0]
            })
            .collect();
        let mut direct = ramp.clone();
        let mut inverted = ramp.clone();
        assert!(run(
            &mut direct,
            WIDTH,
            HEIGHT,
            &solid([0.25, 0.25, 0.25, 1.0], WIDTH, HEIGHT),
            WIDTH,
            HEIGHT,
            grade(8.0, CompoundBlurSizeMode::Stretch, false),
        ));
        assert!(run(
            &mut inverted,
            WIDTH,
            HEIGHT,
            &solid([0.75, 0.75, 0.75, 1.0], WIDTH, HEIGHT),
            WIDTH,
            HEIGHT,
            grade(8.0, CompoundBlurSizeMode::Stretch, true),
        ));
        for (index, (direct, inverted)) in direct.iter().zip(&inverted).enumerate() {
            assert_pixel_close(*direct, *inverted, &format!("pixel {index}"));
        }
    }

    #[test]
    fn a_constant_frame_survives_every_radius() {
        // The centre tap carries the same weight as the disc taps, and a constant
        // frame averages back to itself.
        const WIDTH: u32 = 16;
        const HEIGHT: u32 = 16;
        for max_blur in [1.0_f32, 8.0, 200.0] {
            let mut frame = solid([0.2, 0.4, 0.8, 1.0], WIDTH, HEIGHT);
            assert!(run(
                &mut frame,
                WIDTH,
                HEIGHT,
                &solid([1.0, 1.0, 1.0, 1.0], WIDTH, HEIGHT),
                WIDTH,
                HEIGHT,
                grade(max_blur, CompoundBlurSizeMode::Stretch, false),
            ));
            for (index, pixel) in frame.iter().enumerate() {
                assert_pixel_close(
                    *pixel,
                    [0.2, 0.4, 0.8, 1.0],
                    &format!("max_blur {max_blur} pixel {index}"),
                );
            }
        }
    }

    #[test]
    fn disc_sampling_softens_the_step_edge() {
        const WIDTH: u32 = 64;
        const HEIGHT: u32 = 32;
        let edge = WIDTH as usize / 2;
        let mut frame: Vec<[f32; 4]> = (0..WIDTH * HEIGHT)
            .map(|index| {
                if ((index % WIDTH) as usize) < edge {
                    [0.0, 0.0, 0.0, 1.0]
                } else {
                    [1.0, 1.0, 1.0, 1.0]
                }
            })
            .collect();
        assert!(run(
            &mut frame,
            WIDTH,
            HEIGHT,
            &solid([1.0, 1.0, 1.0, 1.0], WIDTH, HEIGHT),
            WIDTH,
            HEIGHT,
            grade(8.0, CompoundBlurSizeMode::Stretch, false),
        ));
        let row = 16 * WIDTH as usize;
        let coverage = |column: usize| frame[row + column][0];
        assert_eq!(coverage(0), 0.0, "far left stays black");
        assert_eq!(coverage(WIDTH as usize - 1), 1.0, "far right stays white");
        // Every pixel next to the edge is a proper average, not a copy, and the
        // two sides stay balanced because the disc taps are quasi-uniform.
        let left = coverage(edge - 1);
        let right = coverage(edge);
        assert!(left > 0.05 && left < 0.95, "left of the edge: {left}");
        assert!(right > 0.05 && right < 0.95, "right of the edge: {right}");
        assert!(
            (left - (1.0 - right)).abs() < 0.2,
            "the edge profile is roughly symmetric: {left} vs {right}"
        );
        let mut previous = 0.0_f32;
        for column in 0..WIDTH as usize {
            let current = coverage(column);
            assert!(current >= previous, "column {column} must not darken");
            previous = current;
        }
    }

    #[test]
    fn alpha_is_blurred_with_the_picture() {
        const WIDTH: u32 = 32;
        const HEIGHT: u32 = 16;
        let mut frame: Vec<[f32; 4]> = (0..WIDTH * HEIGHT)
            .map(|index| {
                if ((index % WIDTH) as usize) < WIDTH as usize / 2 {
                    [1.0, 1.0, 1.0, 1.0]
                } else {
                    // Straight white at zero coverage premultiplies to black.
                    [1.0, 1.0, 1.0, 0.0]
                }
            })
            .collect();
        assert!(run(
            &mut frame,
            WIDTH,
            HEIGHT,
            &solid([1.0, 1.0, 1.0, 1.0], WIDTH, HEIGHT),
            WIDTH,
            HEIGHT,
            grade(6.0, CompoundBlurSizeMode::Stretch, false),
        ));
        let row = 8 * WIDTH as usize;
        let mut softened = false;
        for column in 0..WIDTH as usize {
            let pixel = frame[row + column];
            let coverage = pixel[3];
            assert!((0.0..=1.0).contains(&coverage), "coverage stays a fraction");
            assert_close(
                pixel[0],
                coverage,
                "premultiplied white keeps rgb equal to alpha",
            );
            if coverage > 0.0 && coverage < 1.0 {
                softened = true;
            }
        }
        assert!(softened, "the coverage edge is blurred, not hard");
        assert_close(frame[row][3], 1.0, "far left stays opaque");
        assert_close(
            frame[row + WIDTH as usize - 1][3],
            0.0,
            "far right stays empty",
        );
    }

    #[test]
    fn zero_coverage_normalises_to_transparent_black() {
        const WIDTH: u32 = 8;
        const HEIGHT: u32 = 8;
        let mut frame = solid([0.5, 0.25, 0.75, 0.0], WIDTH, HEIGHT);
        assert!(run(
            &mut frame,
            WIDTH,
            HEIGHT,
            &solid([1.0, 1.0, 1.0, 1.0], WIDTH, HEIGHT),
            WIDTH,
            HEIGHT,
            grade(4.0, CompoundBlurSizeMode::Stretch, false),
        ));
        for (index, pixel) in frame.iter().enumerate() {
            assert_eq!(*pixel, [0.0; 4], "pixel {index}");
        }
    }

    #[test]
    fn center_crop_outside_the_window_is_the_identity() {
        // The primary is square, the map is twice as wide, so the fit window only
        // covers the middle half of the map horizontally and the left/right
        // quarters of the frame read transparent.
        const WIDTH: u32 = 32;
        const HEIGHT: u32 = 32;
        let mut frame: Vec<[f32; 4]> = (0..WIDTH * HEIGHT)
            .map(|index| {
                let x = (index % WIDTH) as f32;
                [x / (WIDTH - 1) as f32, 0.0, 0.0, 1.0]
            })
            .collect();
        let source = frame.clone();
        let map = solid([1.0, 1.0, 1.0, 1.0], WIDTH, HEIGHT / 2);
        assert!(apply_compound_blur_f32(
            &mut frame,
            WIDTH,
            HEIGHT,
            reference(&map, WIDTH, HEIGHT / 2),
            transparent_sampling(),
            &grade(16.0, CompoundBlurSizeMode::CenterCrop, false),
        ));
        let row = 16 * WIDTH as usize;
        for column in [0_usize, 1, WIDTH as usize - 2, WIDTH as usize - 1] {
            assert_eq!(
                frame[row + column],
                source[row + column],
                "column {column} is outside the centred window"
            );
        }
        assert_ne!(
            frame[row + WIDTH as usize / 2],
            source[row + WIDTH as usize / 2],
            "the centre of the window is still blurred"
        );

        // Stretch reads the same map everywhere, so the corner is blurred.
        let mut stretched = source.clone();
        let map = solid([1.0, 1.0, 1.0, 1.0], WIDTH, HEIGHT / 2);
        assert!(apply_compound_blur_f32(
            &mut stretched,
            WIDTH,
            HEIGHT,
            reference(&map, WIDTH, HEIGHT / 2),
            transparent_sampling(),
            &grade(16.0, CompoundBlurSizeMode::Stretch, false),
        ));
        assert_ne!(
            stretched[row], source[row],
            "stretch does not build a crop window"
        );
    }

    #[test]
    fn hash_covers_every_authored_control() {
        use std::collections::hash_map::DefaultHasher;

        fn hash(grade: &CompoundBlurGrade) -> u64 {
            let mut hasher = DefaultHasher::new();
            grade.hash(&mut hasher);
            hasher.finish()
        }

        let base = grade(8.0, CompoundBlurSizeMode::Stretch, false);
        assert_ne!(
            hash(&base),
            hash(&grade(9.0, CompoundBlurSizeMode::Stretch, false))
        );
        assert_ne!(
            hash(&base),
            hash(&grade(8.0, CompoundBlurSizeMode::CenterCrop, false))
        );
        assert_ne!(
            hash(&base),
            hash(&grade(8.0, CompoundBlurSizeMode::Stretch, true))
        );
        assert_ne!(
            hash(&base),
            hash(&CompoundBlurGrade::new(7, 8.0, CompoundBlurSizeMode::Stretch, false).unwrap())
        );
        assert_eq!(
            hash(&base),
            hash(&grade(8.0, CompoundBlurSizeMode::Stretch, false))
        );
    }

    #[test]
    fn checkpoint_can_interrupt_the_kernel() {
        const WIDTH: u32 = 16;
        const HEIGHT: u32 = 16;
        let mut frame = solid([0.5, 0.5, 0.5, 1.0], WIDTH, HEIGHT);
        let map = solid([1.0, 1.0, 1.0, 1.0], WIDTH, HEIGHT);
        let mut remaining = 2;
        let result = apply_compound_blur_controlled(
            &mut frame,
            WIDTH,
            HEIGHT,
            reference(&map, WIDTH, HEIGHT),
            sampling(),
            &grade(4.0, CompoundBlurSizeMode::Stretch, false),
            &mut || {
                if remaining == 0 {
                    return Err(TestInterrupt);
                }
                remaining -= 1;
                Ok(())
            },
        );
        assert_eq!(result, Err(TestInterrupt));
    }

    #[derive(Debug, PartialEq, Eq)]
    struct TestInterrupt;

    /// Both dims and the map have to agree with the buffers.
    #[test]
    fn inconsistent_buffers_are_rejected() {
        let mut frame = solid([0.0, 0.0, 0.0, 1.0], 4, 4);
        let map = solid([1.0, 1.0, 1.0, 1.0], 4, 4);
        assert!(!apply_compound_blur_f32(
            &mut frame,
            3,
            4,
            reference(&map, 4, 4),
            sampling(),
            &grade(4.0, CompoundBlurSizeMode::Stretch, false),
        ));
        assert!(!apply_compound_blur_f32(
            &mut frame,
            4,
            3,
            reference(&map, 4, 4),
            sampling(),
            &grade(4.0, CompoundBlurSizeMode::Stretch, false),
        ));
        assert!(!compound_blur_frame_f32(
            &mut frame,
            usize::MAX,
            4,
            reference(&map, 4, 4),
            sampling(),
            &grade(4.0, CompoundBlurSizeMode::Stretch, false),
        ));
    }

    #[test]
    fn row_band_parallelism_matches_a_serial_reference() {
        // Large enough that `for_each_row_band` splits across workers, so this
        // compares the parallel kernel with a straight serial evaluation of the
        // documented formula.
        const WIDTH: u32 = 37;
        const HEIGHT: u32 = 21;
        let map: Vec<[f32; 4]> = (0..WIDTH * HEIGHT)
            .map(|index| {
                let x = (index % WIDTH) as f32 / (WIDTH - 1) as f32;
                [x, 1.0 - x, 0.5, 1.0]
            })
            .collect();
        let frame: Vec<[f32; 4]> = (0..WIDTH * HEIGHT)
            .map(|index| {
                let x = (index % WIDTH) as f32 / (WIDTH - 1) as f32;
                let y = (index / WIDTH) as f32 / (HEIGHT - 1) as f32;
                [x, y, x * y, 1.0]
            })
            .collect();
        let settings = grade(9.0, CompoundBlurSizeMode::Stretch, false);
        let mut parallel = frame.clone();
        assert!(run(
            &mut parallel,
            WIDTH,
            HEIGHT,
            &map,
            WIDTH,
            HEIGHT,
            settings,
        ));

        // The serial reference evaluates the same formula pixel by pixel.
        let serial = serial_reference(&frame, WIDTH, HEIGHT, &map, settings);
        for (index, (parallel, serial)) in parallel.iter().zip(&serial).enumerate() {
            assert_pixel_close(*parallel, *serial, &format!("pixel {index}"));
        }
    }

    fn serial_reference(
        frame: &[[f32; 4]],
        width: u32,
        height: u32,
        map_pixels: &[[f32; 4]],
        grade: CompoundBlurGrade,
    ) -> Vec<[f32; 4]> {
        let extent = EffectFrameExtent::new(width, height);
        let source: Vec<[f32; 4]> = frame.iter().map(|pixel| premultiply(*pixel)).collect();
        let map = reference(map_pixels, width, height);
        let mut output = Vec::with_capacity(source.len());
        for y in 0..height {
            for x in 0..width {
                let uv = compound_blur_reference_uv(
                    x,
                    y,
                    width,
                    height,
                    map.transform,
                    extent,
                    AuxiliaryCoordinateMapping::PrimaryGrid,
                    grade.size_mode(),
                );
                let sample = sample_auxiliary_f32(map, uv, sampling());
                let mut luminance =
                    (0.2126 * sample[0] + 0.7152 * sample[1] + 0.0722 * sample[2]).clamp(0.0, 1.0);
                if grade.invert_blur() {
                    luminance = 1.0 - luminance;
                }
                let radius = grade.max_blur() * luminance;
                let centre = source[y as usize * width as usize + x as usize];
                if radius < 0.5 {
                    output.push(centre);
                    continue;
                }
                let mut sum = [0.0_f32; 4];
                for tap in 0..=COMPOUND_BLUR_TAPS {
                    let (distance, angle) = if tap == 0 {
                        (0.0, 0.0)
                    } else {
                        (
                            radius * (tap as f32 / COMPOUND_BLUR_TAPS as f32).sqrt(),
                            0.5 + tap as f32 * COMPOUND_BLUR_GOLDEN_ANGLE,
                        )
                    };
                    let tap = crate::resample::sample_bilinear_clamped(
                        &source,
                        width as usize,
                        height as usize,
                        x as f64 + f64::from(distance * angle.cos()),
                        y as f64 + f64::from(distance * angle.sin()),
                    );
                    for (channel, value) in sum.iter_mut().enumerate() {
                        *value += tap[channel];
                    }
                }
                let count = (COMPOUND_BLUR_TAPS + 1) as f32;
                let average = [
                    sum[0] / count,
                    sum[1] / count,
                    sum[2] / count,
                    sum[3] / count,
                ];
                output.push(if average[3] <= 0.0 { [0.0; 4] } else { average });
            }
        }
        output
    }

    fn premultiply(pixel: [f32; 4]) -> [f32; 4] {
        let alpha = pixel[3].clamp(0.0, 1.0);
        [pixel[0] * alpha, pixel[1] * alpha, pixel[2] * alpha, alpha]
    }
}
