//! Color management primitives shared by preview, render and export.

use crate::types::{
    ColorEngine, ColorSpace, OcioColorSpaceIdentity, OcioConfigSource, WorkingColorSpace,
};
use serde::{Deserialize, Serialize};

// ── ColorEngine: centralized dispatch ─────────────────────────────────────────

impl ColorEngine {
    /// Resolve the exact OCIO config source owned by this product mode.
    pub fn ocio_source(&self) -> crate::types::OcioConfigSource {
        match self {
            Self::MondrianStandard { package } => {
                crate::types::OcioConfigSource::MondrianStandard { package: *package }
            }
            Self::Aces { preset } => preset.ocio_source(),
            Self::CustomOcio { identity } => identity.source().clone(),
        }
    }

    /// Apply a float OCIO processor between explicit encoded/working identities.
    pub fn convert_identity_float(
        &self,
        data: &mut [f32],
        src: OcioColorSpaceIdentity,
        dst: OcioColorSpaceIdentity,
    ) -> Result<(), String> {
        crate::ocio::apply_ocio_identity_float(self, data, src, dst)
    }

    /// Apply an OCIO display/view processor from an explicit source identity.
    pub fn display_transform_identity_float(
        &self,
        data: &mut [f32],
        src: OcioColorSpaceIdentity,
        display: &str,
        view: &str,
    ) -> Result<(), String> {
        crate::ocio::apply_ocio_display_identity_float(self, data, src, display, view)
    }

    /// Whether the engine is ready to process data.
    pub fn is_available(&self) -> bool {
        crate::ocio::ocio_engine_is_validated(self)
    }

    /// Ensure any required external config is loaded.
    pub fn ensure_loaded(&self) -> Result<(), String> {
        match self {
            Self::MondrianStandard { .. } => crate::ocio::ensure_color_engine_ocio_loaded(self),
            Self::Aces { preset } => crate::ocio::ensure_ocio_loaded(&preset.ocio_source()),
            Self::CustomOcio { .. } => crate::ocio::ensure_color_engine_ocio_loaded(self),
        }
    }

    /// Load this engine's exact OCIO config and resolve its unqualified default display/view.
    ///
    /// Custom OCIO deliberately rejects this query because its output bindings
    /// must always be selected by standardized target identity.
    pub fn default_display_view(&self) -> Result<(String, String), String> {
        match self {
            Self::Aces { preset } => {
                let (display, view) = preset.default_display_view();
                return Ok((display.to_owned(), view.to_owned()));
            }
            Self::CustomOcio { .. } => {
                return Err(
                    "Custom OCIO display/view resolution requires an explicit output target"
                        .to_owned(),
                );
            }
            Self::MondrianStandard { .. } => {}
        }
        crate::ocio::ocio_default_display_view_for_engine(self)?
            .ok_or_else(|| format!("{} config has no default display/view", self.name()))
    }

    /// Resolve the exact engine-owned View for a standardized encoded output target.
    pub fn output_display_view(
        &self,
        output_color_space: ColorSpace,
    ) -> Result<(String, String), String> {
        match self {
            Self::MondrianStandard { package } => {
                crate::ocio::mondrian_standard_output_display_view_for_package(
                    *package,
                    output_color_space,
                )
            }
            Self::Aces { preset } => preset
                .output_display_view(output_color_space)
                .map(|(display, view)| (display.to_owned(), view.to_owned()))
                .ok_or_else(|| {
                    format!(
                        "ACES preset '{}' has no output View for {output_color_space:?}",
                        preset.builtin_name()
                    )
                }),
            Self::CustomOcio { identity } => identity
                .output(output_color_space)
                .map(|output| (output.display().to_owned(), output.view().to_owned()))
                .ok_or_else(|| {
                    format!("Custom OCIO project has no output binding for {output_color_space:?}")
                }),
        }
    }

    /// Return display names from this engine's exact OCIO config.
    pub fn display_names(&self) -> Result<Vec<String>, String> {
        crate::ocio::ocio_display_names_for_engine(self)
    }

    /// Return view names under a display from this engine's exact OCIO config.
    pub fn view_names(&self, display: &str) -> Result<Vec<String>, String> {
        crate::ocio::ocio_view_names_for_engine(self, display)
    }

    /// Return one display's default view from this engine's exact OCIO config.
    pub fn default_view_for_display(&self, display: &str) -> Result<Option<String>, String> {
        crate::ocio::ocio_default_view_for_display_for_engine(self, display)
    }

    /// Human-readable name for diagnostics / UI.
    pub fn name(&self) -> &'static str {
        match self {
            Self::MondrianStandard { .. } => "Mondrian Standard",
            Self::Aces { .. } => "ACES",
            Self::CustomOcio { .. } => "Custom OpenColorIO",
        }
    }

    /// Resolve and pin a Custom OCIO config plus its project color semantics.
    ///
    /// This constructor reads and validates the selected config immediately;
    /// a bare path is never persisted as a complete project mode.
    pub fn custom_ocio(
        source: OcioConfigSource,
        working_space: WorkingColorSpace,
        output_color_space: ColorSpace,
        display: impl Into<String>,
        view: impl Into<String>,
    ) -> Result<Self, String> {
        crate::ocio::pin_custom_ocio_project(
            source,
            working_space,
            output_color_space,
            display.into(),
            view.into(),
        )
    }

    /// Resolve and pin a Custom OCIO config for one standardized output target.
    ///
    /// Auto-selection succeeds only when the config exposes a uniquely
    /// target-compatible display color space. Ambiguous or unknown semantics
    /// require [`Self::custom_ocio`] with an explicit display/view declaration.
    pub fn custom_ocio_for_output(
        source: OcioConfigSource,
        working_space: WorkingColorSpace,
        output_color_space: ColorSpace,
    ) -> Result<Self, String> {
        crate::ocio::pin_custom_ocio_project_for_output(source, working_space, output_color_space)
    }

    /// Return the single working space pinned by this product mode, if any.
    ///
    /// Mondrian Standard pins the space declared by its immutable package.
    /// Custom OCIO pins the space covered by its saved processor graph. ACES
    /// keeps working-space selection explicit because its official config
    /// exposes multiple supported scene-linear routes.
    pub fn pinned_working_space(&self) -> Option<WorkingColorSpace> {
        match self {
            Self::MondrianStandard { package } => Some(package.working_color_space()),
            Self::Aces { .. } => None,
            Self::CustomOcio { identity } => [
                WorkingColorSpace::LinearRec709,
                WorkingColorSpace::LinearRec2020,
                WorkingColorSpace::LinearP3D65,
                WorkingColorSpace::AcesCg,
            ]
            .into_iter()
            .find(|working| {
                crate::ocio::ocio_working_color_space_name(*working) == identity.working_space()
            }),
        }
    }

    /// Validate a sequence working space against this engine's project identity.
    ///
    /// Standard packages and Custom OCIO identities cover exactly one
    /// project working-space contract; allowing a different sequence space
    /// would execute semantics outside the persisted identity.
    pub fn validate_working_space(&self, working_space: WorkingColorSpace) -> Result<(), String> {
        let actual = crate::ocio::ocio_working_color_space_name(working_space);
        match self {
            Self::MondrianStandard { package } => {
                let expected =
                    crate::ocio::ocio_working_color_space_name(package.working_color_space());
                if actual == expected {
                    Ok(())
                } else {
                    Err(format!(
                        "Mondrian Standard package '{}' pins working space '{expected}', not '{actual}'",
                        package.working_space_id()
                    ))
                }
            }
            Self::Aces { .. } => Ok(()),
            Self::CustomOcio { identity } => {
                if actual == identity.working_space() {
                    Ok(())
                } else {
                    Err(format!(
                        "Custom OCIO project pins working space '{}', not '{actual}'",
                        identity.working_space()
                    ))
                }
            }
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
    /// One encoded-signal luma trace.
    Luma,
    /// Separate encoded red, green, and blue traces.
    RgbParade,
}

/// Horizontal program-signal waveform density.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WaveformScope {
    /// Signal component layout.
    pub mode: WaveformMode,
    /// Number of horizontal source columns.
    pub width: usize,
    /// Number of vertical signal bins.
    pub bins: usize,
    /// Row-major sample density, with RGB parade planes stored consecutively.
    pub values: Vec<u32>,
}

/// RGB and luma histograms for one program-output frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistogramScope {
    /// Number of bins per component.
    pub bins: usize,
    /// Encoded red-channel counts.
    pub red: Vec<u32>,
    /// Encoded green-channel counts.
    pub green: Vec<u32>,
    /// Encoded blue-channel counts.
    pub blue: Vec<u32>,
    /// Non-constant-luminance encoded luma counts for the declared signal primaries.
    pub luma: Vec<u32>,
}

/// One occupied cell in the normalized program-signal vectorscope grid.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorscopeSample {
    /// Normalized blue-difference coordinate in `[-0.5, 0.5]`.
    pub u: f32,
    /// Normalized red-difference coordinate in `[-0.5, 0.5]`.
    pub v: f32,
    /// Number of pixels accumulated into this cell.
    pub weight: u32,
}

/// Counts outside the normalized `0..=1` Program Output signal interval.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgramSignalExcursionCounts {
    /// Samples below nominal black.
    pub below_nominal: u64,
    /// Samples above nominal peak signal.
    pub above_nominal: u64,
}

impl ProgramSignalExcursionCounts {
    fn observe(&mut self, value: f32) {
        if value < 0.0 {
            self.below_nominal = self.below_nominal.saturating_add(1);
        } else if value > 1.0 {
            self.above_nominal = self.above_nominal.saturating_add(1);
        }
    }
}

/// Per-component signal excursions preserved alongside endpoint scope bins.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgramSignalExcursions {
    /// Encoded red excursions.
    pub red: ProgramSignalExcursionCounts,
    /// Encoded green excursions.
    pub green: ProgramSignalExcursionCounts,
    /// Encoded blue excursions.
    pub blue: ProgramSignalExcursionCounts,
    /// Encoded non-constant-luminance luma excursions.
    pub luma: ProgramSignalExcursionCounts,
}

/// Video scopes measured from a single display-encoded Program Output frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColorScopes {
    /// Exact standardized signal color space used for luma/chroma math.
    pub signal_color_space: ColorSpace,
    /// Number of RGBA pixels measured.
    pub sample_count: u64,
    /// Counts hidden by endpoint binning, including negative signal and superwhite.
    pub excursions: ProgramSignalExcursions,
    /// Per-component and luma distributions.
    pub histogram: HistogramScope,
    /// Horizontal signal distribution.
    pub waveform: WaveformScope,
    /// Occupied normalized chroma cells.
    pub vectorscope: Vec<VectorscopeSample>,
}

/// Failure to measure a buffer as an encoded Program Output signal.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProgramColorScopeError {
    /// A frame must contain at least one pixel in each dimension.
    #[error("program scope dimensions must be non-zero, got {width}x{height}")]
    InvalidDimensions {
        /// Declared width.
        width: u32,
        /// Declared height.
        height: u32,
    },
    /// The RGBA8 payload does not match the declared dimensions exactly.
    #[error("program scope RGBA8 byte length mismatch: expected {expected}, got {actual}")]
    Rgba8ByteLengthMismatch {
        /// Required byte count.
        expected: usize,
        /// Supplied byte count.
        actual: usize,
    },
    /// The float payload does not match the declared pixel count exactly.
    #[error("program scope float pixel length mismatch: expected {expected}, got {actual}")]
    FloatPixelLengthMismatch {
        /// Required pixel count.
        expected: usize,
        /// Supplied pixel count.
        actual: usize,
    },
    /// The declared frame dimensions cannot be represented by the host.
    #[error("program scope dimensions overflow host address space: {width}x{height}")]
    DimensionsOverflow {
        /// Declared width.
        width: u32,
        /// Declared height.
        height: u32,
    },
    /// The declared identity is not a supported display-encoded output signal.
    #[error("{color_space:?} is not a supported program-output signal color space")]
    UnsupportedSignalColorSpace {
        /// Rejected identity.
        color_space: ColorSpace,
    },
    /// Float scopes refuse invalid values rather than silently binning them.
    #[error("program scope pixel {pixel} channel {channel} is not finite")]
    NonFiniteSample {
        /// Zero-based pixel index.
        pixel: usize,
        /// RGB channel index (`0 = R`, `1 = G`, `2 = B`).
        channel: usize,
    },
}

/// Non-constant-luminance coefficients for one display-encoded signal space.
///
/// CPU and GPU scopes share this contract so their luma and chroma axes cannot
/// silently diverge.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProgramSignalColorimetry {
    kr: f32,
    kb: f32,
}

impl ProgramSignalColorimetry {
    /// Resolve standardized luma coefficients for a scope-compatible signal.
    pub fn for_color_space(color_space: ColorSpace) -> Result<Self, ProgramColorScopeError> {
        let colorimetry = match color_space {
            ColorSpace::Rec601Pal | ColorSpace::Rec601Ntsc => Self { kr: 0.299, kb: 0.114 },
            ColorSpace::Rec709 | ColorSpace::Srgb => Self { kr: 0.2126, kb: 0.0722 },
            ColorSpace::DisplayP3 => Self { kr: 0.228_974_6, kb: 0.079_286_9 },
            ColorSpace::Rec2020 | ColorSpace::Rec2100Hlg | ColorSpace::Rec2100Pq => {
                Self { kr: 0.2627, kb: 0.0593 }
            }
            unsupported => {
                return Err(ProgramColorScopeError::UnsupportedSignalColorSpace {
                    color_space: unsupported,
                });
            }
        };
        Ok(colorimetry)
    }

    /// Red luma coefficient.
    pub const fn kr(self) -> f32 {
        self.kr
    }

    /// Blue luma coefficient.
    pub const fn kb(self) -> f32 {
        self.kb
    }

    fn luma(self, r: f32, g: f32, b: f32) -> f32 {
        let kg = 1.0 - self.kr - self.kb;
        self.kr * r + kg * g + self.kb * b
    }

    fn chroma(self, r: f32, b: f32, y: f32) -> (f32, f32) {
        let u = (b - y) / (2.0 * (1.0 - self.kb));
        let v = (r - y) / (2.0 * (1.0 - self.kr));
        (u, v)
    }
}

/// Measure display-encoded RGBA8 pixels at the Program Output boundary.
///
/// The RGB channels are interpreted as non-linear signal values in
/// `signal_color_space`; alpha is ignored. This function deliberately rejects
/// working spaces and camera-log identities because applying display-signal
/// luma/chroma coefficients to those values would produce misleading scopes.
pub fn compute_program_color_scopes_rgba8(
    rgba: &[u8],
    width: u32,
    height: u32,
    signal_color_space: ColorSpace,
    waveform_mode: WaveformMode,
    bins: usize,
) -> Result<ColorScopes, ProgramColorScopeError> {
    let expected = expected_program_scope_pixels(width, height)?
        .checked_mul(4)
        .ok_or(ProgramColorScopeError::DimensionsOverflow { width, height })?;
    if rgba.len() != expected {
        return Err(ProgramColorScopeError::Rgba8ByteLengthMismatch {
            expected,
            actual: rgba.len(),
        });
    }
    compute_program_color_scopes_rgb(
        rgba.chunks_exact(4).map(|pixel| {
            [
                pixel[0] as f32 / 255.0,
                pixel[1] as f32 / 255.0,
                pixel[2] as f32 / 255.0,
            ]
        }),
        width,
        height,
        signal_color_space,
        waveform_mode,
        bins,
    )
}

/// Measure float display-encoded RGBA pixels at the Program Output boundary.
///
/// This is the precision-preserving scope path for 10-bit and HDR output. RGB
/// values outside `0..=1` are retained as endpoint overloads rather than being
/// quantized to RGBA8 first. Non-finite samples fail closed.
pub fn compute_program_color_scopes_rgba_f32(
    rgba: &[[f32; 4]],
    width: u32,
    height: u32,
    signal_color_space: ColorSpace,
    waveform_mode: WaveformMode,
    bins: usize,
) -> Result<ColorScopes, ProgramColorScopeError> {
    let expected = expected_program_scope_pixels(width, height)?;
    if rgba.len() != expected {
        return Err(ProgramColorScopeError::FloatPixelLengthMismatch {
            expected,
            actual: rgba.len(),
        });
    }
    for (pixel_index, pixel) in rgba.iter().enumerate() {
        for (channel, value) in pixel[..3].iter().enumerate() {
            if !value.is_finite() {
                return Err(ProgramColorScopeError::NonFiniteSample {
                    pixel: pixel_index,
                    channel,
                });
            }
        }
    }
    compute_program_color_scopes_rgb(
        rgba.iter().map(|pixel| [pixel[0], pixel[1], pixel[2]]),
        width,
        height,
        signal_color_space,
        waveform_mode,
        bins,
    )
}

fn expected_program_scope_pixels(width: u32, height: u32) -> Result<usize, ProgramColorScopeError> {
    if width == 0 || height == 0 {
        return Err(ProgramColorScopeError::InvalidDimensions { width, height });
    }
    usize::try_from(width)
        .ok()
        .and_then(|width| usize::try_from(height).ok().and_then(|height| width.checked_mul(height)))
        .ok_or(ProgramColorScopeError::DimensionsOverflow { width, height })
}

fn compute_program_color_scopes_rgb(
    rgb: impl Iterator<Item = [f32; 3]>,
    width: u32,
    height: u32,
    signal_color_space: ColorSpace,
    waveform_mode: WaveformMode,
    bins: usize,
) -> Result<ColorScopes, ProgramColorScopeError> {
    let colorimetry = ProgramSignalColorimetry::for_color_space(signal_color_space)?;
    let bins = bins.clamp(16, 1024);
    let width_usize = width as usize;
    let expected_pixels = width_usize * height as usize;
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
    let mut excursions = ProgramSignalExcursions::default();

    for (idx, [r, g, b]) in rgb.enumerate() {
        let x = idx % width_usize;
        let y_signal = colorimetry.luma(r, g, b);
        excursions.red.observe(r);
        excursions.green.observe(g);
        excursions.blue.observe(b);
        excursions.luma.observe(y_signal);
        let y = y_signal.clamp(0.0, 1.0);
        let rb = scope_bin(r, bins);
        let gb = scope_bin(g, bins);
        let bb = scope_bin(b, bins);
        let yb = scope_bin(y, bins);
        histogram.red[rb] = histogram.red[rb].saturating_add(1);
        histogram.green[gb] = histogram.green[gb].saturating_add(1);
        histogram.blue[bb] = histogram.blue[bb].saturating_add(1);
        histogram.luma[yb] = histogram.luma[yb].saturating_add(1);

        match waveform_mode {
            WaveformMode::Luma => {
                let value = &mut waveform.values[x * bins + yb];
                *value = value.saturating_add(1);
            }
            WaveformMode::RgbParade => {
                let plane = width_usize * bins;
                for index in [
                    x * bins + rb,
                    plane + x * bins + gb,
                    plane * 2 + x * bins + bb,
                ] {
                    waveform.values[index] = waveform.values[index].saturating_add(1);
                }
            }
        }

        let (u, v) = colorimetry.chroma(r, b, y);
        let ux = ((u + 0.5).clamp(0.0, 0.999) * 64.0) as usize;
        let vy = ((v + 0.5).clamp(0.0, 0.999) * 64.0) as usize;
        let sample = &mut vectors[vy * 64 + ux];
        sample.u = (ux as f32 + 0.5) / 64.0 - 0.5;
        sample.v = (vy as f32 + 0.5) / 64.0 - 0.5;
        sample.weight = sample.weight.saturating_add(1);
    }

    Ok(ColorScopes {
        signal_color_space,
        sample_count: expected_pixels as u64,
        excursions,
        histogram,
        waveform,
        vectorscope: vectors.into_iter().filter(|sample| sample.weight > 0).collect(),
    })
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

    /// Resolve an input RGB color identity from complete FFmpeg primaries and transfer tags.
    ///
    /// Matrix coefficients describe how encoded YCbCr samples become RGB; they
    /// are not part of the resulting RGB colorimetry. Media import therefore
    /// resolves the RGB identity from primaries plus transfer and retains the
    /// decoder matrix as an independent sampling contract.
    pub fn from_ffmpeg_colorimetry(color_primaries: &str, color_trc: &str) -> Option<Self> {
        let mut candidates = Self::ALL.into_iter().filter(|color_space| {
            color_space.ffmpeg_tags().is_some_and(|tags| {
                ffmpeg_tag_eq(tags.color_primaries, color_primaries)
                    && ffmpeg_tag_eq(tags.color_trc, color_trc)
            })
        });
        let candidate = candidates.next()?;
        candidates.next().is_none().then_some(candidate)
    }

    /// Resolve a color space from partial FFmpeg tag hints.
    ///
    /// This is used for media metadata interpretation where some containers
    /// provide only transfer, primaries, or matrix tags. Complete input
    /// colorimetry wins; partial matches are deliberately centralized here so
    /// media probing does not carry its own color-space knowledge table.
    pub fn from_ffmpeg_tag_hints(
        color_primaries: Option<&str>,
        color_trc: Option<&str>,
        colorspace: Option<&str>,
    ) -> Option<Self> {
        // RGB/GBR is a sampling identity, not evidence that distinguishes an
        // sRGB transfer from another transfer over the same primaries.
        let colorspace = colorspace.filter(|matrix| !ffmpeg_tag_eq(matrix, "gbr"));
        if color_primaries.is_none() && color_trc.is_none() && colorspace.is_none() {
            return None;
        }

        let mut candidates = Self::ALL.into_iter().filter(|color_space| {
            color_space.ffmpeg_tags().is_some_and(|tags| {
                ffmpeg_partial_tag_matches(color_primaries, tags.color_primaries)
                    && ffmpeg_partial_tag_matches(color_trc, tags.color_trc)
                    && ffmpeg_partial_tag_matches(colorspace, tags.colorspace)
            })
        });
        let candidate = candidates.next()?;
        candidates.next().is_none().then_some(candidate)
    }
}

fn ffmpeg_partial_tag_matches(actual: Option<&str>, expected: &str) -> bool {
    actual.is_none_or(|actual| ffmpeg_tag_eq(actual, expected))
}

fn ffmpeg_tag_eq(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
        || matches!(
            (left, right),
            ("rgb", "gbr") | ("gbr", "rgb") | ("gamma28", "bt470bg") | ("bt470bg", "gamma28")
        )
}

fn scope_bin(v: f32, bins: usize) -> usize {
    (v.clamp(0.0, 1.0) * (bins.saturating_sub(1)) as f32).round() as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_engine_pins_its_versioned_working_space() {
        let engine = ColorEngine::mondrian_standard();

        assert_eq!(
            engine.pinned_working_space(),
            Some(WorkingColorSpace::LinearRec2020)
        );
        engine
            .validate_working_space(WorkingColorSpace::LinearRec2020)
            .expect("Standard v1 working space");
        let error = engine
            .validate_working_space(WorkingColorSpace::LinearP3D65)
            .expect_err("Standard v1 must reject a different working space");
        assert!(error.contains("Mondrian Standard"));
        assert!(error.contains("Linear Rec.2020"));
    }

    #[test]
    fn aces_engine_keeps_working_space_selection_explicit() {
        let engine = ColorEngine::Aces {
            preset: crate::AcesConfigPreset::StudioV4Aces2Ocio25,
        };

        assert_eq!(engine.pinned_working_space(), None);
        for working_space in [
            WorkingColorSpace::LinearRec709,
            WorkingColorSpace::LinearRec2020,
            WorkingColorSpace::LinearP3D65,
            WorkingColorSpace::AcesCg,
        ] {
            engine
                .validate_working_space(working_space)
                .expect("official ACES config exposes supported working routes");
        }
    }

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
    fn input_colorimetry_is_independent_from_rgb_or_yuv_sampling_matrix() {
        assert_eq!(
            ColorSpace::from_ffmpeg_colorimetry("bt709", "bt709"),
            Some(ColorSpace::Rec709)
        );
        assert_eq!(
            ColorSpace::from_ffmpeg_colorimetry("bt2020", "smpte2084"),
            Some(ColorSpace::Rec2100Pq)
        );
        assert_eq!(
            ColorSpace::from_ffmpeg_tags("bt709", "bt709", "rgb"),
            None,
            "strict delivery tag parsing must still include matrix coefficients"
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
            None,
            "RGB sampling alone cannot distinguish Rec.709 from sRGB transfer"
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
    fn color_space_keeps_unsupported_complete_cicp_tags_unresolved() {
        assert_eq!(
            ColorSpace::from_ffmpeg_tag_hints(Some("smpte432"), Some("smpte2084"), Some("rgb")),
            None,
            "P3 primaries plus PQ transfer must not be relabeled as BT.2020 Rec.2100 PQ"
        );
    }

    #[test]
    fn color_space_does_not_ignore_an_unsupported_present_cicp_field() {
        assert_eq!(
            ColorSpace::from_ffmpeg_tag_hints(Some("smpte432"), Some("smpte2084"), None),
            None,
            "a missing matrix must not erase the explicit P3/PQ combination"
        );
    }

    #[test]
    fn color_space_only_infers_partial_cicp_when_all_present_fields_are_compatible() {
        let conflicts = [
            (Some("bt709"), Some("arib-std-b67"), None),
            (Some("bt2020"), Some("iec61966-2-1"), None),
            (Some("bt709"), Some("bt470bg"), None),
            (Some("bt709"), Some("smpte170m"), None),
            (Some("bt2020"), Some("bt709"), Some("bt709")),
        ];

        for (primaries, transfer, matrix) in conflicts {
            assert_eq!(
                ColorSpace::from_ffmpeg_tag_hints(primaries, transfer, matrix),
                None,
                "conflicting CICP fields must remain unresolved: primaries={primaries:?}, transfer={transfer:?}, matrix={matrix:?}"
            );
        }
    }

    #[test]
    fn color_space_keeps_ambiguous_single_cicp_tags_unresolved() {
        for (primaries, transfer, matrix) in [
            (Some("bt2020"), None, None),
            (None, Some("iec61966-2-1"), None),
            (None, None, Some("rgb")),
        ] {
            assert_eq!(
                ColorSpace::from_ffmpeg_tag_hints(primaries, transfer, matrix),
                None,
                "one tag shared by multiple product spaces must remain unresolved: primaries={primaries:?}, transfer={transfer:?}, matrix={matrix:?}"
            );
        }
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
            identity: Box::new(crate::types::CustomOcioProjectIdentity::from_resolved(
                crate::types::OcioConfigSource::Path { path: missing_path },
                "0".repeat(64),
                "missing-config".to_owned(),
                "0".repeat(64),
                crate::ocio::ocio_working_color_space_name(WorkingColorSpace::LinearRec709)
                    .to_owned(),
                vec![crate::types::CustomOcioOutputIdentity::from_resolved(
                    ColorSpace::Rec709,
                    "missing-display".to_owned(),
                    "missing-view".to_owned(),
                    "missing-display-color-space".to_owned(),
                    crate::types::CustomOcioLookIdentity::None,
                )],
                Vec::new(),
            )),
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

        assert!(err.contains("OCIO config file not found"), "{err}");
        assert_eq!(rgba, original);
    }

    #[test]
    fn program_scopes_count_pixels_and_rgb_parade_channels() {
        let rgba = vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 0, 0, 0, 255];
        let scopes = compute_program_color_scopes_rgba8(
            &rgba,
            2,
            2,
            ColorSpace::Rec709,
            WaveformMode::RgbParade,
            16,
        )
        .expect("valid Rec.709 program signal");
        assert_eq!(scopes.signal_color_space, ColorSpace::Rec709);
        assert_eq!(scopes.sample_count, 4);
        assert_eq!(scopes.histogram.red.iter().sum::<u32>(), 4);
        assert_eq!(scopes.histogram.green.iter().sum::<u32>(), 4);
        assert_eq!(scopes.histogram.blue.iter().sum::<u32>(), 4);
        assert_eq!(scopes.histogram.luma.iter().sum::<u32>(), 4);
        assert_eq!(scopes.waveform.values.iter().sum::<u32>(), 12);
        assert!(!scopes.vectorscope.is_empty());
    }

    #[test]
    fn program_scopes_use_output_primaries_for_luma() {
        let red = [255, 0, 0, 255];
        let rec709 = compute_program_color_scopes_rgba8(
            &red,
            1,
            1,
            ColorSpace::Rec709,
            WaveformMode::Luma,
            101,
        )
        .expect("Rec.709 scope");
        let rec2020 = compute_program_color_scopes_rgba8(
            &red,
            1,
            1,
            ColorSpace::Rec2100Pq,
            WaveformMode::Luma,
            101,
        )
        .expect("Rec.2020/PQ scope");

        assert_eq!(rec709.histogram.luma[21], 1);
        assert_eq!(rec2020.histogram.luma[26], 1);
        assert_ne!(rec709.histogram.luma, rec2020.histogram.luma);
    }

    #[test]
    fn program_scopes_fail_closed_for_invalid_signal_contracts() {
        let invalid_length = compute_program_color_scopes_rgba8(
            &[0, 0, 0],
            1,
            1,
            ColorSpace::Rec709,
            WaveformMode::Luma,
            64,
        )
        .expect_err("truncated RGBA must be rejected");
        assert!(matches!(
            invalid_length,
            ProgramColorScopeError::Rgba8ByteLengthMismatch { expected: 4, actual: 3 }
        ));
        let invalid_float_length = compute_program_color_scopes_rgba_f32(
            &[],
            1,
            1,
            ColorSpace::Rec709,
            WaveformMode::Luma,
            64,
        )
        .expect_err("missing float pixel must be rejected");
        assert_eq!(
            invalid_float_length,
            ProgramColorScopeError::FloatPixelLengthMismatch { expected: 1, actual: 0 }
        );

        let unsupported = compute_program_color_scopes_rgba8(
            &[0, 0, 0, 255],
            1,
            1,
            ColorSpace::SonySLog3SGamut3,
            WaveformMode::Luma,
            64,
        )
        .expect_err("camera-log source is not a program-output signal");
        assert!(matches!(
            unsupported,
            ProgramColorScopeError::UnsupportedSignalColorSpace {
                color_space: ColorSpace::SonySLog3SGamut3
            }
        ));
    }

    #[test]
    fn float_program_scopes_preserve_hdr_precision_and_reject_non_finite_rgb() {
        let pixels = [[0.501, 0.0, 0.0, 1.0], [0.509, 0.0, 0.0, 1.0]];
        let scopes = compute_program_color_scopes_rgba_f32(
            &pixels,
            2,
            1,
            ColorSpace::Rec2100Pq,
            WaveformMode::RgbParade,
            1024,
        )
        .expect("finite PQ signal");
        assert_eq!(scopes.histogram.red[513], 1);
        assert_eq!(scopes.histogram.red[521], 1);

        let invalid = compute_program_color_scopes_rgba_f32(
            &[[0.0, f32::NAN, 0.0, 1.0]],
            1,
            1,
            ColorSpace::Rec709,
            WaveformMode::Luma,
            64,
        )
        .expect_err("non-finite program signal must fail closed");
        assert_eq!(
            invalid,
            ProgramColorScopeError::NonFiniteSample { pixel: 0, channel: 1 }
        );
    }

    #[test]
    fn float_program_scopes_report_negative_and_superwhite_signal_excursions() {
        let pixels = [[-0.1, 0.5, 1.2, 1.0], [1.1, -0.2, 0.4, 1.0]];
        let scopes = compute_program_color_scopes_rgba_f32(
            &pixels,
            2,
            1,
            ColorSpace::Rec709,
            WaveformMode::Luma,
            64,
        )
        .expect("finite extended Rec.709 signal");

        assert_eq!(scopes.excursions.red.below_nominal, 1);
        assert_eq!(scopes.excursions.red.above_nominal, 1);
        assert_eq!(scopes.excursions.green.below_nominal, 1);
        assert_eq!(scopes.excursions.green.above_nominal, 0);
        assert_eq!(scopes.excursions.blue.below_nominal, 0);
        assert_eq!(scopes.excursions.blue.above_nominal, 1);
        assert_eq!(scopes.histogram.red.first(), Some(&1));
        assert_eq!(scopes.histogram.red.last(), Some(&1));
    }
}
