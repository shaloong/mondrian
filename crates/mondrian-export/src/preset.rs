//! 导出格式预设

use mondrian_core::timeline_data::AssetMediaInterpretation;
use mondrian_core::types::{AssetId, ColorSpace};
use mondrian_core::AudioSourceComponentId;
use mondrian_media::{AudioSourceSelection, MediaFileFingerprint, VideoColorDiagnostic};
use mondrian_timeline::sequence::{DeliveryBitDepth, Sequence, VideoRange};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolution {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Container {
    Mp4,
    Mov,
    Mkv,
    Gif,
    Mxf,
    Webm,
}

/// One preset value that may use the Sequence delivery default or override it.
///
/// `FollowSequence` is an authoring convenience only. Export admission always
/// resolves it to a concrete immutable delivery contract before execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case", tag = "source", content = "value")]
pub enum ExportParameter<T> {
    /// Resolve the concrete value from the selected Sequence.
    #[default]
    FollowSequence,
    /// Use this delivery-specific value instead of the Sequence default.
    Explicit(T),
}

impl<T: Copy> ExportParameter<T> {
    /// Resolve this authoring choice against one Sequence-owned default.
    pub const fn resolve(self, sequence_default: T) -> T {
        match self {
            Self::FollowSequence => sequence_default,
            Self::Explicit(value) => value,
        }
    }
}

/// Chroma representation requested from the delivery encoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ExportChromaSampling {
    /// Planar YUV 4:2:0, used by interoperable H.264/HEVC/AV1 delivery.
    #[default]
    Yuv420,
    /// Planar YUV 4:2:2, used by ProRes editorial and mastering profiles.
    Yuv422,
    /// Planar YUV 4:4:4, used by ProRes 4444 profiles.
    Yuv444,
    /// Packed RGB representation, currently used by GIF delivery.
    Rgb,
}

/// Concrete encoded video signal choices independent from creative color intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportVideoSignal {
    /// Encoded sample depth, either inherited from the Sequence or explicit.
    pub bit_depth: ExportParameter<DeliveryBitDepth>,
    /// Encoded video range, either inherited from the Sequence or explicit.
    pub range: ExportParameter<VideoRange>,
    /// Encoded chroma representation.
    pub chroma_sampling: ExportChromaSampling,
}

impl Default for ExportVideoSignal {
    fn default() -> Self {
        Self {
            bit_depth: ExportParameter::FollowSequence,
            range: ExportParameter::FollowSequence,
            chroma_sampling: ExportChromaSampling::Yuv420,
        }
    }
}

impl ExportVideoSignal {
    const fn explicit(
        bit_depth: DeliveryBitDepth,
        range: VideoRange,
        chroma_sampling: ExportChromaSampling,
    ) -> Self {
        Self {
            bit_depth: ExportParameter::Explicit(bit_depth),
            range: ExportParameter::Explicit(range),
            chroma_sampling,
        }
    }
}

/// H.264 profile variants with verified FFmpeg lowering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum H264Profile {
    /// H.264 High Profile, verified as 8-bit 4:2:0 in the current backend.
    High,
}

/// HEVC profile variants with verified FFmpeg lowering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HevcProfile {
    /// HEVC Main Profile, verified as 8-bit 4:2:0.
    Main,
    /// HEVC Main 10 Profile, verified as 10-bit 4:2:0.
    Main10,
}

/// AV1 profile variants with verified FFmpeg lowering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Av1Profile {
    /// AV1 Main Profile, verified as 8-bit or 10-bit 4:2:0.
    Main,
}

/// ProRes profile variants with exact encoder and signal contracts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProResProfile {
    /// ProRes 422 Proxy.
    Proxy,
    /// ProRes 422 LT.
    Lt,
    /// ProRes 422.
    Standard,
    /// ProRes 422 HQ.
    Hq,
    /// ProRes 4444.
    FourFourFourFour,
    /// ProRes 4444 XQ.
    FourFourFourFourXq,
}

impl ProResProfile {
    /// Whether this profile carries 4:4:4 image data and may carry alpha.
    pub const fn is_4444(self) -> bool {
        matches!(self, Self::FourFourFourFour | Self::FourFourFourFourXq)
    }
}

/// Single-pass quality control with an optional, properly bounded VBV ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoRateControl {
    /// Codec constant-quality factor.
    pub crf: u8,
    /// Optional VBV maximum bitrate in kilobits per second.
    pub max_bitrate_kbps: Option<u32>,
    /// Optional VBV buffer size in kilobits.
    pub buffer_size_kbits: Option<u32>,
}

impl VideoRateControl {
    /// Construct unconstrained single-pass constant-quality control.
    pub const fn constant_quality(crf: u8) -> Self {
        Self {
            crf,
            max_bitrate_kbps: None,
            buffer_size_kbits: None,
        }
    }

    /// Construct constant-quality control with a complete VBV ceiling.
    pub const fn constrained_quality(
        crf: u8,
        max_bitrate_kbps: u32,
        buffer_size_kbits: u32,
    ) -> Self {
        Self {
            crf,
            max_bitrate_kbps: Some(max_bitrate_kbps),
            buffer_size_kbits: Some(buffer_size_kbits),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoCodecConfig {
    /// Software H.264 encoding with an explicit profile and rate control.
    H264 {
        /// Exact encoder profile.
        profile: H264Profile,
        /// Single-pass rate-control contract.
        rate_control: VideoRateControl,
    },
    /// Software HEVC encoding with an explicit profile and rate control.
    Hevc {
        /// Exact encoder profile.
        profile: HevcProfile,
        /// Single-pass rate-control contract.
        rate_control: VideoRateControl,
    },
    /// Software AV1 encoding with an explicit profile and rate control.
    Av1 {
        /// Exact encoder profile.
        profile: Av1Profile,
        /// Single-pass rate-control contract.
        rate_control: VideoRateControl,
    },
    /// Apple ProRes encoding with an explicit profile.
    ProRes {
        /// Exact encoder profile.
        profile: ProResProfile,
    },
    /// Palette GIF encoding.
    Gif {
        /// Maximum palette entries.
        colors: u16,
        /// Whether the GIF encoder may dither palette conversion.
        dither: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioCodecConfig {
    /// Omit the audio stream.
    Disabled,
    /// AAC encoding at the requested bitrate in kilobits per second.
    Aac { bitrate_kbps: u32 },
    /// Uncompressed PCM with the requested sample depth.
    Pcm { bit_depth: u8 },
    /// MP3 encoding at the requested bitrate in kilobits per second.
    Mp3 { bitrate_kbps: u32 },
}

/// How timeline coverage is delivered by an export preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ExportAlphaMode {
    /// Composite over scene-linear black before the output transform.
    #[default]
    FlattenBlack,
    /// Preserve straight coverage alpha in an explicitly supported codec.
    Preserve,
}

/// Creative color target applied after Timeline compositing.
///
/// This is distinct from encoded range, bit depth, and chroma sampling.
/// `FollowSequence` preserves Program Output. Explicit targets state whether
/// the Project engine's rendering View or a direct colorimetric transform is
/// required; execution never infers that choice from a codec name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case", tag = "mode", content = "color_space")]
pub enum ExportColorTarget {
    /// Deliver the selected Sequence's Program Output.
    #[default]
    FollowSequence,
    /// Transform working pixels directly into an encoded delivery space.
    Colorimetric(ColorSpace),
    /// Apply the Project engine's rendering View for this display target.
    RenderingView(ColorSpace),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportPreset {
    pub name: String,
    pub container: Container,
    pub video: VideoCodecConfig,
    pub audio: AudioCodecConfig,
    pub resolution: Option<Resolution>,
    /// Encoded signal representation. Creative output color remains Sequence-owned.
    #[serde(default)]
    pub video_signal: ExportVideoSignal,
    /// Explicit alpha delivery policy; codec choice alone never implies transparency.
    #[serde(default)]
    pub alpha_mode: ExportAlphaMode,
    /// Explicit creative color target, independent from encoded signal layout.
    #[serde(default)]
    pub color_target: ExportColorTarget,
}

impl ExportPreset {
    /// Broadly compatible Rec.709 H.264/AAC MP4 delivery.
    pub fn h264_aac_sdr_1080p() -> Self {
        Self {
            name: "H.264/AAC SDR 1080p".into(),
            container: Container::Mp4,
            video: VideoCodecConfig::H264 {
                profile: H264Profile::High,
                rate_control: VideoRateControl::constrained_quality(18, 8_000, 16_000),
            },
            audio: AudioCodecConfig::Aac { bitrate_kbps: 192 },
            resolution: Some(Resolution { width: 1920, height: 1080 }),
            video_signal: ExportVideoSignal::explicit(
                DeliveryBitDepth::Eight,
                VideoRange::Legal,
                ExportChromaSampling::Yuv420,
            ),
            alpha_mode: ExportAlphaMode::FlattenBlack,
            color_target: ExportColorTarget::RenderingView(ColorSpace::Rec709),
        }
    }

    /// HEVC Main10/AAC MP4 delivery that follows Sequence dimensions.
    pub fn hevc_main10_aac() -> Self {
        Self {
            name: "HEVC Main10/AAC".into(),
            container: Container::Mp4,
            video: VideoCodecConfig::Hevc {
                profile: HevcProfile::Main10,
                rate_control: VideoRateControl::constant_quality(20),
            },
            audio: AudioCodecConfig::Aac { bitrate_kbps: 192 },
            resolution: None,
            video_signal: ExportVideoSignal::explicit(
                DeliveryBitDepth::Ten,
                VideoRange::Legal,
                ExportChromaSampling::Yuv420,
            ),
            alpha_mode: ExportAlphaMode::FlattenBlack,
            color_target: ExportColorTarget::FollowSequence,
        }
    }

    pub fn tiktok_vertical() -> Self {
        Self {
            name: "TikTok 1080×1920".into(),
            container: Container::Mp4,
            video: VideoCodecConfig::H264 {
                profile: H264Profile::High,
                rate_control: VideoRateControl::constrained_quality(20, 6_000, 12_000),
            },
            audio: AudioCodecConfig::Aac { bitrate_kbps: 128 },
            resolution: Some(Resolution { width: 1080, height: 1920 }),
            video_signal: ExportVideoSignal::explicit(
                DeliveryBitDepth::Eight,
                VideoRange::Legal,
                ExportChromaSampling::Yuv420,
            ),
            alpha_mode: ExportAlphaMode::FlattenBlack,
            color_target: ExportColorTarget::RenderingView(ColorSpace::Rec709),
        }
    }

    pub fn proxy_720p() -> Self {
        Self {
            name: "Proxy 720p".into(),
            container: Container::Mp4,
            video: VideoCodecConfig::H264 {
                profile: H264Profile::High,
                rate_control: VideoRateControl::constant_quality(23),
            },
            audio: AudioCodecConfig::Aac { bitrate_kbps: 128 },
            resolution: Some(Resolution { width: 1280, height: 720 }),
            video_signal: ExportVideoSignal::explicit(
                DeliveryBitDepth::Eight,
                VideoRange::Legal,
                ExportChromaSampling::Yuv420,
            ),
            alpha_mode: ExportAlphaMode::FlattenBlack,
            color_target: ExportColorTarget::RenderingView(ColorSpace::Rec709),
        }
    }

    /// MOV/ProRes 4444 XQ intermediate that preserves straight alpha.
    pub fn prores_4444_alpha() -> Self {
        Self {
            name: "ProRes 4444 XQ + Alpha".into(),
            container: Container::Mov,
            video: VideoCodecConfig::ProRes { profile: ProResProfile::FourFourFourFourXq },
            audio: AudioCodecConfig::Pcm { bit_depth: 24 },
            resolution: None,
            video_signal: ExportVideoSignal::explicit(
                DeliveryBitDepth::Twelve,
                VideoRange::Full,
                ExportChromaSampling::Yuv444,
            ),
            alpha_mode: ExportAlphaMode::Preserve,
            color_target: ExportColorTarget::FollowSequence,
        }
    }
}

/// Stable identities for product-owned delivery presets.
///
/// The app and Headless validation use this catalog instead of independently
/// rebuilding semantically similar presets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum BuiltinExportPreset {
    /// Interoperable H.264 High/AAC SDR MP4.
    #[default]
    H264AacSdr1080p,
    /// HEVC Main10/AAC MP4 that follows Sequence dimensions.
    HevcMain10Aac,
    /// H.264/AAC vertical social delivery.
    TiktokVertical,
    /// Lightweight H.264/AAC editorial proxy.
    Proxy720p,
    /// ProRes 4444 XQ with preserved alpha.
    ProRes4444Alpha,
}

impl BuiltinExportPreset {
    /// Stable product-owned preset order shared by every frontend.
    pub const ALL: [Self; 5] = [
        Self::H264AacSdr1080p,
        Self::HevcMain10Aac,
        Self::TiktokVertical,
        Self::Proxy720p,
        Self::ProRes4444Alpha,
    ];

    /// Stable non-localized identifier for Headless validation and preset selection.
    pub const fn id(self) -> &'static str {
        match self {
            Self::H264AacSdr1080p => "h264-aac-sdr",
            Self::HevcMain10Aac => "hevc-main10",
            Self::TiktokVertical => "tiktok-vertical",
            Self::Proxy720p => "proxy-720p",
            Self::ProRes4444Alpha => "prores-4444-alpha",
        }
    }

    /// Human-readable product label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::H264AacSdr1080p => "H.264/AAC SDR 1080p",
            Self::HevcMain10Aac => "HEVC Main10/AAC（跟随序列）",
            Self::TiktokVertical => "TikTok 竖屏 9:16",
            Self::Proxy720p => "代理文件 720p",
            Self::ProRes4444Alpha => "ProRes 4444 XQ + Alpha（12-bit）",
        }
    }

    /// Materialize the complete typed preset contract.
    pub fn preset(self) -> ExportPreset {
        match self {
            Self::H264AacSdr1080p => ExportPreset::h264_aac_sdr_1080p(),
            Self::HevcMain10Aac => ExportPreset::hevc_main10_aac(),
            Self::TiktokVertical => ExportPreset::tiktok_vertical(),
            Self::Proxy720p => ExportPreset::proxy_720p(),
            Self::ProRes4444Alpha => ExportPreset::prores_4444_alpha(),
        }
    }
}

/// 导出配置（预设 + 自定义覆盖）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportConfig {
    pub preset: ExportPreset,
    /// Immutable timeline and media-dependency snapshot captured at admission.
    pub timeline: Box<TimelineExportSnapshot>,
    pub output_path: std::path::PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TimelineExportRange {
    #[default]
    SequenceInOut,
    EntireSequence,
    WorkArea {
        start_frame: i64,
        end_frame_exclusive: i64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineExportSnapshot {
    /// Exact Project color engine frozen when the job is admitted.
    pub color_environment: mondrian_core::ProjectColorEnvironment,
    pub sequence: Sequence,
    #[serde(default)]
    pub sequences: Vec<Sequence>,
    /// Closed dependency set for every real media asset reachable from the sequence graph.
    #[serde(default)]
    pub media: HashMap<AssetId, ExportMediaDependency>,
    #[serde(default)]
    pub range: TimelineExportRange,
}

/// One internally consistent media dependency frozen into an export snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportMediaDependency {
    /// Source path resolved when the snapshot was captured.
    pub path: PathBuf,
    /// Exact source revision that the export is allowed to publish from.
    pub source_fingerprint: MediaFileFingerprint,
    /// Frozen physical bindings for the audio Components used by this snapshot.
    #[serde(default)]
    pub audio_components: HashMap<AudioSourceComponentId, AudioSourceSelection>,
    /// Color space explicitly detected from source metadata, when reliable.
    pub detected_color_space: Option<ColorSpace>,
    /// Persistent user interpretation captured with the source revision.
    pub interpretation: AssetMediaInterpretation,
    /// Source color evidence captured with the source revision.
    pub color_diagnostic: Option<VideoColorDiagnostic>,
}
