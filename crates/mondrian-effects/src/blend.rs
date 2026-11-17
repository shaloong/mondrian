//! Blend (CHN-02): composites the primary layer with a second authored layer.
//!
//! The authored controls are the auxiliary layer slot, one of the five Adobe
//! documented modes, how strongly the result is mixed back with the original
//! image, and how a differently sized layer is fitted onto the primary frame.
//!
//! The auxiliary raster is straight coverage, while the primary working buffer
//! is premultiplied. The kernel unpremultiplies the primary pixel, folds the
//! layer sample over it (`alt = mix(C_s, C_a, a_a)`), applies the mode, mixes
//! the result back with the original image and premultiplies the output.

use std::hash::{Hash, Hasher};

use crate::auxiliary::{sample_auxiliary_f32, AuxiliaryFloatFrame};
use crate::graph::{AuxiliaryCoordinateMapping, AuxiliarySampling};

/// Lowest accepted auxiliary layer slot; `0` means no second layer.
pub const BLEND_MIN_LAYER: u32 = 0;
/// Highest accepted auxiliary layer slot.
pub const BLEND_MAX_LAYER: u32 = 19;
/// Documented default auxiliary layer slot.
pub const DEFAULT_LAYER: u32 = 0;
/// Documented default mix with the original image.
pub const DEFAULT_BLEND_WITH_ORIGINAL: f32 = 1.0;
/// Scene-linear Rec. 709 luma weights.
pub const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];
/// Chroma below which a primary pixel counts as gray.
pub const CHROMA_EPSILON: f32 = 1e-6;
/// Luma below which the layer cannot supply a colour ratio.
pub const LUMA_EPSILON: f32 = 1e-6;
/// Checkpoint cadence of the CPU kernel, in pixels.
const CHECKPOINT_STRIDE: usize = 4_096;

/// One of the five documented blend modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BlendMode {
    /// Colour the primary with the layer colour while keeping its luminance.
    ColorOnly,
    /// Like [`Self::ColorOnly`], but only for pixels that already carry chroma.
    TintOnly,
    /// Take the per-channel minimum of the primary and the layer.
    DarkenOnly,
    /// Take the per-channel maximum of the primary and the layer.
    LightenOnly,
    /// Fade the primary out as the layer fades in.
    Crossfade,
}

impl BlendMode {
    /// Every authored mode, in menu order.
    pub const ALL: [Self; 5] = [
        Self::ColorOnly,
        Self::TintOnly,
        Self::DarkenOnly,
        Self::LightenOnly,
        Self::Crossfade,
    ];

    /// Stable menu code carried in the GPU uniform block.
    pub const fn code(self) -> u32 {
        match self {
            Self::ColorOnly => 1,
            Self::TintOnly => 2,
            Self::DarkenOnly => 3,
            Self::LightenOnly => 4,
            Self::Crossfade => 5,
        }
    }

    /// Inverse of [`Self::code`].
    pub const fn from_code(code: u32) -> Option<Self> {
        match code {
            1 => Some(Self::ColorOnly),
            2 => Some(Self::TintOnly),
            3 => Some(Self::DarkenOnly),
            4 => Some(Self::LightenOnly),
            5 => Some(Self::Crossfade),
            _ => None,
        }
    }

    /// Stable schema key.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::ColorOnly => "color_only",
            Self::TintOnly => "tint_only",
            Self::DarkenOnly => "darken_only",
            Self::LightenOnly => "lighten_only",
            Self::Crossfade => "crossfade",
        }
    }

    /// Inverse of [`Self::as_key`].
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "color_only" => Some(Self::ColorOnly),
            "tint_only" => Some(Self::TintOnly),
            "darken_only" => Some(Self::DarkenOnly),
            "lighten_only" => Some(Self::LightenOnly),
            "crossfade" => Some(Self::Crossfade),
            _ => None,
        }
    }

    /// Localized menu label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::ColorOnly => "仅颜色",
            Self::TintOnly => "仅色调",
            Self::DarkenOnly => "仅变暗",
            Self::LightenOnly => "仅变亮",
            Self::Crossfade => "交叉淡化",
        }
    }
}

/// How a differently sized layer is fitted onto the primary frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BlendSizeMode {
    /// Stretch the layer over the whole primary frame.
    Stretch,
    /// Preserve the layer aspect ratio and crop it to the primary frame.
    CenterCrop,
}

impl BlendSizeMode {
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

/// Why a set of authored blend controls was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlendError {
    parameter: &'static str,
}

impl BlendError {
    /// Authored parameter that produced this rejection.
    pub const fn parameter(self) -> &'static str {
        self.parameter
    }
}

impl std::fmt::Display for BlendError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "blend.{} must be finite and within its documented range",
            self.parameter
        )
    }
}

impl std::error::Error for BlendError {}

/// Authored blend controls.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlendGrade {
    layer: u32,
    mode: BlendMode,
    blend_with_original: f32,
    size_mode: BlendSizeMode,
}

impl BlendGrade {
    /// Validate authored controls.
    pub fn new(
        layer: u32,
        mode: BlendMode,
        blend_with_original: f32,
        size_mode: BlendSizeMode,
    ) -> Result<Self, BlendError> {
        if !(BLEND_MIN_LAYER..=BLEND_MAX_LAYER).contains(&layer) {
            return Err(BlendError { parameter: "layer" });
        }
        if !blend_with_original.is_finite() || !(0.0..=1.0).contains(&blend_with_original) {
            return Err(BlendError { parameter: "blend_with_original" });
        }
        Ok(Self { layer, mode, blend_with_original, size_mode })
    }

    /// Controls a freshly authored effect starts with.
    pub const fn documented_defaults() -> Self {
        Self {
            layer: DEFAULT_LAYER,
            mode: BlendMode::ColorOnly,
            blend_with_original: DEFAULT_BLEND_WITH_ORIGINAL,
            size_mode: BlendSizeMode::Stretch,
        }
    }

    /// Auxiliary layer slot that supplies the second layer.
    pub const fn layer(self) -> u32 {
        self.layer
    }

    /// Documented mode applied to the two layers.
    pub const fn mode(self) -> BlendMode {
        self.mode
    }

    /// Strength of the mode against the untouched original image.
    pub const fn blend_with_original(self) -> f32 {
        self.blend_with_original
    }

    /// How the layer is fitted onto the primary frame.
    pub const fn size_mode(self) -> BlendSizeMode {
        self.size_mode
    }

    /// Mix with the original image in the normalized `0..=1` kernel domain.
    pub const fn blend_with_original_normalized(self) -> f32 {
        self.blend_with_original
    }

    /// Packed controls shared with the GPU spatial pass.
    ///
    /// `params = [mode.code(), blend_with_original, size_mode.code(), 0]` — the
    /// fourth lane is reserved: the renderer overwrites it with the linear
    /// filter flag.
    ///
    /// `extra = [layer, 0, 0, 0]`.
    pub fn gpu_controls(self) -> ([f32; 4], [f32; 4]) {
        (
            [
                self.mode.code() as f32,
                self.blend_with_original_normalized(),
                self.size_mode.code() as f32,
                0.0,
            ],
            [self.layer as f32, 0.0, 0.0, 0.0],
        )
    }

    /// Hash every authored control into the render operation's signature.
    pub fn hash_state<H: Hasher>(&self, state: &mut H) {
        self.layer.hash(state);
        self.mode.hash(state);
        self.blend_with_original.to_bits().hash(state);
        self.size_mode.hash(state);
    }

    /// Blend one premultiplied primary pixel with one straight layer sample.
    ///
    /// The result is premultiplied, matching the working buffer.
    pub fn apply_pixel_from(
        &self,
        primary_premultiplied: [f32; 4],
        secondary_straight: [f32; 4],
    ) -> [f32; 4] {
        let alpha_primary = primary_premultiplied[3];
        if alpha_primary <= 0.0 {
            return [0.0; 4];
        }
        let color_primary = [
            primary_premultiplied[0] / alpha_primary,
            primary_premultiplied[1] / alpha_primary,
            primary_premultiplied[2] / alpha_primary,
        ];
        let alpha_layer = secondary_straight[3].clamp(0.0, 1.0);
        let color_layer = [
            secondary_straight[0],
            secondary_straight[1],
            secondary_straight[2],
        ];
        let alt = mix_straight(color_primary, color_layer, alpha_layer);
        let (color_out, alpha_out) = match self.mode {
            BlendMode::ColorOnly => (
                colorized(color_primary, color_layer, alpha_layer),
                alpha_primary,
            ),
            BlendMode::TintOnly => {
                if chroma(color_primary) > CHROMA_EPSILON {
                    (
                        colorized(color_primary, color_layer, alpha_layer),
                        alpha_primary,
                    )
                } else {
                    (color_primary, alpha_primary)
                }
            }
            BlendMode::DarkenOnly => (
                [
                    color_primary[0].min(alt[0]),
                    color_primary[1].min(alt[1]),
                    color_primary[2].min(alt[2]),
                ],
                alpha_primary,
            ),
            BlendMode::LightenOnly => (
                [
                    color_primary[0].max(alt[0]),
                    color_primary[1].max(alt[1]),
                    color_primary[2].max(alt[2]),
                ],
                alpha_primary,
            ),
            BlendMode::Crossfade => (alt, alpha_layer + alpha_primary * (1.0 - alpha_layer)),
        };
        // `1.0` means "show the original image": the default is the identity.
        let mix = self.blend_with_original.clamp(0.0, 1.0);
        let color_final = mix_straight(color_out, color_primary, mix);
        let alpha_final = (alpha_out * (1.0 - mix) + alpha_primary * mix).clamp(0.0, 1.0);
        if alpha_final <= 0.0 {
            return [0.0; 4];
        }
        [
            color_final[0] * alpha_final,
            color_final[1] * alpha_final,
            color_final[2] * alpha_final,
            alpha_final,
        ]
    }
}

impl Hash for BlendGrade {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.hash_state(state);
    }
}

/// Scene-linear Rec. 709 luma of a straight colour.
#[inline]
fn luma(color: [f32; 3]) -> f32 {
    LUMA[0] * color[0] + LUMA[1] * color[1] + LUMA[2] * color[2]
}

/// Maximum minus minimum of a straight colour.
#[inline]
fn chroma(color: [f32; 3]) -> f32 {
    color[0].max(color[1]).max(color[2]) - color[0].min(color[1]).min(color[2])
}

/// Straight linear interpolation `from + (to - from) * amount`.
#[inline]
fn mix_straight(from: [f32; 3], to: [f32; 3], amount: f32) -> [f32; 3] {
    [
        from[0] + (to[0] - from[0]) * amount,
        from[1] + (to[1] - from[1]) * amount,
        from[2] + (to[2] - from[2]) * amount,
    ]
}

/// Colour-only tint: keep the primary luminance, take the layer colour.
#[inline]
fn colorized(color_primary: [f32; 3], color_layer: [f32; 3], alpha_layer: f32) -> [f32; 3] {
    let luma_primary = luma(color_primary);
    let luma_layer = luma(color_layer);
    let tinted = if luma_layer <= LUMA_EPSILON {
        [luma_primary; 3]
    } else {
        let scale = luma_primary / luma_layer;
        [
            color_layer[0] * scale,
            color_layer[1] * scale,
            color_layer[2] * scale,
        ]
    };
    mix_straight(color_primary, tinted, alpha_layer)
}

/// Reference uv for one primary pixel centre under the authored geometry.
///
/// The delivery affine is applied first (only when the sampling declaration is
/// `FrameTransform`), then the size mode fits the layer aspect ratio into the
/// primary frame. The kernel samples the result on the primary grid, so the
/// affine is never applied twice.
pub(crate) fn blend_reference_uv(
    x: u32,
    y: u32,
    frame_width: u32,
    frame_height: u32,
    transform: [f32; 6],
    extent: crate::EffectFrameExtent,
    sampling_mapping: AuxiliaryCoordinateMapping,
    size_mode: BlendSizeMode,
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
    if size_mode == BlendSizeMode::CenterCrop {
        let primary_aspect = frame_width as f32 / frame_height as f32;
        let reference_aspect = extent.width() as f32 / extent.height() as f32;
        let crop_h = (reference_aspect / primary_aspect).min(1.0);
        let crop_w = (primary_aspect / reference_aspect).min(1.0);
        uv = [0.5 + (uv[0] - 0.5) / crop_w, 0.5 + (uv[1] - 0.5) / crop_h];
    }
    uv
}

/// Blend a Float32 frame with a second layer, with a cooperative checkpoint.
///
/// Returns `Ok(false)` when the buffers are inconsistent, and `Ok(true)` after
/// writing the whole frame.
pub fn apply_blend_controlled<E>(
    primary: &mut [[f32; 4]],
    width: u32,
    height: u32,
    reference: AuxiliaryFloatFrame<'_>,
    sampling: AuxiliarySampling,
    grade: &BlendGrade,
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

    // The reference uv already contains the delivery affine and the size-mode
    // crop, so the sampler must read it on the primary grid.
    let grid_sampling = AuxiliarySampling::new(
        AuxiliaryCoordinateMapping::PrimaryGrid,
        sampling.filter,
        sampling.out_of_bounds,
    );
    let size_mode = grade.size_mode();
    for y in 0..height {
        let row = y as usize * width as usize;
        for x in 0..width {
            let index = row + x as usize;
            let uv = blend_reference_uv(
                x,
                y,
                width,
                height,
                reference.transform,
                extent,
                sampling.mapping,
                size_mode,
            );
            let layer = sample_auxiliary_f32(reference, uv, grid_sampling);
            primary[index] = grade.apply_pixel_from(primary[index], layer);
            if index.is_multiple_of(CHECKPOINT_STRIDE) {
                checkpoint()?;
            }
        }
    }
    checkpoint()?;
    Ok(true)
}

/// Blend one Float32 frame with a second layer.
///
/// Returns `false` when the buffers are inconsistent.
pub fn apply_blend_f32(
    primary: &mut [[f32; 4]],
    width: u32,
    height: u32,
    reference: AuxiliaryFloatFrame<'_>,
    sampling: AuxiliarySampling,
    grade: &BlendGrade,
) -> bool {
    apply_blend_controlled(
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
        mode: BlendMode,
        blend_with_original: f32,
        size_mode: BlendSizeMode,
    ) -> BlendGrade {
        BlendGrade::new(layer, mode, blend_with_original, size_mode).unwrap()
    }

    fn mode_grade(mode: BlendMode) -> BlendGrade {
        grade(6, mode, 0.0, BlendSizeMode::Stretch)
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
        settings: BlendGrade,
    ) -> bool {
        apply_blend_f32(
            primary,
            width,
            height,
            reference(pixels, aux_width, aux_height),
            AuxiliarySampling::FRAME_TRANSFORM_LINEAR_CLAMP,
            &settings,
        )
    }

    fn assert_pixel(actual: [f32; 4], expected: [f32; 4]) {
        for (channel, (actual, expected)) in actual.iter().zip(expected).enumerate() {
            assert!(
                (actual - expected).abs() < 1e-5,
                "channel {channel}: {actual} != {expected}"
            );
        }
    }

    #[test]
    fn modes_round_trip_through_their_codes_and_keys() {
        for (index, mode) in BlendMode::ALL.into_iter().enumerate() {
            assert_eq!(mode.code(), index as u32 + 1);
            assert_eq!(BlendMode::from_code(mode.code()), Some(mode));
            assert_eq!(BlendMode::from_key(mode.as_key()), Some(mode));
            assert!(!mode.label().is_empty());
        }
        assert_eq!(BlendMode::ColorOnly.as_key(), "color_only");
        assert_eq!(BlendMode::ColorOnly.label(), "仅颜色");
        assert_eq!(BlendMode::Crossfade.label(), "交叉淡化");
        assert!(BlendMode::from_code(0).is_none());
        assert!(BlendMode::from_key("overlay").is_none());
    }

    #[test]
    fn size_modes_round_trip_through_their_codes_and_keys() {
        for (index, mode) in BlendSizeMode::ALL.into_iter().enumerate() {
            assert_eq!(mode.code(), index as u32 + 1);
            assert_eq!(BlendSizeMode::from_code(mode.code()), Some(mode));
            assert_eq!(BlendSizeMode::from_key(mode.as_key()), Some(mode));
            assert!(!mode.label().is_empty());
        }
        assert_eq!(BlendSizeMode::Stretch.as_key(), "stretch");
        assert_eq!(BlendSizeMode::CenterCrop.as_key(), "center_crop");
        assert_eq!(BlendSizeMode::Stretch.label(), "拉伸适配");
        assert_eq!(BlendSizeMode::CenterCrop.label(), "中心裁剪");
        assert!(BlendSizeMode::from_code(0).is_none());
        assert!(BlendSizeMode::from_key("cover").is_none());
    }

    #[test]
    fn documented_defaults_are_the_identity() {
        let defaults = BlendGrade::documented_defaults();
        assert_eq!(defaults.layer(), 0);
        assert_eq!(defaults.mode(), BlendMode::ColorOnly);
        assert_eq!(defaults.blend_with_original(), 1.0);
        assert_eq!(defaults.size_mode(), BlendSizeMode::Stretch);
        assert_eq!(defaults.blend_with_original_normalized(), 1.0);
        assert_eq!(
            defaults.gpu_controls(),
            ([1.0, 1.0, 1.0, 0.0], [0.0, 0.0, 0.0, 0.0])
        );

        let layer = [0.9, 0.1, 0.1, 1.0];
        assert_pixel(
            defaults.apply_pixel_from([0.6, 0.3, 0.2, 1.0], layer),
            [0.6, 0.3, 0.2, 1.0],
        );
        assert_pixel(
            defaults.apply_pixel_from([0.3, 0.15, 0.1, 0.5], layer),
            [0.3, 0.15, 0.1, 0.5],
        );
    }

    #[test]
    fn out_of_range_controls_are_rejected() {
        for layer in [20, 21, u32::MAX] {
            let error = BlendGrade::new(layer, BlendMode::ColorOnly, 1.0, BlendSizeMode::Stretch)
                .unwrap_err();
            assert_eq!(error.parameter(), "layer");
            assert_eq!(
                error.to_string(),
                "blend.layer must be finite and within its documented range"
            );
        }
        for mix in [-1.0, 1.5, f32::NAN, f32::INFINITY] {
            let error =
                BlendGrade::new(6, BlendMode::ColorOnly, mix, BlendSizeMode::Stretch).unwrap_err();
            assert_eq!(error.parameter(), "blend_with_original");
            assert_eq!(
                error.to_string(),
                "blend.blend_with_original must be finite and within its documented range"
            );
        }
        assert!(BlendGrade::new(0, BlendMode::Crossfade, 0.0, BlendSizeMode::CenterCrop).is_ok());
        assert!(BlendGrade::new(19, BlendMode::TintOnly, 1.0, BlendSizeMode::Stretch).is_ok());
    }

    #[test]
    fn gpu_controls_pack_the_documented_layout() {
        let settings = grade(12, BlendMode::Crossfade, 0.25, BlendSizeMode::CenterCrop);
        let (params, extra) = settings.gpu_controls();
        assert_eq!(params, [5.0, 0.25, 2.0, 0.0]);
        assert_eq!(extra, [12.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn hash_covers_every_control() {
        use std::collections::hash_map::DefaultHasher;

        fn hash_of(grade: BlendGrade) -> u64 {
            let mut hasher = DefaultHasher::new();
            grade.hash(&mut hasher);
            hasher.finish()
        }

        let base = grade(6, BlendMode::ColorOnly, 0.0, BlendSizeMode::Stretch);
        assert_eq!(hash_of(base), hash_of(base));
        let variants = [
            grade(7, BlendMode::ColorOnly, 0.0, BlendSizeMode::Stretch),
            grade(6, BlendMode::TintOnly, 0.0, BlendSizeMode::Stretch),
            grade(6, BlendMode::ColorOnly, 0.5, BlendSizeMode::Stretch),
            grade(6, BlendMode::ColorOnly, 0.0, BlendSizeMode::CenterCrop),
        ];
        for variant in variants {
            assert_ne!(hash_of(base), hash_of(variant));
        }
    }

    #[test]
    fn color_only_preserves_the_primary_luminance() {
        let primary = [0.8, 0.2, 0.4, 1.0];
        let layer = [0.2, 0.6, 0.9, 1.0];
        let output = mode_grade(BlendMode::ColorOnly).apply_pixel_from(primary, layer);
        assert!((luma([output[0], output[1], output[2]]) - luma([0.8, 0.2, 0.4])).abs() < 1e-5);
        // The layer hue survives: the tinted pixel is the layer colour scaled.
        assert!(output[2] > output[0] && output[1] > output[0]);
        assert_pixel(output, {
            let scale = luma([0.8, 0.2, 0.4]) / luma([0.2, 0.6, 0.9]);
            [0.2 * scale, 0.6 * scale, 0.9 * scale, 1.0]
        });
    }

    #[test]
    fn color_only_is_the_identity_where_the_layer_is_transparent() {
        let primary = [0.8, 0.2, 0.4, 1.0];
        let output =
            mode_grade(BlendMode::ColorOnly).apply_pixel_from(primary, [1.0, 0.0, 0.0, 0.0]);
        assert_pixel(output, primary);
    }

    #[test]
    fn color_only_falls_back_to_gray_for_a_black_layer() {
        let primary = [0.8, 0.2, 0.4, 1.0];
        let output =
            mode_grade(BlendMode::ColorOnly).apply_pixel_from(primary, [0.0, 0.0, 0.0, 1.0]);
        let luma_primary = luma([0.8, 0.2, 0.4]);
        assert_pixel(output, [luma_primary, luma_primary, luma_primary, 1.0]);
    }

    #[test]
    fn tint_only_leaves_gray_pixels_alone() {
        let primary = [0.5, 0.5, 0.5, 1.0];
        let output =
            mode_grade(BlendMode::TintOnly).apply_pixel_from(primary, [0.0, 0.0, 1.0, 1.0]);
        assert_pixel(output, primary);
    }

    #[test]
    fn tint_only_colours_pixels_that_already_carry_chroma() {
        let primary = [0.8, 0.2, 0.4, 1.0];
        let layer = [0.2, 0.6, 0.9, 1.0];
        assert_pixel(
            mode_grade(BlendMode::TintOnly).apply_pixel_from(primary, layer),
            mode_grade(BlendMode::ColorOnly).apply_pixel_from(primary, layer),
        );
    }

    #[test]
    fn darken_only_takes_the_per_channel_minimum() {
        let primary = [0.8, 0.2, 0.4, 1.0];
        let layer = [0.3, 0.9, 0.1, 1.0];
        let output = mode_grade(BlendMode::DarkenOnly).apply_pixel_from(primary, layer);
        assert_pixel(output, [0.3, 0.2, 0.1, 1.0]);
    }

    #[test]
    fn lighten_only_takes_the_per_channel_maximum() {
        let primary = [0.8, 0.2, 0.4, 1.0];
        let layer = [0.3, 0.9, 0.1, 1.0];
        let output = mode_grade(BlendMode::LightenOnly).apply_pixel_from(primary, layer);
        assert_pixel(output, [0.8, 0.9, 0.4, 1.0]);
    }

    #[test]
    fn darken_and_lighten_leave_a_transparent_layer_alone() {
        let primary = [0.8, 0.2, 0.4, 1.0];
        let layer = [0.0, 1.0, 0.0, 0.0];
        assert_pixel(
            mode_grade(BlendMode::DarkenOnly).apply_pixel_from(primary, layer),
            primary,
        );
        assert_pixel(
            mode_grade(BlendMode::LightenOnly).apply_pixel_from(primary, layer),
            primary,
        );
    }

    #[test]
    fn crossfade_replaces_the_colour_and_widens_coverage() {
        // Premultiplied primary: straight [0.8, 0.2, 0.4] at coverage 0.5.
        let primary = [0.4, 0.1, 0.2, 0.5];
        let output =
            mode_grade(BlendMode::Crossfade).apply_pixel_from(primary, [0.1, 0.9, 0.3, 0.4]);
        let alpha = 0.4 + 0.5 * (1.0 - 0.4);
        let color = [
            0.8 + (0.1 - 0.8) * 0.4,
            0.2 + (0.9 - 0.2) * 0.4,
            0.4 + (0.3 - 0.4) * 0.4,
        ];
        assert_pixel(
            output,
            [color[0] * alpha, color[1] * alpha, color[2] * alpha, alpha],
        );
        assert!(output[3] > primary[3]);
    }

    #[test]
    fn blend_with_original_interpolates_and_clamps() {
        let primary = [0.8, 0.2, 0.4, 1.0];
        let layer = [0.2, 0.6, 0.9, 1.0];
        let full = mode_grade(BlendMode::Crossfade).apply_pixel_from(primary, layer);
        let half = grade(6, BlendMode::Crossfade, 0.5, BlendSizeMode::Stretch)
            .apply_pixel_from(primary, layer);
        for channel in 0..4 {
            let expected = full[channel] * 0.5 + primary[channel] * 0.5;
            assert!((half[channel] - expected).abs() < 1e-5);
        }

        // An out-of-range layer coverage is clamped to one, and the coverage of
        // the result never leaves `0..=1`.
        let clamped = grade(6, BlendMode::Crossfade, 0.25, BlendSizeMode::Stretch)
            .apply_pixel_from(primary, [0.2, 0.6, 0.9, 2.0]);
        assert!((clamped[3] - 1.0).abs() < 1e-6);
        assert!(clamped[..3].iter().all(|channel| channel.is_finite()));

        // A one-sided straight alpha at max mix is the identity.
        let untouched = grade(6, BlendMode::DarkenOnly, 1.0, BlendSizeMode::Stretch)
            .apply_pixel_from(primary, layer);
        assert_pixel(untouched, primary);
    }

    #[test]
    fn uncovered_primary_writes_zero() {
        assert_eq!(
            mode_grade(BlendMode::ColorOnly)
                .apply_pixel_from([0.5, 0.5, 0.5, 0.0], [0.2, 0.6, 0.9, 1.0]),
            [0.0; 4]
        );
        assert_eq!(
            mode_grade(BlendMode::Crossfade)
                .apply_pixel_from([-0.5, 0.0, 0.0, -0.25], [0.2, 0.6, 0.9, 1.0]),
            [0.0; 4]
        );
    }

    #[test]
    fn layer_slot_does_not_change_the_pixel_math() {
        let primary = [0.8, 0.2, 0.4, 1.0];
        let layer = [0.2, 0.6, 0.9, 1.0];
        let zero = grade(0, BlendMode::ColorOnly, 0.0, BlendSizeMode::Stretch)
            .apply_pixel_from(primary, layer);
        let top = grade(19, BlendMode::ColorOnly, 0.0, BlendSizeMode::Stretch)
            .apply_pixel_from(primary, layer);
        assert_pixel(zero, top);
    }

    #[test]
    fn stretch_reads_the_whole_reference_frame() {
        let layer = [[0.25, 0.75, 0.5, 1.0]];
        let mut primary = solid(2, 2, [0.5, 0.5, 0.5, 1.0]);
        assert!(run(
            &mut primary,
            2,
            2,
            &layer,
            1,
            1,
            mode_grade(BlendMode::Crossfade)
        ));
        for pixel in primary {
            assert_pixel(pixel, [0.25, 0.75, 0.5, 1.0]);
        }
    }

    #[test]
    fn stretch_maps_the_primary_grid_onto_the_reference_grid() {
        let layer = [[0.5, 0.5, 0.5, 1.0]];
        let frame = reference(&layer, 1, 1);
        let extent = frame.frame.extent();
        let mode = BlendSizeMode::Stretch;
        assert_eq!(
            blend_reference_uv(
                0,
                0,
                16,
                9,
                frame.transform,
                extent,
                AuxiliaryCoordinateMapping::FrameTransform,
                mode
            ),
            [0.5 / 16.0, 0.5 / 9.0]
        );
        assert_eq!(
            blend_reference_uv(
                15,
                8,
                16,
                9,
                frame.transform,
                extent,
                AuxiliaryCoordinateMapping::FrameTransform,
                mode
            ),
            [15.5 / 16.0, 8.5 / 9.0]
        );

        // A delivery affine shifts the primary centre; the primary-grid mapping
        // skips it because the host already registered that grid.
        let shifted = [1.0, 0.0, 0.25, 0.0, 1.0, 0.25];
        assert_eq!(
            blend_reference_uv(
                0,
                0,
                16,
                9,
                shifted,
                extent,
                AuxiliaryCoordinateMapping::FrameTransform,
                mode
            ),
            [0.5 / 16.0 + 0.25, 0.5 / 9.0 + 0.25]
        );
        assert_eq!(
            blend_reference_uv(
                0,
                0,
                16,
                9,
                shifted,
                extent,
                AuxiliaryCoordinateMapping::PrimaryGrid,
                mode
            ),
            [0.5 / 16.0, 0.5 / 9.0]
        );
    }

    #[test]
    fn centre_crop_maps_the_reference_aspect_into_the_primary_frame() {
        let pixels = solid(4, 3, [0.5, 0.5, 0.5, 1.0]);
        let frame = reference(&pixels, 4, 3);
        let extent = frame.frame.extent();
        let mode = BlendSizeMode::CenterCrop;
        // Primary 16x9 (Af = 1.778) against a 4x3 layer (Aa = 1.333) keeps the
        // full width and crops 25% of the layer height.
        let af: f32 = 16.0 / 9.0;
        let aa: f32 = 4.0 / 3.0;
        let crop_h = (aa / af).min(1.0);
        assert!((crop_h - 0.75).abs() < 1e-6);
        let centre = blend_reference_uv(
            8,
            4,
            16,
            9,
            frame.transform,
            extent,
            AuxiliaryCoordinateMapping::FrameTransform,
            mode,
        );
        assert!((centre[1] - 0.5).abs() < 1e-6);
        let top = blend_reference_uv(
            8,
            0,
            16,
            9,
            frame.transform,
            extent,
            AuxiliaryCoordinateMapping::FrameTransform,
            mode,
        );
        assert!((top[1] - (0.5 + (0.5 / 9.0 - 0.5) / crop_h)).abs() < 1e-6);
    }

    #[test]
    fn centre_crop_reads_outside_the_window_as_transparent() {
        let settings = grade(6, BlendMode::Crossfade, 0.0, BlendSizeMode::CenterCrop);
        // A layer far wider than the frame keeps only the middle of its width:
        // the left column maps several texels left of the window.
        let pixels = solid(8, 1, [0.2, 0.4, 0.6, 1.0]);
        let transparent = AuxiliarySampling::new(
            crate::AuxiliaryCoordinateMapping::FrameTransform,
            crate::AuxiliaryFilter::Linear,
            crate::AuxiliaryOutOfBounds::Transparent,
        );

        let mut cropped = solid(16, 9, [0.5, 0.5, 0.5, 1.0]);
        assert!(apply_blend_f32(
            &mut cropped,
            16,
            9,
            reference(&pixels, 8, 1),
            transparent,
            &settings,
        ));
        // The top-left pixel maps outside the crop window, so the transparent
        // read leaves the primary untouched.
        assert_pixel(cropped[0], [0.5, 0.5, 0.5, 1.0]);
        // The centre of the frame sits inside the window and takes the layer.
        assert_pixel(cropped[4 * 16 + 8], [0.2, 0.4, 0.6, 1.0]);

        let mut clamped = solid(16, 9, [0.5, 0.5, 0.5, 1.0]);
        assert!(apply_blend_f32(
            &mut clamped,
            16,
            9,
            reference(&pixels, 8, 1),
            AuxiliarySampling::FRAME_TRANSFORM_LINEAR_CLAMP,
            &settings,
        ));
        // Clamping repeats the border texel instead of reading transparency.
        assert_pixel(clamped[0], [0.2, 0.4, 0.6, 1.0]);
    }

    #[test]
    fn region_execution_matches_the_full_frame() {
        let width = 4u32;
        let height = 2u32;
        let primary: Vec<[f32; 4]> = (0..(width * height) as usize)
            .map(|index| {
                let value = index as f32 * 0.1;
                [value, 1.0 - value, value * 0.5, 0.5 + value * 0.05]
            })
            .collect();
        let layer: Vec<[f32; 4]> = (0..(width * height) as usize)
            .map(|index| {
                let value = index as f32 * 0.07;
                [1.0 - value, value, 0.25, 0.75]
            })
            .collect();
        let settings = grade(6, BlendMode::Crossfade, 0.25, BlendSizeMode::CenterCrop);
        let sampling = AuxiliarySampling::FRAME_TRANSFORM_LINEAR_CLAMP;

        let mut full = primary.clone();
        assert!(apply_blend_f32(
            &mut full,
            width,
            height,
            reference(&layer, width, height),
            sampling,
            &settings,
        ));

        // Retain the right-hand column of both rows.
        let region = crate::adjustment::EffectRasterRegion::new(width, height, 2, 0, 2, 2);
        let mut retained = vec![primary[2], primary[3], primary[6], primary[7]];
        let op = crate::EffectRenderOp::Blend { grade: settings };
        assert!(crate::auxiliary::apply_auxiliary_effect_f32_region(
            &mut retained,
            region,
            &op,
            reference(&layer, width, height),
            sampling,
            &mut || Ok::<(), std::convert::Infallible>(()),
        )
        .unwrap());
        assert_pixel(retained[0], full[2]);
        assert_pixel(retained[1], full[3]);
        assert_pixel(retained[2], full[6]);
        assert_pixel(retained[3], full[7]);
    }

    #[test]
    fn inconsistent_buffers_and_an_empty_reference_fail_closed() {
        let layer = solid(1, 1, [0.2, 0.4, 0.6, 1.0]);
        let mut primary = [[0.5, 0.5, 0.5, 1.0], [0.5, 0.5, 0.5, 1.0]];
        assert!(!run(
            &mut primary,
            1,
            1,
            &layer,
            1,
            1,
            mode_grade(BlendMode::ColorOnly)
        ));
        let empty: Vec<[f32; 4]> = Vec::new();
        let mut primary = [[0.5, 0.5, 0.5, 1.0]];
        assert!(!apply_blend_f32(
            &mut primary,
            1,
            1,
            reference(&empty, 0, 0),
            AuxiliarySampling::FRAME_TRANSFORM_LINEAR_CLAMP,
            &grade(6, BlendMode::ColorOnly, 0.0, BlendSizeMode::Stretch),
        ));
    }
}
