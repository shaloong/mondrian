//! UI-facing color models and parsing helpers.

use crate::types::{
    AcesConfigPreset, Color, ColorEngine, ColorSpace, MondrianStandardPackageIdentity,
    WorkingColorSpace,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Product-level intent for a final working-space to display or delivery transform.
///
/// This describes the color-science choice independently from the CPU/GPU
/// execution backend. Preview and export must preserve this intent until the
/// renderer plans the final output boundary.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OutputTransformIntent {
    /// Mondrian's bundled, versioned OCIO color-management package.
    MondrianStandard {
        /// Full Standard package identity stored in project/cache contracts.
        package: MondrianStandardPackageIdentity,
    },
    /// An official, immutable ACES config preset whose exact View is resolved
    /// from the requested encoded output target.
    Aces {
        /// Exact ACES/OCIO package release selected by the project.
        preset: AcesConfigPreset,
    },
    /// A direct working-space to encoded-output conversion without a view transform.
    Colorimetric,
    /// A target-qualified output binding from a fully pinned Custom OCIO identity.
    CustomOcio {
        /// Standardized output target whose display/view must exist in the engine identity.
        output_color_space: ColorSpace,
    },
}

impl OutputTransformIntent {
    /// Select the current Mondrian Standard output-transform contract.
    pub const fn mondrian_standard() -> Self {
        Self::MondrianStandard { package: MondrianStandardPackageIdentity::V3 }
    }

    /// Select an exact immutable Mondrian Standard package.
    pub const fn mondrian_standard_package(package: MondrianStandardPackageIdentity) -> Self {
        Self::MondrianStandard { package }
    }

    /// Select an exact immutable ACES config preset.
    pub const fn aces_preset(preset: AcesConfigPreset) -> Self {
        Self::Aces { preset }
    }

    /// Resolve this product intent into the optional OCIO display/view pair
    /// consumed by a renderer output boundary.
    ///
    /// `Colorimetric` deliberately returns no view. Mondrian Standard and ACES
    /// resolve output-target-specific Views from their immutable package or
    /// preset and reject identity drift instead of silently executing another
    /// config or relabeling a default View.
    pub fn resolve_display_view(
        &self,
        output_color_space: ColorSpace,
        engine: &ColorEngine,
    ) -> Result<Option<(String, String)>, OutputTransformIntentResolutionError> {
        match self {
            Self::Colorimetric => Ok(None),
            Self::CustomOcio { output_color_space: intent_output } => {
                let ColorEngine::CustomOcio { identity } = engine else {
                    return Err(OutputTransformIntentResolutionError::EngineMismatch {
                        intent: format!("Custom OCIO output {intent_output:?}"),
                        engine: engine.name().to_owned(),
                    });
                };
                if intent_output != &output_color_space {
                    return Err(
                        OutputTransformIntentResolutionError::CustomOcioOutputMismatch {
                            intent_output: *intent_output,
                            requested_output: output_color_space,
                        },
                    );
                }
                identity
                    .output(output_color_space)
                    .map(|output| (output.display().to_owned(), output.view().to_owned()))
                    .map(Some)
                    .ok_or(
                        OutputTransformIntentResolutionError::UnsupportedCustomOcioOutput {
                            output_color_space,
                        },
                    )
            }
            Self::Aces { preset } => {
                let ColorEngine::Aces { preset: engine_preset } = engine else {
                    return Err(OutputTransformIntentResolutionError::EngineMismatch {
                        intent: format!("ACES preset '{}'", preset.builtin_name()),
                        engine: engine.name().to_owned(),
                    });
                };
                if preset != engine_preset {
                    return Err(OutputTransformIntentResolutionError::AcesPresetMismatch {
                        intent_preset: preset.builtin_name().to_owned(),
                        engine_preset: engine_preset.builtin_name().to_owned(),
                    });
                }
                preset
                    .output_display_view(output_color_space)
                    .map(|(display, view)| (display.to_owned(), view.to_owned()))
                    .map(Some)
                    .ok_or_else(
                        || OutputTransformIntentResolutionError::UnsupportedAcesOutput {
                            preset: preset.builtin_name().to_owned(),
                            output_color_space,
                        },
                    )
            }
            Self::MondrianStandard { package } => {
                let ColorEngine::MondrianStandard { package: engine_package } = engine else {
                    return Err(OutputTransformIntentResolutionError::EngineMismatch {
                        intent: "Mondrian Standard".to_owned(),
                        engine: engine.name().to_owned(),
                    });
                };
                if package != engine_package {
                    return Err(OutputTransformIntentResolutionError::PackageMismatch {
                        intent_sha256: package.package_sha256().to_owned(),
                        engine_sha256: engine_package.package_sha256().to_owned(),
                    });
                }
                crate::mondrian_standard_output_display_view_for_package(
                    *package,
                    output_color_space,
                )
                .map(Some)
                .map_err(|reason| {
                    OutputTransformIntentResolutionError::UnsupportedStandardOutput {
                        output_color_space,
                        reason,
                    }
                })
            }
        }
    }
}

/// Failure to resolve a product output-transform intent for renderer execution.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum OutputTransformIntentResolutionError {
    /// The selected engine cannot execute the requested product intent.
    #[error("output transform intent {intent} cannot execute through {engine}")]
    EngineMismatch {
        /// Human-readable product intent.
        intent: String,
        /// Human-readable selected engine.
        engine: String,
    },
    /// The Standard intent and engine pin different immutable packages.
    #[error(
        "Mondrian Standard package mismatch: intent sha256={intent_sha256}, engine sha256={engine_sha256}"
    )]
    PackageMismatch {
        /// Package digest pinned by the intent.
        intent_sha256: String,
        /// Package digest pinned by the engine.
        engine_sha256: String,
    },
    /// The intent and engine pin different immutable ACES config releases.
    #[error(
        "ACES preset mismatch: intent preset='{intent_preset}', engine preset='{engine_preset}'"
    )]
    AcesPresetMismatch {
        /// Built-in config identifier pinned by the intent.
        intent_preset: String,
        /// Built-in config identifier pinned by the engine.
        engine_preset: String,
    },
    /// Mondrian Standard has no view for the requested encoded output target.
    #[error("Mondrian Standard cannot resolve {output_color_space:?}: {reason}")]
    UnsupportedStandardOutput {
        /// Requested encoded output target.
        output_color_space: ColorSpace,
        /// Resolution failure from the package registry.
        reason: String,
    },
    /// The selected ACES config has no rendering View for this output target.
    #[error(
        "ACES preset '{preset}' has no rendering View for output target {output_color_space:?}"
    )]
    UnsupportedAcesOutput {
        /// Built-in config identifier pinned by the intent.
        preset: String,
        /// Requested encoded output target.
        output_color_space: ColorSpace,
    },
    /// The runtime intent and output boundary request disagree on the Custom target.
    #[error(
        "Custom OCIO output intent targets {intent_output:?}, not requested {requested_output:?}"
    )]
    CustomOcioOutputMismatch {
        /// Target stored in the typed output intent.
        intent_output: ColorSpace,
        /// Target requested by the render boundary.
        requested_output: ColorSpace,
    },
    /// The pinned Custom identity does not provide a binding for this output target.
    #[error("Custom OCIO project has no output binding for {output_color_space:?}")]
    UnsupportedCustomOcioOutput {
        /// Requested standardized encoded output target.
        output_color_space: ColorSpace,
    },
}

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
        scene_referred_workflow: bool,
        _working_color_space: WorkingColorSpace,
        _output_color_space: ColorSpace,
    ) -> bool {
        match self {
            Self::Automatic => scene_referred_workflow,
            Self::Always => true,
            Self::Never => false,
        }
    }
}

/// Machine-local Viewer display-management policy.
///
/// This value is execution/session state. It is deliberately separate from
/// Sequence-authored output-transform and tone-map semantics.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DisplayManagementPolicy {
    /// Monitor/profile source used for preview presentation.
    #[serde(default)]
    pub monitor_profile: MonitorProfileReference,
    /// SDR/HDR viewer mode policy.
    #[serde(default)]
    pub viewer_mode: ViewerDisplayMode,
}

impl Default for DisplayManagementPolicy {
    fn default() -> Self {
        Self {
            monitor_profile: MonitorProfileReference::MatchOutputColorSpace,
            viewer_mode: ViewerDisplayMode::MatchOutputColorSpace,
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

    fn pinned_custom_rec709_engine() -> ColorEngine {
        ColorEngine::CustomOcio {
            identity: Box::new(
                crate::CustomOcioProjectIdentity::from_pinned_parts(
                    crate::OcioConfigSource::Environment,
                    "0".repeat(64),
                    "1".repeat(64),
                    "Linear Rec.2020".to_owned(),
                    vec![crate::CustomOcioOutputIdentity::from_pinned_parts(
                        ColorSpace::Rec709,
                        "Rec.709 Display".to_owned(),
                        "Studio View".to_owned(),
                        "Camera Rec.709".to_owned(),
                        crate::CustomOcioLookIdentity::None,
                    )
                    .expect("valid Rec.709 output binding")],
                    Vec::new(),
                    Vec::new(),
                )
                .expect("valid Custom OCIO identity"),
            ),
        }
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
        assert!(!DisplayToneMapPolicy::Automatic.resolve(
            false,
            WorkingColorSpace::LinearRec2020,
            ColorSpace::Rec709
        ));
        assert!(DisplayToneMapPolicy::Automatic.resolve(
            true,
            WorkingColorSpace::LinearRec709,
            ColorSpace::Rec2100Pq
        ));
        assert!(DisplayToneMapPolicy::Always.resolve(
            false,
            WorkingColorSpace::LinearRec709,
            ColorSpace::Rec709
        ));
        assert!(!DisplayToneMapPolicy::Never.resolve(
            true,
            WorkingColorSpace::LinearRec2020,
            ColorSpace::Rec709
        ));
    }

    #[test]
    fn output_transform_intent_resolves_standard_view_for_output_target() {
        let intent = OutputTransformIntent::mondrian_standard();
        let resolved = intent
            .resolve_display_view(ColorSpace::Rec2100Pq, &ColorEngine::mondrian_standard())
            .expect("Mondrian Standard PQ view")
            .expect("display/view");

        assert_eq!(resolved.0, "Rec.2100-PQ - Display");
        assert_eq!(resolved.1, "Mondrian Standard HDR 1000 nits v1");
    }

    #[test]
    fn output_transform_intent_rejects_standard_engine_drift() {
        let intent = OutputTransformIntent::mondrian_standard();
        let error = intent
            .resolve_display_view(
                ColorSpace::Srgb,
                &ColorEngine::Aces {
                    preset: crate::AcesConfigPreset::StudioV4Aces2Ocio25,
                },
            )
            .expect_err("Standard intent must not execute through ACES");

        assert!(matches!(
            error,
            OutputTransformIntentResolutionError::EngineMismatch { .. }
        ));
    }

    #[test]
    fn output_transform_intent_resolves_target_specific_aces_view() {
        let preset = crate::AcesConfigPreset::StudioV4Aces2Ocio25;
        let intent = OutputTransformIntent::aces_preset(preset);

        assert_eq!(
            intent
                .resolve_display_view(ColorSpace::DisplayP3, &ColorEngine::Aces { preset })
                .expect("ACES Display P3 target")
                .expect("display/view"),
            (
                "Display P3 - Display".to_owned(),
                "ACES 2.0 - SDR 100 nits (P3 D65)".to_owned(),
            )
        );
        assert_eq!(
            intent
                .resolve_display_view(ColorSpace::Rec2100Pq, &ColorEngine::Aces { preset })
                .expect("ACES PQ target")
                .expect("display/view"),
            (
                "Rec.2100-PQ - Display".to_owned(),
                "ACES 2.0 - HDR 1000 nits (Rec.2020)".to_owned(),
            )
        );
    }

    #[test]
    fn output_transform_intent_rejects_unsupported_aces_target_and_preset_drift() {
        let studio = crate::AcesConfigPreset::StudioV4Aces2Ocio25;
        let intent = OutputTransformIntent::aces_preset(studio);

        let unsupported = intent
            .resolve_display_view(ColorSpace::Rec2020, &ColorEngine::Aces { preset: studio })
            .expect_err("official ACES 2.0 presets have no Rec.2020 SDR View");
        assert!(matches!(
            unsupported,
            OutputTransformIntentResolutionError::UnsupportedAcesOutput { .. }
        ));

        let drift = intent
            .resolve_display_view(
                ColorSpace::Srgb,
                &ColorEngine::Aces { preset: crate::AcesConfigPreset::CgV4Aces2Ocio25 },
            )
            .expect_err("intent must not execute through another ACES preset");
        assert!(matches!(
            drift,
            OutputTransformIntentResolutionError::AcesPresetMismatch { .. }
        ));
    }

    #[test]
    fn custom_output_intent_resolves_only_its_pinned_target() {
        let engine = pinned_custom_rec709_engine();
        let intent = OutputTransformIntent::CustomOcio { output_color_space: ColorSpace::Rec709 };

        assert_eq!(
            intent
                .resolve_display_view(ColorSpace::Rec709, &engine)
                .expect("pinned Custom Rec.709 output")
                .expect("display/view"),
            ("Rec.709 Display".to_owned(), "Studio View".to_owned())
        );
        assert!(matches!(
            intent
                .resolve_display_view(ColorSpace::Rec2100Pq, &engine)
                .expect_err("Custom output intent must remain target-qualified"),
            OutputTransformIntentResolutionError::CustomOcioOutputMismatch { .. }
        ));

        let unsupported =
            OutputTransformIntent::CustomOcio { output_color_space: ColorSpace::Rec2100Pq };
        assert!(matches!(
            unsupported
                .resolve_display_view(ColorSpace::Rec2100Pq, &engine)
                .expect_err("unbound Custom PQ output must fail closed"),
            OutputTransformIntentResolutionError::UnsupportedCustomOcioOutput { .. }
        ));
    }

    #[test]
    fn display_management_has_no_second_output_transform_truth() {
        let serialized =
            serde_json::to_value(DisplayManagementPolicy::default()).expect("serialize policy");
        assert!(
            serialized.get("export_delivery_view").is_none(),
            "engine-owned output intent must not be overridden by a second display/view policy: {serialized}"
        );
    }
}
