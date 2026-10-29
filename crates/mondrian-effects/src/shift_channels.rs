//! Shift Channels (CHN-07): rewire the RGBA outputs from per-pixel scalars.
//!
//! Every output channel picks one scalar sampled from the incoming pixel, so
//! the grade is a pure permutation/mix of the pixel's own components. Colors
//! are sampled in the working domain without clamping: luminance is the
//! working space CIE Y, hue is the HSL hue in turns, saturation is the HSL
//! chroma relative to the pixel's achievable range, and lightness is the HSL
//! lightness. Neutral pixels report hue 0 and saturation 0 because neither is
//! defined once chroma vanishes.

use crate::hsl_space::{extremes, hue_turns};

/// One scalar a shifted output channel can read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelSource {
    Red,
    Green,
    Blue,
    Alpha,
    Luminance,
    Hue,
    Saturation,
    Lightness,
    Maximum,
    Minimum,
}

impl ChannelSource {
    /// Every source in menu order.
    pub const ALL: [Self; 10] = [
        Self::Red,
        Self::Green,
        Self::Blue,
        Self::Alpha,
        Self::Luminance,
        Self::Hue,
        Self::Saturation,
        Self::Lightness,
        Self::Maximum,
        Self::Minimum,
    ];

    /// Stable persisted key.
    pub const fn key(self) -> &'static str {
        match self {
            Self::Red => "red",
            Self::Green => "green",
            Self::Blue => "blue",
            Self::Alpha => "alpha",
            Self::Luminance => "luminance",
            Self::Hue => "hue",
            Self::Saturation => "saturation",
            Self::Lightness => "lightness",
            Self::Maximum => "maximum",
            Self::Minimum => "minimum",
        }
    }

    /// Resolves a persisted key.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|source| source.key() == key)
    }

    /// Localized inspector label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Red => "红色",
            Self::Green => "绿色",
            Self::Blue => "蓝色",
            Self::Alpha => "Alpha",
            Self::Luminance => "亮度",
            Self::Hue => "色相",
            Self::Saturation => "饱和度",
            Self::Lightness => "明度",
            Self::Maximum => "最大值",
            Self::Minimum => "最小值",
        }
    }

    /// Compact integer used by the fused GPU point path.
    pub const fn gpu_tag(self) -> u32 {
        match self {
            Self::Red => 0,
            Self::Green => 1,
            Self::Blue => 2,
            Self::Alpha => 3,
            Self::Luminance => 4,
            Self::Hue => 5,
            Self::Saturation => 6,
            Self::Lightness => 7,
            Self::Maximum => 8,
            Self::Minimum => 9,
        }
    }
}

/// One authored Shift Channels grade.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShiftChannelsGrade {
    outputs: [ChannelSource; 4],
    luminance_coefficients: [f32; 3],
}

impl ShiftChannelsGrade {
    /// Builds a grade from the four output sources and the working-space CIE Y
    /// coefficients.
    pub const fn new(
        red: ChannelSource,
        green: ChannelSource,
        blue: ChannelSource,
        alpha: ChannelSource,
        luminance_coefficients: [f32; 3],
    ) -> Self {
        Self {
            outputs: [red, green, blue, alpha],
            luminance_coefficients,
        }
    }

    /// Authored source for each output channel, in RGBA order.
    pub const fn outputs(self) -> [ChannelSource; 4] {
        self.outputs
    }

    /// Working-space CIE Y coefficients carried by this grade.
    pub const fn luminance_coefficients(self) -> [f32; 3] {
        self.luminance_coefficients
    }

    /// Whether the grade leaves every channel untouched.
    pub const fn is_identity(self) -> bool {
        matches!(
            self.outputs,
            [
                ChannelSource::Red,
                ChannelSource::Green,
                ChannelSource::Blue,
                ChannelSource::Alpha
            ]
        )
    }

    /// Packed channel sources for the fused point path: four nibbles.
    pub const fn packed_sources(self) -> u32 {
        let mut packed = 0;
        let mut index = 0;
        while index < 4 {
            packed |= self.outputs[index].gpu_tag() << (index * 4);
            index += 1;
        }
        packed
    }

    /// Rewrites one straight-alpha pixel.
    #[inline]
    pub fn apply(self, pixel: [f32; 4]) -> [f32; 4] {
        let rgb = [pixel[0], pixel[1], pixel[2]];
        let source = |channel: ChannelSource| -> f32 {
            match channel {
                ChannelSource::Red => rgb[0],
                ChannelSource::Green => rgb[1],
                ChannelSource::Blue => rgb[2],
                ChannelSource::Alpha => pixel[3],
                ChannelSource::Luminance => {
                    rgb[0] * self.luminance_coefficients[0]
                        + rgb[1] * self.luminance_coefficients[1]
                        + rgb[2] * self.luminance_coefficients[2]
                }
                ChannelSource::Hue => hue_sample(rgb),
                ChannelSource::Saturation => saturation_sample(rgb),
                ChannelSource::Lightness => {
                    let (high, low) = extremes(rgb);
                    (high + low) * 0.5
                }
                ChannelSource::Maximum => {
                    let (high, _) = extremes(rgb);
                    high
                }
                ChannelSource::Minimum => {
                    let (_, low) = extremes(rgb);
                    low
                }
            }
        };
        [
            source(self.outputs[0]),
            source(self.outputs[1]),
            source(self.outputs[2]),
            source(self.outputs[3]),
        ]
    }
}

/// HSL hue in turns; neutral pixels report 0.
fn hue_sample(rgb: [f32; 3]) -> f32 {
    let (high, low) = extremes(rgb);
    let chroma = high - low;
    if chroma.is_nan() || chroma <= crate::hsl_space::NEUTRAL_CHROMA {
        return 0.0;
    }
    hue_turns(rgb, high, chroma)
}

/// HSL chroma relative to the pixel's achievable range; neutral pixels report 0.
fn saturation_sample(rgb: [f32; 3]) -> f32 {
    let (high, low) = extremes(rgb);
    let chroma = high - low;
    let lightness = (high + low) * 0.5;
    let range = 1.0 - (2.0 * lightness - 1.0).abs();
    let neutral = crate::hsl_space::NEUTRAL_CHROMA;
    if chroma.is_nan() || range.is_nan() || chroma <= neutral || range <= neutral {
        return 0.0;
    }
    chroma / range
}

#[cfg(test)]
mod tests {
    use super::*;

    const COEFFICIENTS: [f32; 3] = [0.2126, 0.7152, 0.0722];

    fn grade(
        red: ChannelSource,
        green: ChannelSource,
        blue: ChannelSource,
        alpha: ChannelSource,
    ) -> ShiftChannelsGrade {
        ShiftChannelsGrade::new(red, green, blue, alpha, COEFFICIENTS)
    }

    #[test]
    fn sources_expose_stable_unique_keys_in_menu_order() {
        let mut keys = std::collections::BTreeSet::new();
        for source in ChannelSource::ALL {
            assert!(keys.insert(source.key()), "{}", source.key());
            assert_eq!(ChannelSource::from_key(source.key()), Some(source));
            assert!(!source.label().is_empty());
        }
        assert_eq!(keys.len(), 10);
        assert_eq!(ChannelSource::from_key("nope"), None);
        assert_eq!(
            ChannelSource::ALL.map(ChannelSource::gpu_tag),
            [0, 1, 2, 3, 4, 5, 6, 7, 8, 9]
        );
    }

    #[test]
    fn identity_grade_is_skipped_but_any_rewire_is_not() {
        let identity = |red, green, blue, alpha| grade(red, green, blue, alpha);
        assert!(identity(
            ChannelSource::Red,
            ChannelSource::Green,
            ChannelSource::Blue,
            ChannelSource::Alpha
        )
        .is_identity());
        assert!(!identity(
            ChannelSource::Red,
            ChannelSource::Red,
            ChannelSource::Blue,
            ChannelSource::Alpha
        )
        .is_identity());
        assert!(!identity(
            ChannelSource::Red,
            ChannelSource::Green,
            ChannelSource::Blue,
            ChannelSource::Hue
        )
        .is_identity());
    }

    #[test]
    fn channel_swap_copies_the_requested_components() {
        let pixel = [0.25, 0.5, 0.75, 0.4];
        let shifted = grade(
            ChannelSource::Green,
            ChannelSource::Blue,
            ChannelSource::Red,
            ChannelSource::Alpha,
        )
        .apply(pixel);
        assert_eq!(shifted, [0.5, 0.75, 0.25, 0.4]);
    }

    #[test]
    fn alpha_can_be_read_from_a_color_channel_and_vice_versa() {
        let pixel = [0.25, 0.5, 0.75, 0.4];
        let shifted = grade(
            ChannelSource::Alpha,
            ChannelSource::Green,
            ChannelSource::Blue,
            ChannelSource::Red,
        )
        .apply(pixel);
        assert_eq!(shifted, [0.4, 0.5, 0.75, 0.25]);
    }

    #[test]
    fn luminance_uses_the_supplied_working_space_coefficients() {
        let shifted = grade(
            ChannelSource::Luminance,
            ChannelSource::Luminance,
            ChannelSource::Luminance,
            ChannelSource::Alpha,
        )
        .apply([0.2, 0.4, 0.8, 1.0]);
        let expected = 0.2 * COEFFICIENTS[0] + 0.4 * COEFFICIENTS[1] + 0.8 * COEFFICIENTS[2];
        assert!((shifted[0] - expected).abs() <= 1.0e-6);
        assert_eq!(shifted[1], shifted[0]);
        assert_eq!(shifted[2], shifted[0]);
        assert_eq!(shifted[3], 1.0);
    }

    #[test]
    fn hue_lightness_and_saturation_report_hsl_components() {
        let hue = grade(
            ChannelSource::Hue,
            ChannelSource::Hue,
            ChannelSource::Hue,
            ChannelSource::Alpha,
        )
        .apply([0.9, 0.1, 0.1, 1.0]);
        assert!((hue[0] - 0.0).abs() <= 1.0e-6, "{hue:?}");

        let green = grade(
            ChannelSource::Hue,
            ChannelSource::Hue,
            ChannelSource::Hue,
            ChannelSource::Alpha,
        )
        .apply([0.1, 0.9, 0.1, 1.0]);
        assert!((green[0] - 1.0 / 3.0).abs() <= 1.0e-6, "{green:?}");

        let lightness = grade(
            ChannelSource::Lightness,
            ChannelSource::Lightness,
            ChannelSource::Lightness,
            ChannelSource::Alpha,
        )
        .apply([0.2, 0.4, 0.8, 1.0]);
        assert!((lightness[0] - 0.5).abs() <= 1.0e-6, "{lightness:?}");

        let saturation = grade(
            ChannelSource::Saturation,
            ChannelSource::Saturation,
            ChannelSource::Saturation,
            ChannelSource::Alpha,
        )
        .apply([1.0, 0.0, 0.0, 1.0]);
        assert!((saturation[0] - 1.0).abs() <= 1.0e-6, "{saturation:?}");
    }

    #[test]
    fn neutral_pixels_report_zero_hue_and_saturation() {
        let neutral = grade(
            ChannelSource::Hue,
            ChannelSource::Saturation,
            ChannelSource::Lightness,
            ChannelSource::Alpha,
        )
        .apply([0.5, 0.5, 0.5, 1.0]);
        assert_eq!(neutral, [0.0, 0.0, 0.5, 1.0]);
    }

    #[test]
    fn extremes_are_available_as_sources() {
        let both = grade(
            ChannelSource::Maximum,
            ChannelSource::Minimum,
            ChannelSource::Red,
            ChannelSource::Alpha,
        )
        .apply([0.25, 0.5, 0.75, 1.0]);
        assert_eq!(both, [0.75, 0.25, 0.25, 1.0]);
    }

    #[test]
    fn extended_range_values_are_not_clamped() {
        let shifted = grade(
            ChannelSource::Blue,
            ChannelSource::Green,
            ChannelSource::Red,
            ChannelSource::Alpha,
        )
        .apply([-0.5, 2.0, 4.0, 1.0]);
        assert_eq!(shifted, [4.0, 2.0, -0.5, 1.0]);
    }

    #[test]
    fn packed_sources_round_trip_every_channel() {
        let authored = grade(
            ChannelSource::Alpha,
            ChannelSource::Luminance,
            ChannelSource::Minimum,
            ChannelSource::Hue,
        );
        let packed = authored.packed_sources();
        let decoded = std::array::from_fn(|index| {
            let tag = (packed >> (index * 4)) & 0b1111;
            ChannelSource::ALL[tag as usize]
        });
        assert_eq!(decoded, authored.outputs());
        assert_eq!(
            grade(
                ChannelSource::Red,
                ChannelSource::Green,
                ChannelSource::Blue,
                ChannelSource::Alpha
            )
            .packed_sources(),
            0x3210
        );
    }
}
