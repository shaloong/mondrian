//! 导出格式预设

use mondrian_core::timeline_data::AssetMediaInterpretation;
use mondrian_core::types::{AssetId, ColorSpace};
use mondrian_media::{MediaFileFingerprint, VideoColorDiagnostic};
use mondrian_timeline::sequence::Sequence;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Resolution {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Container {
    Mp4,
    Mov,
    Mkv,
    Gif,
    Mxf,
    Webm,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum VideoCodecConfig {
    H264 { crf: u8, bitrate_kbps: Option<u32> },
    H265 { crf: u8, bitrate_kbps: Option<u32> },
    Av1 { crf: u8 },
    ProRes { variant: String },
    Gif { colors: u16, dither: bool },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AudioCodecConfig {
    Aac { bitrate_kbps: u32 },
    Pcm { bit_depth: u8 },
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportPreset {
    pub name: String,
    pub container: Container,
    pub video: VideoCodecConfig,
    pub audio: AudioCodecConfig,
    pub resolution: Option<Resolution>,
    /// Explicit alpha delivery policy; codec choice alone never implies transparency.
    #[serde(default)]
    pub alpha_mode: ExportAlphaMode,
}

impl ExportPreset {
    pub fn youtube_1080p() -> Self {
        Self {
            name: "YouTube 1080p".into(),
            container: Container::Mp4,
            video: VideoCodecConfig::H264 { crf: 18, bitrate_kbps: Some(8000) },
            audio: AudioCodecConfig::Aac { bitrate_kbps: 192 },
            resolution: Some(Resolution { width: 1920, height: 1080 }),
            alpha_mode: ExportAlphaMode::FlattenBlack,
        }
    }

    pub fn tiktok_vertical() -> Self {
        Self {
            name: "TikTok 1080×1920".into(),
            container: Container::Mp4,
            video: VideoCodecConfig::H264 { crf: 20, bitrate_kbps: Some(6000) },
            audio: AudioCodecConfig::Aac { bitrate_kbps: 128 },
            resolution: Some(Resolution { width: 1080, height: 1920 }),
            alpha_mode: ExportAlphaMode::FlattenBlack,
        }
    }

    pub fn proxy_720p() -> Self {
        Self {
            name: "Proxy 720p".into(),
            container: Container::Mp4,
            video: VideoCodecConfig::H264 { crf: 23, bitrate_kbps: None },
            audio: AudioCodecConfig::Aac { bitrate_kbps: 128 },
            resolution: Some(Resolution { width: 1280, height: 720 }),
            alpha_mode: ExportAlphaMode::FlattenBlack,
        }
    }

    /// MOV/ProRes 4444 XQ intermediate that preserves straight alpha.
    pub fn prores_4444_alpha() -> Self {
        Self {
            name: "ProRes 4444 XQ + Alpha".into(),
            container: Container::Mov,
            video: VideoCodecConfig::ProRes { variant: "4444xq".into() },
            audio: AudioCodecConfig::Pcm { bit_depth: 24 },
            resolution: None,
            alpha_mode: ExportAlphaMode::Preserve,
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
    pub sequence: Sequence,
    #[serde(default)]
    pub sequences: Vec<Sequence>,
    /// Closed dependency set for every real media asset reachable from the sequence graph.
    #[serde(default)]
    pub media: HashMap<AssetId, ExportMediaDependency>,
    #[serde(default)]
    pub range: TimelineExportRange,
    /// 项目级色彩管理设置（所有序列默认继承）。
    #[serde(default)]
    pub project_color_management: mondrian_core::ProjectColorManagement,
}

/// One internally consistent media dependency frozen into an export snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportMediaDependency {
    /// Source path resolved when the snapshot was captured.
    pub path: PathBuf,
    /// Exact source revision that the export is allowed to publish from.
    pub source_fingerprint: MediaFileFingerprint,
    /// Color space explicitly detected from source metadata, when reliable.
    pub detected_color_space: Option<ColorSpace>,
    /// Persistent user interpretation captured with the source revision.
    pub interpretation: AssetMediaInterpretation,
    /// Source color evidence captured with the source revision.
    pub color_diagnostic: Option<VideoColorDiagnostic>,
}
