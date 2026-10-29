//! Channel-scope invert kernel for the built-in `Invert` (反转) effect.
//!
//! The effect is a single-frame, pixel-local operation. Color scopes are
//! defined on the working scene-linear RGB(A) values as `v -> 1 - v`, without
//! clamping, so HDR excursions and negative channels keep their meaning and the
//! operation stays exactly involutive.
//!
//! The HSL scopes are chroma-preserving moves on the same values:
//!
//! * `Hue` rotates the hue by half a turn while keeping lightness
//!   `(max + min) / 2` and chroma `max - min` unchanged.
//! * `Lightness` keeps chroma and reflects lightness around `0.5`
//!   (`v -> v + (1 - 2L)`).
//! * `Saturation` keeps hue and lightness and reflects HSL saturation around
//!   `0.5`. HSL saturation is only defined inside the unit cube; pixels with
//!   negative channels or a lightness outside `(0, 1)` keep their value instead
//!   of being clamped into a different color.
//!
//! `blend_with_original` is the retained fraction of the original pixel and is
//! a convex mixture by contract: `0.0` keeps the inverted result and `1.0` is
//! the identity. The graph builder does not emit an operation for `1.0`.

use serde::{Deserialize, Serialize};

use crate::hsl_space::{opposite_chroma, opposite_hue, opposite_lightness};

/// Channel scope of the `Invert` effect.
///
/// The menu carries sixteen distinct scopes: twelve RGB/Alpha combinations
/// (matching the reference channel menu, which lists the three color channels,
/// alpha, their pairs and the full combinations), the combined `Hls` scope and
/// the individual hue, lightness and saturation scopes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvertChannels {
    /// Invert the three color channels.
    Rgb,
    Red,
    Green,
    Blue,
    Alpha,
    RedGreen,
    RedBlue,
    GreenBlue,
    RedGreenAlpha,
    RedBlueAlpha,
    GreenBlueAlpha,
    RedGreenBlueAlpha,
    /// Invert hue, saturation and lightness.
    Hls,
    Hue,
    Lightness,
    Saturation,
}

impl InvertChannels {
    /// Every scope in menu order.
    pub const ALL: [Self; 16] = [
        Self::Rgb,
        Self::Red,
        Self::Green,
        Self::Blue,
        Self::Alpha,
        Self::RedGreen,
        Self::RedBlue,
        Self::GreenBlue,
        Self::RedGreenAlpha,
        Self::RedBlueAlpha,
        Self::GreenBlueAlpha,
        Self::RedGreenBlueAlpha,
        Self::Hls,
        Self::Hue,
        Self::Lightness,
        Self::Saturation,
    ];

    const RED_BIT: u8 = 0b0001;
    const GREEN_BIT: u8 = 0b0010;
    const BLUE_BIT: u8 = 0b0100;
    const ALPHA_BIT: u8 = 0b1000;

    /// Stable author-facing key. This key is part of the persisted effect
    /// instance and must not change.
    pub const fn key(self) -> &'static str {
        match self {
            Self::Rgb => "rgb",
            Self::Red => "red",
            Self::Green => "green",
            Self::Blue => "blue",
            Self::Alpha => "alpha",
            Self::RedGreen => "red_green",
            Self::RedBlue => "red_blue",
            Self::GreenBlue => "green_blue",
            Self::RedGreenAlpha => "red_green_alpha",
            Self::RedBlueAlpha => "red_blue_alpha",
            Self::GreenBlueAlpha => "green_blue_alpha",
            Self::RedGreenBlueAlpha => "red_green_blue_alpha",
            Self::Hls => "hls",
            Self::Hue => "hue",
            Self::Lightness => "lightness",
            Self::Saturation => "saturation",
        }
    }

    /// Resolve a persisted author-facing key.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|scope| scope.key() == key)
    }

    /// Display label for the parameter menu.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Rgb => "RGB",
            Self::Red => "红色",
            Self::Green => "绿色",
            Self::Blue => "蓝色",
            Self::Alpha => "Alpha",
            Self::RedGreen => "红绿",
            Self::RedBlue => "红蓝",
            Self::GreenBlue => "绿蓝",
            Self::RedGreenAlpha => "红绿 Alpha",
            Self::RedBlueAlpha => "红蓝 Alpha",
            Self::GreenBlueAlpha => "绿蓝 Alpha",
            Self::RedGreenBlueAlpha => "RGB + Alpha",
            Self::Hls => "HLS",
            Self::Hue => "色相",
            Self::Lightness => "亮度",
            Self::Saturation => "饱和度",
        }
    }

    /// Bit mask of the color/alpha channels this scope inverts.
    ///
    /// HSL scopes have no channel mask and operate on the complete color.
    pub const fn channel_mask(self) -> u8 {
        match self {
            Self::Rgb | Self::Hue | Self::Lightness | Self::Saturation | Self::Hls => 0,
            Self::Red => Self::RED_BIT,
            Self::Green => Self::GREEN_BIT,
            Self::Blue => Self::BLUE_BIT,
            Self::Alpha => Self::ALPHA_BIT,
            Self::RedGreen => Self::RED_BIT | Self::GREEN_BIT,
            Self::RedBlue => Self::RED_BIT | Self::BLUE_BIT,
            Self::GreenBlue => Self::GREEN_BIT | Self::BLUE_BIT,
            Self::RedGreenAlpha => Self::RED_BIT | Self::GREEN_BIT | Self::ALPHA_BIT,
            Self::RedBlueAlpha => Self::RED_BIT | Self::BLUE_BIT | Self::ALPHA_BIT,
            Self::GreenBlueAlpha => Self::GREEN_BIT | Self::BLUE_BIT | Self::ALPHA_BIT,
            Self::RedGreenBlueAlpha => {
                Self::RED_BIT | Self::GREEN_BIT | Self::BLUE_BIT | Self::ALPHA_BIT
            }
        }
    }

    /// Mask applied by the three color channels. `RGB` inverts all of them.
    const fn color_mask(self) -> u8 {
        if matches!(self, Self::Rgb) {
            Self::RED_BIT | Self::GREEN_BIT | Self::BLUE_BIT
        } else {
            self.channel_mask() & (Self::RED_BIT | Self::GREEN_BIT | Self::BLUE_BIT)
        }
    }

    const fn inverts_hue(self) -> bool {
        matches!(self, Self::Hue | Self::Hls)
    }

    const fn inverts_lightness(self) -> bool {
        matches!(self, Self::Lightness | Self::Hls)
    }

    const fn inverts_saturation(self) -> bool {
        matches!(self, Self::Saturation | Self::Hls)
    }

    /// Channel masks as the flat integers a pointwise GPU backend needs.
    ///
    /// Returns `(color_mask, flags)`: `color_mask` selects the RGB channels
    /// inverted with `1 - x` and `flags` carries the remaining scope in its low
    /// bits — alpha inversion, hue rotation, lightness reflection and
    /// saturation reflection, in that order. A backend that decodes these two
    /// integers reproduces [`invert_rgba_f32`] exactly, which keeps the fused
    /// GPU path and the CPU kernel driven by the same authored scope.
    pub const fn gpu_scope(self) -> (u32, u32) {
        let color_mask = self.color_mask() as u32;
        let mut flags = 0;
        if self.channel_mask() & Self::ALPHA_BIT != 0 {
            flags |= INVERT_GPU_FLAG_ALPHA;
        }
        if self.inverts_hue() {
            flags |= INVERT_GPU_FLAG_HUE;
        }
        if self.inverts_saturation() {
            flags |= INVERT_GPU_FLAG_SATURATION;
        }
        if self.inverts_lightness() {
            flags |= INVERT_GPU_FLAG_LIGHTNESS;
        }
        (color_mask, flags)
    }
}

/// GPU scope flag: invert coverage as well.
pub const INVERT_GPU_FLAG_ALPHA: u32 = 0b0001;
/// GPU scope flag: rotate hue by half a turn.
pub const INVERT_GPU_FLAG_HUE: u32 = 0b0010;
/// GPU scope flag: reflect saturation.
pub const INVERT_GPU_FLAG_SATURATION: u32 = 0b0100;
/// GPU scope flag: reflect lightness.
pub const INVERT_GPU_FLAG_LIGHTNESS: u32 = 0b1000;

/// Invert one straight-alpha, scene-linear RGBA pixel.
///
/// Pixels without positive coverage belong to no image region; callers skip
/// them before calling this kernel.
pub fn invert_rgba_f32(
    pixel: [f32; 4],
    channels: InvertChannels,
    blend_with_original: f32,
) -> [f32; 4] {
    let mut inverted = pixel;

    if channels.inverts_hue() {
        let rgb = opposite_hue([pixel[0], pixel[1], pixel[2]]);
        inverted[0] = rgb[0];
        inverted[1] = rgb[1];
        inverted[2] = rgb[2];
    }
    if channels.inverts_saturation() {
        let rgb = opposite_chroma([inverted[0], inverted[1], inverted[2]]);
        inverted[0] = rgb[0];
        inverted[1] = rgb[1];
        inverted[2] = rgb[2];
    }
    if channels.inverts_lightness() {
        let rgb = opposite_lightness([inverted[0], inverted[1], inverted[2]]);
        inverted[0] = rgb[0];
        inverted[1] = rgb[1];
        inverted[2] = rgb[2];
    }

    let mask = channels.color_mask();
    if mask & InvertChannels::RED_BIT != 0 {
        inverted[0] = 1.0 - pixel[0];
    }
    if mask & InvertChannels::GREEN_BIT != 0 {
        inverted[1] = 1.0 - pixel[1];
    }
    if mask & InvertChannels::BLUE_BIT != 0 {
        inverted[2] = 1.0 - pixel[2];
    }
    if channels.channel_mask() & InvertChannels::ALPHA_BIT != 0 {
        inverted[3] = 1.0 - pixel[3];
    } else {
        inverted[3] = pixel[3];
    }

    mix_with_original(pixel, inverted, blend_with_original)
}

fn mix_with_original(original: [f32; 4], inverted: [f32; 4], blend_with_original: f32) -> [f32; 4] {
    let retained = blend_with_original.clamp(0.0, 1.0);
    if retained == 0.0 {
        return inverted;
    }
    if retained == 1.0 {
        return original;
    }
    let mut result = [0.0; 4];
    for (index, slot) in result.iter_mut().enumerate() {
        *slot = inverted[index] * (1.0 - retained) + original[index] * retained;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inverted(pixel: [f32; 4], channels: InvertChannels) -> [f32; 4] {
        invert_rgba_f32(pixel, channels, 0.0)
    }

    fn assert_pixel(actual: [f32; 4], expected: [f32; 4]) {
        for index in 0..4 {
            assert!(
                (actual[index] - expected[index]).abs() < 1.0e-6,
                "channel {index}: actual {actual:?} expected {expected:?}"
            );
        }
    }

    fn lightness(rgb: [f32; 3]) -> f32 {
        let (high, low) = crate::hsl_space::extremes(rgb);
        (high + low) * 0.5
    }

    fn chroma(rgb: [f32; 3]) -> f32 {
        let (high, low) = crate::hsl_space::extremes(rgb);
        high - low
    }

    #[test]
    fn channel_scope_keys_are_unique_and_round_trip() {
        let mut keys: Vec<&str> = InvertChannels::ALL.iter().map(|scope| scope.key()).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), InvertChannels::ALL.len());
        for scope in InvertChannels::ALL {
            assert_eq!(InvertChannels::from_key(scope.key()), Some(scope));
        }
        assert_eq!(InvertChannels::from_key("unknown"), None);
    }

    #[test]
    fn rgb_scope_inverts_only_color_channels() {
        assert_pixel(
            inverted([0.25, 0.5, 0.75, 1.0], InvertChannels::Rgb),
            [0.75, 0.5, 0.25, 1.0],
        );
        assert_pixel(
            inverted([0.25, 0.5, 0.75, 0.4], InvertChannels::RedGreen),
            [0.75, 0.5, 0.75, 0.4],
        );
    }

    #[test]
    fn hdr_and_negative_values_are_not_clamped() {
        assert_pixel(
            inverted([2.0, -0.5, 0.25, 1.0], InvertChannels::Rgb),
            [-1.0, 1.5, 0.75, 1.0],
        );
    }

    #[test]
    fn color_scopes_are_involutive() {
        let pixel = [0.125, 0.5, 1.75, 0.5];
        for scope in [
            InvertChannels::Rgb,
            InvertChannels::Red,
            InvertChannels::Blue,
            InvertChannels::RedGreenAlpha,
            InvertChannels::RedGreenBlueAlpha,
        ] {
            assert_pixel(inverted(inverted(pixel, scope), scope), pixel);
        }
    }

    #[test]
    fn alpha_scope_inverts_coverage_and_keeps_color() {
        assert_pixel(
            inverted([0.2, 0.4, 0.6, 0.25], InvertChannels::Alpha),
            [0.2, 0.4, 0.6, 0.75],
        );
        assert_pixel(
            inverted([0.2, 0.4, 0.6, 0.25], InvertChannels::RedGreenBlueAlpha),
            [0.8, 0.6, 0.4, 0.75],
        );
    }

    #[test]
    fn blend_with_original_is_a_convex_mixture() {
        let pixel = [0.0, 0.25, 0.5, 1.0];
        assert_pixel(
            invert_rgba_f32(pixel, InvertChannels::Rgb, 0.0),
            [1.0, 0.75, 0.5, 1.0],
        );
        assert_pixel(
            invert_rgba_f32(pixel, InvertChannels::Rgb, 0.5),
            [0.5, 0.5, 0.5, 1.0],
        );
        assert_pixel(invert_rgba_f32(pixel, InvertChannels::Rgb, 1.0), pixel);
        // The mixture is convex by contract: author values outside the unit
        // interval do not extrapolate.
        assert_pixel(invert_rgba_f32(pixel, InvertChannels::Rgb, 4.0), pixel);
        assert_pixel(
            invert_rgba_f32(pixel, InvertChannels::Rgb, -3.0),
            [1.0, 0.75, 0.5, 1.0],
        );
    }

    #[test]
    fn hue_scope_rotates_half_a_turn_and_keeps_lightness_and_chroma() {
        assert_pixel(
            inverted([1.0, 0.0, 0.0, 1.0], InvertChannels::Hue),
            [0.0, 1.0, 1.0, 1.0],
        );
        let pixel = [0.2, 0.45, 0.9, 0.5];
        let result = inverted(pixel, InvertChannels::Hue);
        assert!(
            (lightness([result[0], result[1], result[2]])
                - lightness([pixel[0], pixel[1], pixel[2]]))
            .abs()
                < 1.0e-6
        );
        assert!(
            (chroma([result[0], result[1], result[2]]) - chroma([pixel[0], pixel[1], pixel[2]]))
                .abs()
                < 1.0e-6
        );
        assert_eq!(result[3], pixel[3]);
        // A neutral pixel has no hue to rotate.
        assert_pixel(
            inverted([0.4, 0.4, 0.4, 1.0], InvertChannels::Hue),
            [0.4, 0.4, 0.4, 1.0],
        );
    }

    #[test]
    fn hue_scope_is_involutive() {
        let pixel = [0.2, 0.45, 0.9, 1.0];
        let twice = inverted(inverted(pixel, InvertChannels::Hue), InvertChannels::Hue);
        for index in 0..3 {
            assert!((twice[index] - pixel[index]).abs() < 1.0e-5);
        }
    }

    #[test]
    fn lightness_scope_reflects_lightness_and_keeps_chroma() {
        assert_pixel(
            inverted([0.2, 0.6, 1.0, 1.0], InvertChannels::Lightness),
            [0.0, 0.4, 0.8, 1.0],
        );
        assert_pixel(
            inverted([0.1, 0.35, 0.6, 1.0], InvertChannels::Lightness),
            [0.4, 0.65, 0.9, 1.0],
        );
    }

    #[test]
    fn saturation_scope_reflects_saturation() {
        // Chroma 0.4 at lightness 0.6 is exactly half of the available range.
        assert_pixel(
            inverted([0.8, 0.4, 0.4, 1.0], InvertChannels::Saturation),
            [0.8, 0.4, 0.4, 1.0],
        );
        let pixel = [0.6, 0.5, 0.5, 1.0];
        let result = inverted(pixel, InvertChannels::Saturation);
        assert!((lightness([result[0], result[1], result[2]]) - 0.55).abs() < 1.0e-6);
        assert!((chroma([result[0], result[1], result[2]]) - 0.8).abs() < 1.0e-6);
        let twice = inverted(result, InvertChannels::Saturation);
        for index in 0..3 {
            assert!(
                (twice[index] - pixel[index]).abs() < 1.0e-5,
                "{twice:?} vs {pixel:?}"
            );
        }
    }

    #[test]
    fn saturation_scope_keeps_neutral_and_out_of_domain_pixels() {
        assert_pixel(
            inverted([0.4, 0.4, 0.4, 1.0], InvertChannels::Saturation),
            [0.4, 0.4, 0.4, 1.0],
        );
        // HSL saturation is undefined above the unit cube peak.
        assert_pixel(
            inverted([2.0, 0.5, 0.5, 1.0], InvertChannels::Saturation),
            [2.0, 0.5, 0.5, 1.0],
        );
    }

    #[test]
    fn hls_scope_inverts_all_three_components_and_is_involutive() {
        let pixel = [0.2, 0.45, 0.9, 0.5];
        let once = inverted(pixel, InvertChannels::Hls);
        assert_ne!(once, pixel);
        assert_eq!(once[3], pixel[3]);
        let twice = inverted(once, InvertChannels::Hls);
        for index in 0..3 {
            assert!(
                (twice[index] - pixel[index]).abs() < 1.0e-5,
                "{twice:?} vs {pixel:?}"
            );
        }
    }

    #[test]
    fn hsl_scopes_leave_channels_outside_their_scope_untouched() {
        let pixel = [0.25, 0.5, 0.75, 0.5];
        for scope in [
            InvertChannels::Hue,
            InvertChannels::Lightness,
            InvertChannels::Saturation,
        ] {
            assert_eq!(inverted(pixel, scope)[3], 0.5);
        }
    }

    #[test]
    fn packed_scope_drives_the_same_result_as_the_enum_scope() {
        // Mirror of the documented GPU contract: a backend that only receives
        // the packed `(color_mask, flags)` pair must reproduce the CPU kernel.
        fn decoded(
            pixel: [f32; 4],
            (color_mask, flags): (u32, u32),
            blend_with_original: f32,
        ) -> [f32; 4] {
            let mut inverted = pixel;
            if flags & INVERT_GPU_FLAG_HUE != 0 {
                let rgb = opposite_hue([pixel[0], pixel[1], pixel[2]]);
                inverted[0..3].copy_from_slice(&rgb);
            }
            if flags & INVERT_GPU_FLAG_SATURATION != 0 {
                let rgb = opposite_chroma([inverted[0], inverted[1], inverted[2]]);
                inverted[0..3].copy_from_slice(&rgb);
            }
            if flags & INVERT_GPU_FLAG_LIGHTNESS != 0 {
                let rgb = opposite_lightness([inverted[0], inverted[1], inverted[2]]);
                inverted[0..3].copy_from_slice(&rgb);
            }
            let mask_bits = [1, 2, 4];
            for (channel, bit) in mask_bits.into_iter().enumerate() {
                if color_mask & bit != 0 {
                    inverted[channel] = 1.0 - pixel[channel];
                }
            }
            if flags & INVERT_GPU_FLAG_ALPHA != 0 {
                inverted[3] = 1.0 - pixel[3];
            }
            mix_with_original(pixel, inverted, blend_with_original)
        }

        let samples = [
            [0.25, 0.5, 0.75, 1.0],
            [0.9, 0.1, 0.35, 0.5],
            [1.5, -0.25, 2.0, 0.75],
            [0.5, 0.5, 0.5, 0.25],
            [0.0, 0.0, 0.0, 1.0],
        ];
        for scope in InvertChannels::ALL {
            let packed = scope.gpu_scope();
            for pixel in samples {
                for blend in [0.0, 0.25, 1.0] {
                    let expected = invert_rgba_f32(pixel, scope, blend);
                    let actual = decoded(pixel, packed, blend);
                    assert_eq!(
                        actual.map(f32::to_bits),
                        expected.map(f32::to_bits),
                        "scope {} blend {blend} pixel {pixel:?}",
                        scope.key()
                    );
                }
            }
        }
    }

    #[test]
    fn gpu_scope_flags_are_disjoint_per_component() {
        assert_eq!(InvertChannels::Red.gpu_scope(), (0b001, 0));
        assert_eq!(InvertChannels::Green.gpu_scope(), (0b010, 0));
        assert_eq!(InvertChannels::Blue.gpu_scope(), (0b100, 0));
        assert_eq!(
            InvertChannels::Rgb.gpu_scope(),
            (0b111, 0),
            "RGB selects every color channel"
        );
        assert_eq!(
            InvertChannels::Alpha.gpu_scope(),
            (0, INVERT_GPU_FLAG_ALPHA)
        );
        assert_eq!(
            InvertChannels::RedGreenBlueAlpha.gpu_scope(),
            (0b111, INVERT_GPU_FLAG_ALPHA)
        );
        assert_eq!(
            InvertChannels::Hue.gpu_scope(),
            (0, INVERT_GPU_FLAG_HUE),
            "hue scopes carry no channel mask"
        );
        assert_eq!(
            InvertChannels::Hls.gpu_scope(),
            (
                0,
                INVERT_GPU_FLAG_HUE | INVERT_GPU_FLAG_SATURATION | INVERT_GPU_FLAG_LIGHTNESS
            )
        );
        for scope in InvertChannels::ALL {
            let (color_mask, flags) = scope.gpu_scope();
            assert!(color_mask <= 0b111, "{}", scope.key());
            assert!(flags <= 0b1111, "{}", scope.key());
        }
    }
}
