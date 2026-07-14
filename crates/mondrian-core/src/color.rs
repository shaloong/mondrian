//! Color management primitives shared by preview, render and export.

use crate::types::{ColorEngine, ColorSpace, OcioColorSpaceIdentity};
use serde::{Deserialize, Serialize};

// ── ColorEngine: centralized dispatch ─────────────────────────────────────────

impl ColorEngine {
    /// Apply a float OCIO processor between explicit encoded/working identities.
    pub fn convert_identity_float(
        &self,
        data: &mut [f32],
        src: OcioColorSpaceIdentity,
        dst: OcioColorSpaceIdentity,
    ) -> Result<(), String> {
        match self {
            Self::MondrianStandard { .. } => crate::ocio::ensure_mondrian_default_ocio_loaded()?,
            Self::Aces { .. } | Self::CustomOcio { .. } => self.ensure_loaded()?,
        }
        crate::ocio::apply_ocio_identity_float(data, src, dst)
    }

    /// Apply an OCIO display/view processor from an explicit source identity.
    pub fn display_transform_identity_float(
        &self,
        data: &mut [f32],
        src: OcioColorSpaceIdentity,
        display: &str,
        view: &str,
    ) -> Result<(), String> {
        match self {
            Self::MondrianStandard { .. } => crate::ocio::ensure_mondrian_default_ocio_loaded()?,
            Self::Aces { .. } | Self::CustomOcio { .. } => self.ensure_loaded()?,
        }
        crate::ocio::apply_ocio_display_identity_float(data, src, display, view)
    }

    /// Whether the engine is ready to process data.
    pub fn is_available(&self) -> bool {
        match self {
            Self::MondrianStandard { .. } => crate::ocio::mondrian_default_ocio_available(),
            Self::Aces { .. } | Self::CustomOcio { .. } => crate::ocio::ocio_available(),
        }
    }

    /// Ensure any required external config is loaded.
    pub fn ensure_loaded(&self) -> Result<(), String> {
        match self {
            Self::MondrianStandard { .. } => crate::ocio::ensure_mondrian_default_ocio_loaded(),
            Self::Aces { preset } => crate::ocio::ensure_ocio_loaded(&preset.ocio_source()),
            Self::CustomOcio { source } => crate::ocio::ensure_ocio_loaded(source),
        }
    }

    /// Human-readable name for diagnostics / UI.
    pub fn name(&self) -> &'static str {
        match self {
            Self::MondrianStandard { .. } => "Mondrian Standard",
            Self::Aces { .. } => "ACES",
            Self::CustomOcio { .. } => "Custom OpenColorIO",
        }
    }
}

/// Broad encoding category used by validation, preview diagnostics and export tagging.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ColorEncodingKind {
    /// Display-referred SDR delivery or monitoring signal.
    DisplaySdr,
    /// Display-referred HDR delivery or monitoring signal.
    DisplayHdr,
    /// Scene-linear float media.
    SceneLinear,
    /// Scene-referred logarithmic media without a stable FFmpeg delivery tag.
    SceneLog,
}

/// CICP-style color primaries used by a [`ColorSpace`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ColorPrimaries {
    /// ITU-R BT.709 / sRGB primaries.
    Bt709,
    /// BT.470 System B/G primaries used by 625-line Rec.601/PAL material.
    Bt470Bg,
    /// SMPTE 170M/SMPTE-C primaries used by 525-line Rec.601/NTSC material.
    Smpte170M,
    /// ITU-R BT.2020 primaries.
    Bt2020,
    /// Display P3 D65 primaries as represented by FFmpeg `smpte432`.
    P3D65,
    /// Sony S-Gamut3 primaries.
    SonySGamut3,
    /// Sony S-Gamut primaries.
    SonySGamut,
    /// Sony S-Gamut3.Cine primaries.
    SonySGamut3Cine,
    /// ARRI Wide Gamut 3 primaries.
    ArriWideGamut3,
    /// ARRI Wide Gamut 4 primaries.
    ArriWideGamut4,
    /// Canon Cinema Gamut with a D55 white point.
    CanonCinemaGamutD55,
    /// Panasonic V-Gamut primaries.
    PanasonicVGamut,
    /// REDWideGamutRGB primaries.
    RedWideGamutRgb,
    /// Blackmagic Wide Gamut Gen 5 primaries.
    BlackmagicWideGamutGen5,
    /// DJI D-Gamut primaries.
    DjiDGamut,
    /// DaVinci Wide Gamut primaries.
    DavinciWideGamut,
    /// ACES AP0 primaries with the ACES white point.
    AcesAp0,
    /// ACES AP1 primaries with the ACES white point.
    AcesAp1,
}

/// CICP-style transfer characteristic used by a [`ColorSpace`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ColorTransferCharacteristic {
    /// BT.709-style SDR transfer.
    Bt709,
    /// BT.470 System B/G nominal gamma 2.8 transfer.
    Gamma28,
    /// SMPTE 170M camera transfer (mathematically aligned with BT.709 here).
    Smpte170M,
    /// IEC 61966-2-1 sRGB transfer.
    Srgb,
    /// Hybrid Log-Gamma transfer.
    Hlg,
    /// Perceptual Quantizer transfer.
    Pq,
    /// Linear-light transfer.
    Linear,
    /// ACEScct logarithmic transfer.
    AcesCct,
    /// Apple Log acquisition transfer.
    AppleLog,
    /// Sony S-Log3 acquisition transfer.
    SLog3,
    /// Sony S-Log2 acquisition transfer.
    SLog2,
    /// ARRI LogC3 EI800 acquisition transfer.
    ArriLogC3,
    /// ARRI LogC4 acquisition transfer.
    ArriLogC4,
    /// Canon Log 2 acquisition transfer.
    CanonLog2,
    /// Canon Log 3 acquisition transfer.
    CanonLog3,
    /// Panasonic V-Log acquisition transfer.
    PanasonicVLog,
    /// RED Log3G10 acquisition transfer.
    RedLog3G10,
    /// Blackmagic Film Gen 5 acquisition transfer.
    BlackmagicFilmGen5,
    /// DJI D-Log acquisition transfer.
    DjiDLog,
    /// DaVinci Intermediate acquisition transfer.
    DavinciIntermediate,
}

/// CICP-style matrix coefficients used when encoding YUV/RGB signals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ColorMatrixCoefficients {
    /// BT.709 non-constant luminance matrix.
    Bt709,
    /// FCC legacy television matrix.
    Fcc,
    /// BT.470BG / 625-line BT.601 matrix.
    Bt470Bg,
    /// SMPTE 170M / 525-line BT.601 matrix.
    Smpte170M,
    /// SMPTE 240M matrix.
    Smpte240M,
    /// BT.2020 non-constant luminance matrix.
    Bt2020NonConstant,
    /// RGB signal with no YUV matrix.
    Rgb,
    /// No reliable standardized matrix tag is available for this acquisition space.
    Unspecified,
}

/// Canonical encoding metadata for a Mondrian color space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ColorEncodingSpec {
    /// Primary chromaticities.
    pub primaries: ColorPrimaries,
    /// Transfer characteristic.
    pub transfer: ColorTransferCharacteristic,
    /// Matrix coefficients for encoded video.
    pub matrix: ColorMatrixCoefficients,
    /// Delivery/acquisition category.
    pub kind: ColorEncodingKind,
}

impl ColorEncodingSpec {
    /// Returns true when the encoded signal is display-referred HDR.
    pub fn is_hdr(self) -> bool {
        self.kind == ColorEncodingKind::DisplayHdr
    }

    /// Returns true for display-referred SDR or HDR delivery identities.
    pub fn is_display_referred(self) -> bool {
        matches!(
            self.kind,
            ColorEncodingKind::DisplaySdr | ColorEncodingKind::DisplayHdr
        )
    }

    /// Returns true when samples are already scene-linear.
    pub fn is_scene_linear(self) -> bool {
        self.kind == ColorEncodingKind::SceneLinear
    }

    /// Returns true when this space represents a scene-referred log signal.
    pub fn is_scene_log(self) -> bool {
        self.kind == ColorEncodingKind::SceneLog
    }

    /// Convert the canonical metadata to FFmpeg color tags when those tags are trustworthy.
    pub fn ffmpeg_tags(self) -> Option<FfmpegColorTags> {
        if !matches!(
            self.kind,
            ColorEncodingKind::DisplaySdr | ColorEncodingKind::DisplayHdr
        ) || self.matrix == ColorMatrixCoefficients::Unspecified
        {
            return None;
        }

        Some(FfmpegColorTags {
            color_primaries: self.primaries.ffmpeg_name()?,
            color_trc: self.transfer.ffmpeg_name()?,
            colorspace: self.matrix.ffmpeg_name()?,
        })
    }
}

impl ColorPrimaries {
    fn ffmpeg_name(self) -> Option<&'static str> {
        match self {
            Self::Bt709 => Some("bt709"),
            Self::Bt470Bg => Some("bt470bg"),
            Self::Smpte170M => Some("smpte170m"),
            Self::Bt2020 => Some("bt2020"),
            Self::P3D65 => Some("smpte432"),
            Self::SonySGamut3
            | Self::SonySGamut
            | Self::SonySGamut3Cine
            | Self::ArriWideGamut3
            | Self::ArriWideGamut4
            | Self::CanonCinemaGamutD55
            | Self::PanasonicVGamut
            | Self::RedWideGamutRgb
            | Self::BlackmagicWideGamutGen5
            | Self::DjiDGamut
            | Self::DavinciWideGamut
            | Self::AcesAp0
            | Self::AcesAp1 => None,
        }
    }
}

impl ColorTransferCharacteristic {
    fn ffmpeg_name(self) -> Option<&'static str> {
        match self {
            Self::Bt709 => Some("bt709"),
            Self::Gamma28 => Some("bt470bg"),
            Self::Smpte170M => Some("smpte170m"),
            Self::Srgb => Some("iec61966-2-1"),
            Self::Hlg => Some("arib-std-b67"),
            Self::Pq => Some("smpte2084"),
            Self::Linear
            | Self::AcesCct
            | Self::AppleLog
            | Self::SLog2
            | Self::SLog3
            | Self::ArriLogC3
            | Self::ArriLogC4
            | Self::CanonLog2
            | Self::CanonLog3
            | Self::PanasonicVLog
            | Self::RedLog3G10
            | Self::BlackmagicFilmGen5
            | Self::DjiDLog
            | Self::DavinciIntermediate => None,
        }
    }
}

impl ColorMatrixCoefficients {
    fn ffmpeg_name(self) -> Option<&'static str> {
        match self {
            Self::Bt709 => Some("bt709"),
            Self::Fcc => Some("fcc"),
            Self::Bt470Bg => Some("bt470bg"),
            Self::Smpte170M => Some("smpte170m"),
            Self::Smpte240M => Some("smpte240m"),
            Self::Bt2020NonConstant => Some("bt2020nc"),
            Self::Rgb => Some("rgb"),
            Self::Unspecified => None,
        }
    }
}

/// FFmpeg color tag triplet for standardized delivery spaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FfmpegColorTags {
    /// `-color_primaries` value.
    pub color_primaries: &'static str,
    /// `-color_trc` value.
    pub color_trc: &'static str,
    /// `-colorspace` value.
    pub colorspace: &'static str,
}

/// Linear-light RGBA frame in an explicit rendering working space.
///
/// This is a pure frame contract. Color conversion belongs to the configured
/// OCIO processor and is never inferred from the working-space primaries.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkingRgbaF32Frame {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Linear-light RGBA values. RGB may be negative or exceed 1.0.
    pub data: Vec<[f32; 4]>,
    /// Linear rendering identity for the RGB samples.
    pub color_space: crate::WorkingColorSpace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WaveformMode {
    Luma,
    RgbParade,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WaveformScope {
    pub mode: WaveformMode,
    pub width: usize,
    pub bins: usize,
    pub values: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistogramScope {
    pub bins: usize,
    pub red: Vec<u32>,
    pub green: Vec<u32>,
    pub blue: Vec<u32>,
    pub luma: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorscopeSample {
    pub u: f32,
    pub v: f32,
    pub weight: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColorScopes {
    pub histogram: HistogramScope,
    pub waveform: WaveformScope,
    pub vectorscope: Vec<VectorscopeSample>,
}

pub fn compute_color_scopes(
    rgba: &[u8],
    width: u32,
    height: u32,
    waveform_mode: WaveformMode,
    bins: usize,
) -> ColorScopes {
    let bins = bins.clamp(16, 1024);
    let width_usize = width.max(1) as usize;
    let expected_pixels = width as usize * height as usize;
    let mut histogram = HistogramScope {
        bins,
        red: vec![0; bins],
        green: vec![0; bins],
        blue: vec![0; bins],
        luma: vec![0; bins],
    };
    let waveform_channels = match waveform_mode {
        WaveformMode::Luma => 1,
        WaveformMode::RgbParade => 3,
    };
    let mut waveform = WaveformScope {
        mode: waveform_mode,
        width: width_usize,
        bins,
        values: vec![0; width_usize * bins * waveform_channels],
    };
    let mut vectors = vec![VectorscopeSample { u: 0.0, v: 0.0, weight: 0 }; 64 * 64];

    for (idx, px) in rgba.chunks_exact(4).take(expected_pixels).enumerate() {
        let x = idx % width_usize;
        let r = px[0] as f32 / 255.0;
        let g = px[1] as f32 / 255.0;
        let b = px[2] as f32 / 255.0;
        let y = luma(r, g, b).clamp(0.0, 1.0);
        let rb = scope_bin(r, bins);
        let gb = scope_bin(g, bins);
        let bb = scope_bin(b, bins);
        let yb = scope_bin(y, bins);
        histogram.red[rb] += 1;
        histogram.green[gb] += 1;
        histogram.blue[bb] += 1;
        histogram.luma[yb] += 1;

        match waveform_mode {
            WaveformMode::Luma => waveform.values[x * bins + yb] += 1,
            WaveformMode::RgbParade => {
                let plane = width_usize * bins;
                waveform.values[x * bins + rb] += 1;
                waveform.values[plane + x * bins + gb] += 1;
                waveform.values[plane * 2 + x * bins + bb] += 1;
            }
        }

        let u = (b - y) * 0.565;
        let v = (r - y) * 0.713;
        let ux = ((u + 0.5).clamp(0.0, 0.999) * 64.0) as usize;
        let vy = ((v + 0.5).clamp(0.0, 0.999) * 64.0) as usize;
        let sample = &mut vectors[vy * 64 + ux];
        sample.u = (ux as f32 + 0.5) / 64.0 - 0.5;
        sample.v = (vy as f32 + 0.5) / 64.0 - 0.5;
        sample.weight = sample.weight.saturating_add(1);
    }

    ColorScopes {
        histogram,
        waveform,
        vectorscope: vectors.into_iter().filter(|sample| sample.weight > 0).collect(),
    }
}

impl ColorSpace {
    /// All product-supported Mondrian color spaces.
    pub const ALL: [Self; 27] = [
        Self::Rec709,
        Self::Rec601Pal,
        Self::Rec601Ntsc,
        Self::Rec2100Hlg,
        Self::Rec2100Pq,
        Self::Srgb,
        Self::Rec2020,
        Self::DisplayP3,
        Self::LinearRec709,
        Self::LinearRec2020,
        Self::LinearP3D65,
        Self::Aces2065_1,
        Self::AcesCg,
        Self::AcesCct,
        Self::AppleLogBt2020,
        Self::SonySLog2SGamut,
        Self::SonySLog3SGamut3,
        Self::SonySLog3SGamut3Cine,
        Self::ArriLogC3WideGamut3,
        Self::ArriLogC4WideGamut4,
        Self::CanonLog2CinemaGamutD55,
        Self::CanonLog3CinemaGamutD55,
        Self::PanasonicVLogVGamut,
        Self::RedLog3G10WideGamutRgb,
        Self::BlackmagicFilmWideGamutGen5,
        Self::DjiDLogDGamut,
        Self::DavinciIntermediateWideGamut,
    ];

    /// Canonical encoding metadata used by preview diagnostics and export tagging.
    pub fn encoding(self) -> ColorEncodingSpec {
        match self {
            Self::Rec709 => ColorEncodingSpec {
                primaries: ColorPrimaries::Bt709,
                transfer: ColorTransferCharacteristic::Bt709,
                matrix: ColorMatrixCoefficients::Bt709,
                kind: ColorEncodingKind::DisplaySdr,
            },
            Self::Rec601Pal => ColorEncodingSpec {
                primaries: ColorPrimaries::Bt470Bg,
                transfer: ColorTransferCharacteristic::Gamma28,
                matrix: ColorMatrixCoefficients::Bt470Bg,
                kind: ColorEncodingKind::DisplaySdr,
            },
            Self::Rec601Ntsc => ColorEncodingSpec {
                primaries: ColorPrimaries::Smpte170M,
                transfer: ColorTransferCharacteristic::Smpte170M,
                matrix: ColorMatrixCoefficients::Smpte170M,
                kind: ColorEncodingKind::DisplaySdr,
            },
            Self::Rec2100Hlg => ColorEncodingSpec {
                primaries: ColorPrimaries::Bt2020,
                transfer: ColorTransferCharacteristic::Hlg,
                matrix: ColorMatrixCoefficients::Bt2020NonConstant,
                kind: ColorEncodingKind::DisplayHdr,
            },
            Self::Rec2100Pq => ColorEncodingSpec {
                primaries: ColorPrimaries::Bt2020,
                transfer: ColorTransferCharacteristic::Pq,
                matrix: ColorMatrixCoefficients::Bt2020NonConstant,
                kind: ColorEncodingKind::DisplayHdr,
            },
            Self::Srgb => ColorEncodingSpec {
                primaries: ColorPrimaries::Bt709,
                transfer: ColorTransferCharacteristic::Srgb,
                matrix: ColorMatrixCoefficients::Rgb,
                kind: ColorEncodingKind::DisplaySdr,
            },
            Self::Rec2020 => ColorEncodingSpec {
                primaries: ColorPrimaries::Bt2020,
                transfer: ColorTransferCharacteristic::Bt709,
                matrix: ColorMatrixCoefficients::Bt2020NonConstant,
                kind: ColorEncodingKind::DisplaySdr,
            },
            Self::DisplayP3 => ColorEncodingSpec {
                primaries: ColorPrimaries::P3D65,
                transfer: ColorTransferCharacteristic::Srgb,
                matrix: ColorMatrixCoefficients::Rgb,
                kind: ColorEncodingKind::DisplaySdr,
            },
            Self::LinearRec709 => ColorEncodingSpec {
                primaries: ColorPrimaries::Bt709,
                transfer: ColorTransferCharacteristic::Linear,
                matrix: ColorMatrixCoefficients::Rgb,
                kind: ColorEncodingKind::SceneLinear,
            },
            Self::LinearRec2020 => ColorEncodingSpec {
                primaries: ColorPrimaries::Bt2020,
                transfer: ColorTransferCharacteristic::Linear,
                matrix: ColorMatrixCoefficients::Rgb,
                kind: ColorEncodingKind::SceneLinear,
            },
            Self::LinearP3D65 => ColorEncodingSpec {
                primaries: ColorPrimaries::P3D65,
                transfer: ColorTransferCharacteristic::Linear,
                matrix: ColorMatrixCoefficients::Rgb,
                kind: ColorEncodingKind::SceneLinear,
            },
            Self::Aces2065_1 => ColorEncodingSpec {
                primaries: ColorPrimaries::AcesAp0,
                transfer: ColorTransferCharacteristic::Linear,
                matrix: ColorMatrixCoefficients::Rgb,
                kind: ColorEncodingKind::SceneLinear,
            },
            Self::AcesCg => ColorEncodingSpec {
                primaries: ColorPrimaries::AcesAp1,
                transfer: ColorTransferCharacteristic::Linear,
                matrix: ColorMatrixCoefficients::Rgb,
                kind: ColorEncodingKind::SceneLinear,
            },
            Self::AcesCct => ColorEncodingSpec {
                primaries: ColorPrimaries::AcesAp1,
                transfer: ColorTransferCharacteristic::AcesCct,
                matrix: ColorMatrixCoefficients::Rgb,
                kind: ColorEncodingKind::SceneLog,
            },
            Self::AppleLogBt2020 => ColorEncodingSpec {
                primaries: ColorPrimaries::Bt2020,
                transfer: ColorTransferCharacteristic::AppleLog,
                matrix: ColorMatrixCoefficients::Unspecified,
                kind: ColorEncodingKind::SceneLog,
            },
            Self::SonySLog2SGamut => ColorEncodingSpec {
                primaries: ColorPrimaries::SonySGamut,
                transfer: ColorTransferCharacteristic::SLog2,
                matrix: ColorMatrixCoefficients::Unspecified,
                kind: ColorEncodingKind::SceneLog,
            },
            Self::SonySLog3SGamut3 => ColorEncodingSpec {
                primaries: ColorPrimaries::SonySGamut3,
                transfer: ColorTransferCharacteristic::SLog3,
                matrix: ColorMatrixCoefficients::Unspecified,
                kind: ColorEncodingKind::SceneLog,
            },
            Self::SonySLog3SGamut3Cine => ColorEncodingSpec {
                primaries: ColorPrimaries::SonySGamut3Cine,
                transfer: ColorTransferCharacteristic::SLog3,
                matrix: ColorMatrixCoefficients::Unspecified,
                kind: ColorEncodingKind::SceneLog,
            },
            Self::ArriLogC3WideGamut3 => ColorEncodingSpec {
                primaries: ColorPrimaries::ArriWideGamut3,
                transfer: ColorTransferCharacteristic::ArriLogC3,
                matrix: ColorMatrixCoefficients::Unspecified,
                kind: ColorEncodingKind::SceneLog,
            },
            Self::ArriLogC4WideGamut4 => ColorEncodingSpec {
                primaries: ColorPrimaries::ArriWideGamut4,
                transfer: ColorTransferCharacteristic::ArriLogC4,
                matrix: ColorMatrixCoefficients::Unspecified,
                kind: ColorEncodingKind::SceneLog,
            },
            Self::CanonLog2CinemaGamutD55 => ColorEncodingSpec {
                primaries: ColorPrimaries::CanonCinemaGamutD55,
                transfer: ColorTransferCharacteristic::CanonLog2,
                matrix: ColorMatrixCoefficients::Unspecified,
                kind: ColorEncodingKind::SceneLog,
            },
            Self::CanonLog3CinemaGamutD55 => ColorEncodingSpec {
                primaries: ColorPrimaries::CanonCinemaGamutD55,
                transfer: ColorTransferCharacteristic::CanonLog3,
                matrix: ColorMatrixCoefficients::Unspecified,
                kind: ColorEncodingKind::SceneLog,
            },
            Self::PanasonicVLogVGamut => ColorEncodingSpec {
                primaries: ColorPrimaries::PanasonicVGamut,
                transfer: ColorTransferCharacteristic::PanasonicVLog,
                matrix: ColorMatrixCoefficients::Unspecified,
                kind: ColorEncodingKind::SceneLog,
            },
            Self::RedLog3G10WideGamutRgb => ColorEncodingSpec {
                primaries: ColorPrimaries::RedWideGamutRgb,
                transfer: ColorTransferCharacteristic::RedLog3G10,
                matrix: ColorMatrixCoefficients::Unspecified,
                kind: ColorEncodingKind::SceneLog,
            },
            Self::BlackmagicFilmWideGamutGen5 => ColorEncodingSpec {
                primaries: ColorPrimaries::BlackmagicWideGamutGen5,
                transfer: ColorTransferCharacteristic::BlackmagicFilmGen5,
                matrix: ColorMatrixCoefficients::Unspecified,
                kind: ColorEncodingKind::SceneLog,
            },
            Self::DjiDLogDGamut => ColorEncodingSpec {
                primaries: ColorPrimaries::DjiDGamut,
                transfer: ColorTransferCharacteristic::DjiDLog,
                matrix: ColorMatrixCoefficients::Unspecified,
                kind: ColorEncodingKind::SceneLog,
            },
            Self::DavinciIntermediateWideGamut => ColorEncodingSpec {
                primaries: ColorPrimaries::DavinciWideGamut,
                transfer: ColorTransferCharacteristic::DavinciIntermediate,
                matrix: ColorMatrixCoefficients::Unspecified,
                kind: ColorEncodingKind::SceneLog,
            },
        }
    }

    pub fn is_hdr(self) -> bool {
        self.encoding().is_hdr()
    }

    /// Whether this color space is valid as a sequence presentation destination.
    pub fn is_display_referred(self) -> bool {
        self.encoding().is_display_referred()
    }

    /// Whether this source identity carries scene-linear samples.
    pub fn is_scene_linear(self) -> bool {
        self.encoding().is_scene_linear()
    }

    /// FFmpeg tag triplet for standardized delivery spaces.
    ///
    /// Scene-linear and scene-log acquisition spaces return `None`; exporting
    /// them with guessed delivery tags would mislabel the payload.
    pub fn ffmpeg_tags(self) -> Option<FfmpegColorTags> {
        self.encoding().ffmpeg_tags()
    }

    /// Resolve a color space from an exact FFmpeg color-tag triplet.
    ///
    /// Scene-linear and scene-log acquisition spaces intentionally do not match
    /// here because Mondrian does not emit delivery tags for them.
    pub fn from_ffmpeg_tags(
        color_primaries: &str,
        color_trc: &str,
        colorspace: &str,
    ) -> Option<Self> {
        Self::ALL.into_iter().find(|color_space| {
            color_space.ffmpeg_tags().is_some_and(|tags| {
                ffmpeg_tag_eq(tags.color_primaries, color_primaries)
                    && ffmpeg_tag_eq(tags.color_trc, color_trc)
                    && ffmpeg_tag_eq(tags.colorspace, colorspace)
            })
        })
    }

    /// Resolve a color space from partial FFmpeg tag hints.
    ///
    /// This is used for media metadata interpretation where some containers
    /// provide only transfer, primaries, or matrix tags. Exact triplets win;
    /// partial matches are deliberately centralized here so media probing does
    /// not carry its own color-space knowledge table.
    pub fn from_ffmpeg_tag_hints(
        color_primaries: Option<&str>,
        color_trc: Option<&str>,
        colorspace: Option<&str>,
    ) -> Option<Self> {
        if let (Some(primaries), Some(transfer), Some(matrix)) =
            (color_primaries, color_trc, colorspace)
        {
            if let Some(color_space) = Self::from_ffmpeg_tags(primaries, transfer, matrix) {
                return Some(color_space);
            }
        }

        if color_primaries.is_some_and(|tag| ffmpeg_tag_eq(tag, "smpte432"))
            && color_trc.is_none_or(|tag| ffmpeg_tag_eq(tag, "iec61966-2-1"))
        {
            return Some(Self::DisplayP3);
        }

        match color_trc {
            Some("smpte2084") => Some(Self::Rec2100Pq),
            Some("arib-std-b67") => Some(Self::Rec2100Hlg),
            Some("iec61966-2-1") => Some(Self::Srgb),
            Some("bt470bg") | Some("gamma28") => Some(Self::Rec601Pal),
            Some("smpte170m") => Some(Self::Rec601Ntsc),
            _ => match color_primaries {
                Some("bt2020") => Some(Self::Rec2020),
                Some("smpte432") => Some(Self::DisplayP3),
                Some("bt709") => {
                    if colorspace.is_some_and(|tag| ffmpeg_tag_eq(tag, "rgb")) {
                        Some(Self::Srgb)
                    } else {
                        Some(Self::Rec709)
                    }
                }
                Some("bt470bg") => Some(Self::Rec601Pal),
                Some("smpte170m") => Some(Self::Rec601Ntsc),
                _ => match colorspace {
                    Some("bt709") => Some(Self::Rec709),
                    Some("bt470bg") => Some(Self::Rec601Pal),
                    Some("smpte170m") => Some(Self::Rec601Ntsc),
                    Some("bt2020nc") | Some("bt2020c") => Some(Self::Rec2020),
                    Some(tag) if ffmpeg_tag_eq(tag, "rgb") => Some(Self::Srgb),
                    _ => None,
                },
            },
        }
    }
}

fn ffmpeg_tag_eq(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right) || matches!((left, right), ("rgb", "gbr") | ("gbr", "rgb"))
}

fn luma(r: f32, g: f32, b: f32) -> f32 {
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

fn scope_bin(v: f32, bins: usize) -> usize {
    (v.clamp(0.0, 1.0) * (bins.saturating_sub(1)) as f32).round() as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_space_encoding_contract_covers_delivery_and_log_spaces() {
        let rec709 = ColorSpace::Rec709.encoding();
        assert_eq!(rec709.primaries, ColorPrimaries::Bt709);
        assert_eq!(rec709.transfer, ColorTransferCharacteristic::Bt709);
        assert_eq!(rec709.matrix, ColorMatrixCoefficients::Bt709);
        assert_eq!(rec709.kind, ColorEncodingKind::DisplaySdr);
        assert!(!rec709.is_hdr());
        assert!(!rec709.is_scene_log());

        let pq = ColorSpace::Rec2100Pq.encoding();
        assert_eq!(pq.primaries, ColorPrimaries::Bt2020);
        assert_eq!(pq.transfer, ColorTransferCharacteristic::Pq);
        assert_eq!(pq.matrix, ColorMatrixCoefficients::Bt2020NonConstant);
        assert_eq!(pq.kind, ColorEncodingKind::DisplayHdr);
        assert!(pq.is_hdr());

        let display_p3 = ColorSpace::DisplayP3.encoding();
        assert_eq!(display_p3.primaries, ColorPrimaries::P3D65);
        assert_eq!(display_p3.transfer, ColorTransferCharacteristic::Srgb);
        assert_eq!(display_p3.matrix, ColorMatrixCoefficients::Rgb);
        assert_eq!(display_p3.kind, ColorEncodingKind::DisplaySdr);

        let aces2065 = ColorSpace::Aces2065_1.encoding();
        assert_eq!(aces2065.primaries, ColorPrimaries::AcesAp0);
        assert_eq!(aces2065.transfer, ColorTransferCharacteristic::Linear);
        assert_eq!(aces2065.kind, ColorEncodingKind::SceneLinear);
        assert!(aces2065.is_scene_linear());
        assert!(!aces2065.is_display_referred());

        let acescct = ColorSpace::AcesCct.encoding();
        assert_eq!(acescct.primaries, ColorPrimaries::AcesAp1);
        assert_eq!(acescct.transfer, ColorTransferCharacteristic::AcesCct);
        assert!(acescct.is_scene_log());

        let slog3 = ColorSpace::SonySLog3SGamut3Cine.encoding();
        assert_eq!(slog3.kind, ColorEncodingKind::SceneLog);
        assert_eq!(slog3.matrix, ColorMatrixCoefficients::Unspecified);
        assert!(slog3.is_scene_log());
        assert!(!slog3.is_hdr());
    }

    #[test]
    fn ffmpeg_tags_are_only_emitted_for_standardized_delivery_spaces() {
        let rec2020 = ColorSpace::Rec2020.ffmpeg_tags().expect("rec2020 has delivery tags");
        assert_eq!(rec2020.color_primaries, "bt2020");
        assert_eq!(rec2020.color_trc, "bt709");
        assert_eq!(rec2020.colorspace, "bt2020nc");

        let srgb = ColorSpace::Srgb.ffmpeg_tags().expect("srgb has delivery tags");
        assert_eq!(srgb.color_primaries, "bt709");
        assert_eq!(srgb.color_trc, "iec61966-2-1");
        assert_eq!(srgb.colorspace, "rgb");

        let display_p3 = ColorSpace::DisplayP3.ffmpeg_tags().expect("Display P3 has delivery tags");
        assert_eq!(display_p3.color_primaries, "smpte432");
        assert_eq!(display_p3.color_trc, "iec61966-2-1");
        assert_eq!(display_p3.colorspace, "rgb");

        let pal = ColorSpace::Rec601Pal.ffmpeg_tags().expect("PAL has CICP tags");
        assert_eq!(pal.color_primaries, "bt470bg");
        assert_eq!(pal.color_trc, "bt470bg");
        assert_eq!(pal.colorspace, "bt470bg");

        let ntsc = ColorSpace::Rec601Ntsc.ffmpeg_tags().expect("NTSC has CICP tags");
        assert_eq!(ntsc.color_primaries, "smpte170m");
        assert_eq!(ntsc.color_trc, "smpte170m");
        assert_eq!(ntsc.colorspace, "smpte170m");

        assert_eq!(ColorSpace::AppleLogBt2020.ffmpeg_tags(), None);
        assert_eq!(ColorSpace::SonySLog3SGamut3Cine.ffmpeg_tags(), None);
        assert_eq!(ColorSpace::ArriLogC4WideGamut4.ffmpeg_tags(), None);
    }

    #[test]
    fn color_space_can_resolve_exact_ffmpeg_tag_triplets() {
        assert_eq!(
            ColorSpace::from_ffmpeg_tags("bt2020", "smpte2084", "bt2020nc"),
            Some(ColorSpace::Rec2100Pq)
        );
        assert_eq!(
            ColorSpace::from_ffmpeg_tags("bt709", "iec61966-2-1", "rgb"),
            Some(ColorSpace::Srgb)
        );
        assert_eq!(
            ColorSpace::from_ffmpeg_tags("bt709", "iec61966-2-1", "gbr"),
            Some(ColorSpace::Srgb)
        );
        assert_eq!(
            ColorSpace::from_ffmpeg_tags("smpte432", "iec61966-2-1", "rgb"),
            Some(ColorSpace::DisplayP3)
        );
        assert_eq!(ColorSpace::from_ffmpeg_tags("bt709", "bt709", "rgb"), None);
        assert_eq!(
            ColorSpace::from_ffmpeg_tags("bt470bg", "bt470bg", "bt470bg"),
            Some(ColorSpace::Rec601Pal)
        );
        assert_eq!(
            ColorSpace::from_ffmpeg_tags("smpte170m", "smpte170m", "smpte170m"),
            Some(ColorSpace::Rec601Ntsc)
        );
    }

    #[test]
    fn color_space_resolves_partial_ffmpeg_tag_hints_centrally() {
        assert_eq!(
            ColorSpace::from_ffmpeg_tag_hints(Some("smpte432"), Some("iec61966-2-1"), None),
            Some(ColorSpace::DisplayP3)
        );
        assert_eq!(
            ColorSpace::from_ffmpeg_tag_hints(Some("smpte431"), None, None),
            None
        );
        assert_eq!(
            ColorSpace::from_ffmpeg_tag_hints(None, Some("smpte2084"), None),
            Some(ColorSpace::Rec2100Pq)
        );
        assert_eq!(
            ColorSpace::from_ffmpeg_tag_hints(Some("bt709"), None, Some("rgb")),
            Some(ColorSpace::Srgb)
        );
        assert_eq!(
            ColorSpace::from_ffmpeg_tag_hints(None, None, Some("bt709")),
            Some(ColorSpace::Rec709)
        );
        assert_eq!(
            ColorSpace::from_ffmpeg_tag_hints(None, None, Some("bt470bg")),
            Some(ColorSpace::Rec601Pal)
        );
        assert_eq!(
            ColorSpace::from_ffmpeg_tag_hints(None, None, Some("smpte170m")),
            Some(ColorSpace::Rec601Ntsc)
        );
        assert_eq!(ColorSpace::from_ffmpeg_tag_hints(None, None, None), None);
    }

    #[test]
    fn standard_engine_uses_typed_ocio_identity_without_byte_boundary() {
        let engine = ColorEngine::mondrian_standard();
        let mut rgba = vec![0.5_f32, 0.5, 0.5, 0.25];

        engine
            .convert_identity_float(
                &mut rgba,
                OcioColorSpaceIdentity::Color(ColorSpace::Rec709),
                OcioColorSpaceIdentity::Working(crate::types::WorkingColorSpace::LinearRec709),
            )
            .expect("embedded OCIO source-to-working processor");

        assert!(engine.is_available());
        assert!(rgba[0] < 0.5);
        assert!((rgba[0] - rgba[1]).abs() < 1.0e-6);
        assert!((rgba[1] - rgba[2]).abs() < 1.0e-6);
        assert!((rgba[3] - 0.25).abs() < 1.0e-6);
    }

    #[test]
    fn explicit_ocio_engine_reports_missing_config_without_mutating_pixels() {
        let missing_path = std::env::temp_dir().join(format!(
            "mondrian-missing-ocio-config-{}.ocio",
            std::process::id()
        ));
        let engine = ColorEngine::CustomOcio {
            source: crate::types::OcioConfigSource::Path { path: missing_path },
        };
        let mut rgba = vec![0.1_f32, 0.2, 0.3, 0.4];
        let original = rgba.clone();

        let err = engine
            .convert_identity_float(
                &mut rgba,
                OcioColorSpaceIdentity::Color(ColorSpace::Rec709),
                OcioColorSpaceIdentity::Working(crate::types::WorkingColorSpace::LinearRec709),
            )
            .expect_err("explicit missing OCIO source must fail closed");

        assert!(err.contains("OCIO config file not found"));
        assert_eq!(rgba, original);
    }

    #[test]
    fn scopes_count_pixels_and_rgb_parade_channels() {
        let rgba = vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 0, 0, 0, 255];
        let scopes = compute_color_scopes(&rgba, 2, 2, WaveformMode::RgbParade, 16);
        assert_eq!(scopes.histogram.red.iter().sum::<u32>(), 4);
        assert_eq!(scopes.histogram.green.iter().sum::<u32>(), 4);
        assert_eq!(scopes.histogram.blue.iter().sum::<u32>(), 4);
        assert_eq!(scopes.histogram.luma.iter().sum::<u32>(), 4);
        assert_eq!(scopes.waveform.values.iter().sum::<u32>(), 12);
        assert!(!scopes.vectorscope.is_empty());
    }
}
