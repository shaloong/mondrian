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

/// Chroma below this value is treated as neutral, where hue and saturation are
/// undefined.
const NEUTRAL_CHROMA: f32 = 1.0e-6;

/// One half turn of the hue circle.
const HALF_TURN: f32 = 0.5;

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
}

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
        let rgb = invert_hue([pixel[0], pixel[1], pixel[2]]);
        inverted[0] = rgb[0];
        inverted[1] = rgb[1];
        inverted[2] = rgb[2];
    }
    if channels.inverts_saturation() {
        let rgb = invert_saturation([inverted[0], inverted[1], inverted[2]]);
        inverted[0] = rgb[0];
        inverted[1] = rgb[1];
        inverted[2] = rgb[2];
    }
    if channels.inverts_lightness() {
        let rgb = invert_lightness([inverted[0], inverted[1], inverted[2]]);
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

/// Invert one straight-alpha 8-bit RGBA pixel.
///
/// This is the crate-local `u8` reference path. It runs the same float kernel
/// and rounds once at the end, so both execution paths share one definition.
pub fn invert_rgba_u8(
    pixel: [u8; 4],
    channels: InvertChannels,
    blend_with_original: f32,
) -> [u8; 4] {
    let unit = [
        f32::from(pixel[0]) / 255.0,
        f32::from(pixel[1]) / 255.0,
        f32::from(pixel[2]) / 255.0,
        f32::from(pixel[3]) / 255.0,
    ];
    let inverted = invert_rgba_f32(unit, channels, blend_with_original);
    [
        unit_to_u8(inverted[0]),
        unit_to_u8(inverted[1]),
        unit_to_u8(inverted[2]),
        unit_to_u8(inverted[3]),
    ]
}

fn unit_to_u8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
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

fn extremes(rgb: [f32; 3]) -> (f32, f32) {
    let high = rgb[0].max(rgb[1]).max(rgb[2]);
    let low = rgb[0].min(rgb[1]).min(rgb[2]);
    (high, low)
}

/// Hue of a non-neutral color in `[0, 1)`.
fn hue_of(rgb: [f32; 3], high: f32, chroma: f32) -> f32 {
    let hue = if high == rgb[0] {
        (rgb[1] - rgb[2]) / chroma
    } else if high == rgb[1] {
        (rgb[2] - rgb[0]) / chroma + 2.0
    } else {
        (rgb[0] - rgb[1]) / chroma + 4.0
    };
    (hue / 6.0).rem_euclid(1.0)
}

/// Rebuild a color from hue, chroma and lightness.
///
/// The reconstruction is exact for any lightness, including values outside
/// `[0, 1]`, because it only adds the common offset `lightness - chroma / 2`.
fn rgb_from_hue_chroma_lightness(hue: f32, chroma: f32, lightness: f32) -> [f32; 3] {
    let sextant = hue.rem_euclid(1.0) * 6.0;
    let second = chroma * (1.0 - (sextant.rem_euclid(2.0) - 1.0).abs());
    let (red, green, blue) = if sextant < 1.0 {
        (chroma, second, 0.0)
    } else if sextant < 2.0 {
        (second, chroma, 0.0)
    } else if sextant < 3.0 {
        (0.0, chroma, second)
    } else if sextant < 4.0 {
        (0.0, second, chroma)
    } else if sextant < 5.0 {
        (second, 0.0, chroma)
    } else {
        (chroma, 0.0, second)
    };
    let offset = lightness - chroma * 0.5;
    [red + offset, green + offset, blue + offset]
}

/// Rotate the hue by half a turn, preserving lightness and chroma.
fn invert_hue(rgb: [f32; 3]) -> [f32; 3] {
    let (high, low) = extremes(rgb);
    let chroma = high - low;
    if chroma <= NEUTRAL_CHROMA {
        return rgb;
    }
    let hue = hue_of(rgb, high, chroma);
    let lightness = (high + low) * 0.5;
    rgb_from_hue_chroma_lightness(hue + HALF_TURN, chroma, lightness)
}

/// Reflect lightness around `0.5`, preserving chroma.
fn invert_lightness(rgb: [f32; 3]) -> [f32; 3] {
    let (high, low) = extremes(rgb);
    let lightness = (high + low) * 0.5;
    let shift = 1.0 - 2.0 * lightness;
    [rgb[0] + shift, rgb[1] + shift, rgb[2] + shift]
}

/// Reflect HSL saturation around `0.5`, preserving hue and lightness.
///
/// HSL saturation is `chroma / (1 - |2L - 1|)`. Outside the unit cube that
/// denominator is not a positive color range, so out-of-domain pixels keep
/// their value instead of being forced into the cube.
fn invert_saturation(rgb: [f32; 3]) -> [f32; 3] {
    let (high, low) = extremes(rgb);
    let chroma = high - low;
    if chroma <= NEUTRAL_CHROMA {
        return rgb;
    }
    let lightness = (high + low) * 0.5;
    let range = 1.0 - (2.0 * lightness - 1.0).abs();
    if range <= NEUTRAL_CHROMA || chroma > range {
        return rgb;
    }
    // Reflecting saturation around 0.5 reflects chroma inside the color range
    // of that lightness. Computing the reflected chroma by subtraction keeps
    // the move involutive to within one rounding step.
    let scale = (range - chroma) / chroma;
    [
        lightness + (rgb[0] - lightness) * scale,
        lightness + (rgb[1] - lightness) * scale,
        lightness + (rgb[2] - lightness) * scale,
    ]
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
        let (high, low) = extremes(rgb);
        (high + low) * 0.5
    }

    fn chroma(rgb: [f32; 3]) -> f32 {
        let (high, low) = extremes(rgb);
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
    fn u8_reference_path_matches_integer_inversion() {
        for value in 0..=255u8 {
            let pixel = [value, 255 - value, value / 2, 255];
            assert_eq!(
                invert_rgba_u8(pixel, InvertChannels::Rgb, 0.0),
                [255 - pixel[0], 255 - pixel[1], 255 - pixel[2], 255]
            );
        }
        assert_eq!(
            invert_rgba_u8([10, 20, 30, 40], InvertChannels::Alpha, 0.0),
            [10, 20, 30, 215]
        );
    }
}
