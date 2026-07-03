//! UI-facing color models and parsing helpers.

use crate::types::{Color, ColorSpace};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Display or monitor profile reference used by preview presentation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum MonitorProfileReference {
    /// Use the final output color space as the display profile contract.
    #[default]
    MatchOutputColorSpace,
    /// Use a Mondrian-managed color space as the monitor profile.
    ColorSpace(ColorSpace),
    /// Use an OCIO display from the active config.
    OcioDisplay {
        /// OCIO display name.
        display: String,
    },
    /// Use an externally managed ICC profile identified by product metadata.
    IccProfile {
        /// Stable profile identifier or absolute profile path chosen by the caller.
        profile_id: String,
    },
}

impl MonitorProfileReference {
    /// Resolve the managed color space when this profile directly maps to one.
    pub fn managed_color_space(&self, output_color_space: ColorSpace) -> Option<ColorSpace> {
        match self {
            Self::MatchOutputColorSpace => Some(output_color_space),
            Self::ColorSpace(color_space) => Some(*color_space),
            Self::OcioDisplay { .. } | Self::IccProfile { .. } => None,
        }
    }
}

/// Viewer presentation mode selected for display management.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum ViewerDisplayMode {
    /// Resolve SDR/HDR mode from the output color-space contract.
    #[default]
    MatchOutputColorSpace,
    /// SDR viewer mode.
    Sdr,
    /// HDR viewer mode using PQ/ST 2084 semantics.
    HdrPq,
    /// HDR viewer mode using HLG semantics.
    HdrHlg,
}

impl ViewerDisplayMode {
    /// Resolve this mode against a concrete output color space.
    pub fn resolve(self, output_color_space: ColorSpace) -> ResolvedViewerDisplayMode {
        match self {
            Self::MatchOutputColorSpace => match output_color_space {
                ColorSpace::Rec2100Pq => ResolvedViewerDisplayMode::HdrPq,
                ColorSpace::Rec2100Hlg => ResolvedViewerDisplayMode::HdrHlg,
                _ => ResolvedViewerDisplayMode::Sdr,
            },
            Self::Sdr => ResolvedViewerDisplayMode::Sdr,
            Self::HdrPq => ResolvedViewerDisplayMode::HdrPq,
            Self::HdrHlg => ResolvedViewerDisplayMode::HdrHlg,
        }
    }
}

/// Concrete SDR/HDR mode after resolving a viewer policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ResolvedViewerDisplayMode {
    /// SDR presentation.
    Sdr,
    /// HDR presentation using PQ/ST 2084 semantics.
    HdrPq,
    /// HDR presentation using HLG semantics.
    HdrHlg,
}

impl ResolvedViewerDisplayMode {
    /// Whether this resolved viewer mode is HDR.
    pub fn is_hdr(self) -> bool {
        matches!(self, Self::HdrPq | Self::HdrHlg)
    }
}

/// Policy for applying tone mapping at display/export output boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum DisplayToneMapPolicy {
    /// Tone-map when the sequence/workflow/output contract requires it.
    #[default]
    Automatic,
    /// Always request tone mapping at the output boundary.
    Always,
    /// Explicitly bypass tone mapping for technical monitoring or passthrough.
    Never,
}

impl DisplayToneMapPolicy {
    /// Resolve the concrete tone-map flag for a working -> output boundary.
    pub fn resolve(
        self,
        auto_tone_map_media: bool,
        scene_referred_workflow: bool,
        working_color_space: ColorSpace,
        output_color_space: ColorSpace,
    ) -> bool {
        match self {
            Self::Automatic => {
                auto_tone_map_media
                    || scene_referred_workflow
                    || (working_color_space.is_hdr() && !output_color_space.is_hdr())
            }
            Self::Always => true,
            Self::Never => false,
        }
    }
}

/// Display-management policy resolved by project/sequence settings.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DisplayManagementPolicy {
    /// Monitor/profile source used for preview presentation.
    #[serde(default)]
    pub monitor_profile: MonitorProfileReference,
    /// SDR/HDR viewer mode policy.
    #[serde(default)]
    pub viewer_mode: ViewerDisplayMode,
    /// Tone-map policy for output boundaries.
    #[serde(default)]
    pub tone_map_policy: DisplayToneMapPolicy,
}

impl Default for DisplayManagementPolicy {
    fn default() -> Self {
        Self {
            monitor_profile: MonitorProfileReference::MatchOutputColorSpace,
            viewer_mode: ViewerDisplayMode::MatchOutputColorSpace,
            tone_map_policy: DisplayToneMapPolicy::Automatic,
        }
    }
}

/// Error returned when parsing a hex color string fails.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ColorParseError {
    /// The string length is not one of RGB, RGBA, RRGGBB, or RRGGBBAA.
    #[error("hex color must be #RGB, #RGBA, #RRGGBB, or #RRGGBBAA")]
    InvalidLength,
    /// A non-hexadecimal digit was found.
    #[error("hex color contains a non-hex digit")]
    InvalidDigit,
}

/// RGB/RGBA color represented as normalized channels in the 0..1 range.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RgbaColor {
    /// Red channel in 0..1.
    pub r: f32,
    /// Green channel in 0..1.
    pub g: f32,
    /// Blue channel in 0..1.
    pub b: f32,
    /// Alpha channel in 0..1.
    pub a: f32,
}

/// HSL color with hue in degrees and channels in the 0..1 range.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct HslColor {
    /// Hue in degrees. Values are normalized into 0..360.
    pub h: f32,
    /// Saturation in 0..1.
    pub s: f32,
    /// Lightness in 0..1.
    pub l: f32,
    /// Alpha channel in 0..1.
    pub a: f32,
}

/// HSV color with hue in degrees and channels in the 0..1 range.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct HsvColor {
    /// Hue in degrees. Values are normalized into 0..360.
    pub h: f32,
    /// Saturation in 0..1.
    pub s: f32,
    /// Value/brightness in 0..1.
    pub v: f32,
    /// Alpha channel in 0..1.
    pub a: f32,
}

/// CMYK color with channels in the 0..1 range.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CmykColor {
    /// Cyan in 0..1.
    pub c: f32,
    /// Magenta in 0..1.
    pub m: f32,
    /// Yellow in 0..1.
    pub y: f32,
    /// Key/black in 0..1.
    pub k: f32,
    /// Alpha channel in 0..1.
    pub a: f32,
}

impl Color {
    /// Construct a color from normalized RGBA channels, clamped into 0..1.
    pub fn from_rgba(color: RgbaColor) -> Self {
        Self {
            r: clamp_unit(color.r),
            g: clamp_unit(color.g),
            b: clamp_unit(color.b),
            a: clamp_unit(color.a),
        }
    }

    /// Return normalized RGBA channels.
    pub fn to_rgba(self) -> RgbaColor {
        RgbaColor {
            r: clamp_unit(self.r),
            g: clamp_unit(self.g),
            b: clamp_unit(self.b),
            a: clamp_unit(self.a),
        }
    }

    /// Construct a color from 8-bit RGBA channels.
    pub fn from_rgba8(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self {
            r: r as f32 / 255.0,
            g: g as f32 / 255.0,
            b: b as f32 / 255.0,
            a: a as f32 / 255.0,
        }
    }

    /// Return 8-bit RGBA channels with rounded conversion.
    pub fn to_rgba8(self) -> [u8; 4] {
        [
            unit_to_u8(self.r),
            unit_to_u8(self.g),
            unit_to_u8(self.b),
            unit_to_u8(self.a),
        ]
    }

    /// Parse a hex color string.
    ///
    /// Supports `#RGB`, `#RGBA`, `#RRGGBB`, `#RRGGBBAA`, and the same forms
    /// without `#`. A `0x` prefix is also accepted.
    pub fn parse_hex(input: &str) -> Result<Self, ColorParseError> {
        let trimmed = input.trim();
        let hex = trimmed
            .strip_prefix('#')
            .or_else(|| trimmed.strip_prefix("0x"))
            .or_else(|| trimmed.strip_prefix("0X"))
            .unwrap_or(trimmed);

        match hex.len() {
            3 => {
                let r = expand_nibble(hex_byte(hex, 0)?);
                let g = expand_nibble(hex_byte(hex, 1)?);
                let b = expand_nibble(hex_byte(hex, 2)?);
                Ok(Self::from_rgba8(r, g, b, 255))
            }
            4 => {
                let r = expand_nibble(hex_byte(hex, 0)?);
                let g = expand_nibble(hex_byte(hex, 1)?);
                let b = expand_nibble(hex_byte(hex, 2)?);
                let a = expand_nibble(hex_byte(hex, 3)?);
                Ok(Self::from_rgba8(r, g, b, a))
            }
            6 => Ok(Self::from_rgba8(
                hex_pair(hex, 0)?,
                hex_pair(hex, 2)?,
                hex_pair(hex, 4)?,
                255,
            )),
            8 => Ok(Self::from_rgba8(
                hex_pair(hex, 0)?,
                hex_pair(hex, 2)?,
                hex_pair(hex, 4)?,
                hex_pair(hex, 6)?,
            )),
            _ => Err(ColorParseError::InvalidLength),
        }
    }

    /// Return `#RRGGBB`.
    pub fn to_hex_rgb(self) -> String {
        let [r, g, b, _] = self.to_rgba8();
        format!("#{r:02X}{g:02X}{b:02X}")
    }

    /// Return `#RRGGBBAA`.
    pub fn to_hex_rgba(self) -> String {
        let [r, g, b, a] = self.to_rgba8();
        format!("#{r:02X}{g:02X}{b:02X}{a:02X}")
    }

    /// Construct a color from HSL.
    pub fn from_hsl(color: HslColor) -> Self {
        let h = normalize_hue(color.h) / 360.0;
        let s = clamp_unit(color.s);
        let l = clamp_unit(color.l);

        if s == 0.0 {
            return Self::from_rgba(RgbaColor { r: l, g: l, b: l, a: color.a });
        }

        let q = if l < 0.5 {
            l * (1.0 + s)
        } else {
            l + s - l * s
        };
        let p = 2.0 * l - q;
        Self::from_rgba(RgbaColor {
            r: hue_to_rgb(p, q, h + 1.0 / 3.0),
            g: hue_to_rgb(p, q, h),
            b: hue_to_rgb(p, q, h - 1.0 / 3.0),
            a: color.a,
        })
    }

    /// Convert this color to HSL.
    pub fn to_hsl(self) -> HslColor {
        let rgba = self.to_rgba();
        let max = rgba.r.max(rgba.g).max(rgba.b);
        let min = rgba.r.min(rgba.g).min(rgba.b);
        let l = (max + min) * 0.5;

        if nearly_equal(max, min) {
            return HslColor { h: 0.0, s: 0.0, l, a: rgba.a };
        }

        let delta = max - min;
        let s = if l > 0.5 {
            delta / (2.0 - max - min)
        } else {
            delta / (max + min)
        };
        HslColor {
            h: rgb_hue_degrees(rgba.r, rgba.g, rgba.b, max, delta),
            s,
            l,
            a: rgba.a,
        }
    }

    /// Construct a color from HSV.
    pub fn from_hsv(color: HsvColor) -> Self {
        let h = normalize_hue(color.h);
        let s = clamp_unit(color.s);
        let v = clamp_unit(color.v);

        if s == 0.0 {
            return Self::from_rgba(RgbaColor { r: v, g: v, b: v, a: color.a });
        }

        let sector = h / 60.0;
        let i = sector.floor() as i32;
        let f = sector - i as f32;
        let p = v * (1.0 - s);
        let q = v * (1.0 - s * f);
        let t = v * (1.0 - s * (1.0 - f));

        let (r, g, b) = match i.rem_euclid(6) {
            0 => (v, t, p),
            1 => (q, v, p),
            2 => (p, v, t),
            3 => (p, q, v),
            4 => (t, p, v),
            _ => (v, p, q),
        };
        Self::from_rgba(RgbaColor { r, g, b, a: color.a })
    }

    /// Convert this color to HSV.
    pub fn to_hsv(self) -> HsvColor {
        let rgba = self.to_rgba();
        let max = rgba.r.max(rgba.g).max(rgba.b);
        let min = rgba.r.min(rgba.g).min(rgba.b);
        let delta = max - min;
        let s = if max == 0.0 { 0.0 } else { delta / max };
        let h = if nearly_equal(delta, 0.0) {
            0.0
        } else {
            rgb_hue_degrees(rgba.r, rgba.g, rgba.b, max, delta)
        };
        HsvColor { h, s, v: max, a: rgba.a }
    }

    /// Construct a color from CMYK.
    pub fn from_cmyk(color: CmykColor) -> Self {
        let c = clamp_unit(color.c);
        let m = clamp_unit(color.m);
        let y = clamp_unit(color.y);
        let k = clamp_unit(color.k);
        Self::from_rgba(RgbaColor {
            r: (1.0 - c) * (1.0 - k),
            g: (1.0 - m) * (1.0 - k),
            b: (1.0 - y) * (1.0 - k),
            a: color.a,
        })
    }

    /// Convert this color to CMYK.
    pub fn to_cmyk(self) -> CmykColor {
        let rgba = self.to_rgba();
        let k = 1.0 - rgba.r.max(rgba.g).max(rgba.b);
        if nearly_equal(k, 1.0) {
            return CmykColor { c: 0.0, m: 0.0, y: 0.0, k: 1.0, a: rgba.a };
        }
        let denom = 1.0 - k;
        CmykColor {
            c: (1.0 - rgba.r - k) / denom,
            m: (1.0 - rgba.g - k) / denom,
            y: (1.0 - rgba.b - k) / denom,
            k,
            a: rgba.a,
        }
    }
}

fn clamp_unit(value: f32) -> f32 {
    if value.is_nan() {
        0.0
    } else {
        value.clamp(0.0, 1.0)
    }
}

fn unit_to_u8(value: f32) -> u8 {
    (clamp_unit(value) * 255.0).round() as u8
}

fn normalize_hue(hue: f32) -> f32 {
    if hue.is_nan() {
        0.0
    } else {
        hue.rem_euclid(360.0)
    }
}

fn hue_to_rgb(p: f32, q: f32, mut t: f32) -> f32 {
    if t < 0.0 {
        t += 1.0;
    }
    if t > 1.0 {
        t -= 1.0;
    }
    if t < 1.0 / 6.0 {
        p + (q - p) * 6.0 * t
    } else if t < 0.5 {
        q
    } else if t < 2.0 / 3.0 {
        p + (q - p) * (2.0 / 3.0 - t) * 6.0
    } else {
        p
    }
}

fn rgb_hue_degrees(r: f32, g: f32, b: f32, max: f32, delta: f32) -> f32 {
    let hue = if nearly_equal(max, r) {
        60.0 * ((g - b) / delta).rem_euclid(6.0)
    } else if nearly_equal(max, g) {
        60.0 * ((b - r) / delta + 2.0)
    } else {
        60.0 * ((r - g) / delta + 4.0)
    };
    normalize_hue(hue)
}

fn hex_byte(input: &str, index: usize) -> Result<u8, ColorParseError> {
    input
        .as_bytes()
        .get(index)
        .and_then(|byte| (*byte as char).to_digit(16))
        .map(|digit| digit as u8)
        .ok_or(ColorParseError::InvalidDigit)
}

fn hex_pair(input: &str, index: usize) -> Result<u8, ColorParseError> {
    let hi = hex_byte(input, index)?;
    let lo = hex_byte(input, index + 1)?;
    Ok((hi << 4) | lo)
}

fn expand_nibble(value: u8) -> u8 {
    (value << 4) | value
}

fn nearly_equal(a: f32, b: f32) -> bool {
    (a - b).abs() <= f32::EPSILON
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() <= 0.001,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn hex_parses_short_and_long_forms() {
        assert_eq!(
            Color::parse_hex("#0F8").unwrap().to_rgba8(),
            [0, 255, 136, 255]
        );
        assert_eq!(
            Color::parse_hex("#0F8C").unwrap().to_rgba8(),
            [0, 255, 136, 204]
        );
        assert_eq!(
            Color::parse_hex("336699").unwrap().to_rgba8(),
            [51, 102, 153, 255]
        );
        assert_eq!(
            Color::parse_hex("0x33669980").unwrap().to_rgba8(),
            [51, 102, 153, 128]
        );
    }

    #[test]
    fn hex_rejects_invalid_input() {
        assert_eq!(Color::parse_hex("#12"), Err(ColorParseError::InvalidLength));
        assert_eq!(Color::parse_hex("#GGG"), Err(ColorParseError::InvalidDigit));
    }

    #[test]
    fn hex_outputs_uppercase() {
        let color = Color::from_rgba8(51, 102, 153, 128);
        assert_eq!(color.to_hex_rgb(), "#336699");
        assert_eq!(color.to_hex_rgba(), "#33669980");
    }

    #[test]
    fn hsl_round_trip_primary_color() {
        let red = Color::from_rgba8(255, 0, 0, 128);
        let hsl = red.to_hsl();
        assert_close(hsl.h, 0.0);
        assert_close(hsl.s, 1.0);
        assert_close(hsl.l, 0.5);
        assert_eq!(Color::from_hsl(hsl).to_rgba8(), [255, 0, 0, 128]);
    }

    #[test]
    fn hsv_round_trip_primary_color() {
        let blue = Color::from_rgba8(0, 0, 255, 64);
        let hsv = blue.to_hsv();
        assert_close(hsv.h, 240.0);
        assert_close(hsv.s, 1.0);
        assert_close(hsv.v, 1.0);
        assert_eq!(Color::from_hsv(hsv).to_rgba8(), [0, 0, 255, 64]);
    }

    #[test]
    fn cmyk_round_trip_sample_color() {
        let color = Color::from_rgba8(51, 102, 153, 200);
        let cmyk = color.to_cmyk();
        let converted = Color::from_cmyk(cmyk).to_rgba8();
        assert_eq!(converted, [51, 102, 153, 200]);
    }

    #[test]
    fn constructors_clamp_channels() {
        let color = Color::from_hsv(HsvColor { h: -120.0, s: 2.0, v: 2.0, a: -1.0 });
        assert_eq!(color.to_rgba8()[3], 0);
        assert_close(color.to_hsv().h, 240.0);
    }

    #[test]
    fn viewer_display_mode_resolves_from_output_color_space() {
        assert_eq!(
            ViewerDisplayMode::MatchOutputColorSpace.resolve(ColorSpace::Rec709),
            ResolvedViewerDisplayMode::Sdr
        );
        assert_eq!(
            ViewerDisplayMode::MatchOutputColorSpace.resolve(ColorSpace::Rec2100Pq),
            ResolvedViewerDisplayMode::HdrPq
        );
        assert_eq!(
            ViewerDisplayMode::MatchOutputColorSpace.resolve(ColorSpace::Rec2100Hlg),
            ResolvedViewerDisplayMode::HdrHlg
        );
        assert!(ResolvedViewerDisplayMode::HdrPq.is_hdr());
        assert!(!ResolvedViewerDisplayMode::Sdr.is_hdr());
    }

    #[test]
    fn display_tone_map_policy_resolves_boundary_flag() {
        assert!(DisplayToneMapPolicy::Automatic.resolve(
            false,
            false,
            ColorSpace::Rec2100Pq,
            ColorSpace::Rec709
        ));
        assert!(DisplayToneMapPolicy::Automatic.resolve(
            false,
            true,
            ColorSpace::Rec709,
            ColorSpace::Rec2100Pq
        ));
        assert!(DisplayToneMapPolicy::Always.resolve(
            false,
            false,
            ColorSpace::Rec709,
            ColorSpace::Rec709
        ));
        assert!(!DisplayToneMapPolicy::Never.resolve(
            true,
            true,
            ColorSpace::Rec2100Pq,
            ColorSpace::Rec709
        ));
    }
}
