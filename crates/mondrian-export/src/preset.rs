//! 导出格式预设

use mondrian_core::timeline_data::AssetMediaInterpretation;
use mondrian_core::types::{AssetId, ColorSpace};
use mondrian_media::VideoColorDiagnostic;
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportPreset {
    pub name: String,
    pub container: Container,
    pub video: VideoCodecConfig,
    pub audio: AudioCodecConfig,
    pub resolution: Option<Resolution>,
}

impl ExportPreset {
    pub fn youtube_1080p() -> Self {
        Self {
            name: "YouTube 1080p".into(),
            container: Container::Mp4,
            video: VideoCodecConfig::H264 { crf: 18, bitrate_kbps: Some(8000) },
            audio: AudioCodecConfig::Aac { bitrate_kbps: 192 },
            resolution: Some(Resolution { width: 1920, height: 1080 }),
        }
    }

    pub fn tiktok_vertical() -> Self {
        Self {
            name: "TikTok 1080×1920".into(),
            container: Container::Mp4,
            video: VideoCodecConfig::H264 { crf: 20, bitrate_kbps: Some(6000) },
            audio: AudioCodecConfig::Aac { bitrate_kbps: 128 },
            resolution: Some(Resolution { width: 1080, height: 1920 }),
        }
    }

    pub fn proxy_720p() -> Self {
        Self {
            name: "Proxy 720p".into(),
            container: Container::Mp4,
            video: VideoCodecConfig::H264 { crf: 23, bitrate_kbps: None },
            audio: AudioCodecConfig::Aac { bitrate_kbps: 128 },
            resolution: Some(Resolution { width: 1280, height: 720 }),
        }
    }
}

/// 导出配置（预设 + 自定义覆盖）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportConfig {
    pub preset: ExportPreset,
    pub input: ExportInput,
    pub output_path: std::path::PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ExportInput {
    /// 直接转码单文件输入（兼容旧流程）
    File {
        input_path: PathBuf,
        in_point: Option<String>, // TODO: FramePosition
        out_point: Option<String>,
    },
    /// 从时间线逐帧渲染后再编码输出
    Timeline(Box<TimelineExportInput>),
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
pub struct TimelineExportInput {
    pub sequence: Sequence,
    #[serde(default)]
    pub sequences: Vec<Sequence>,
    pub asset_paths: HashMap<AssetId, PathBuf>,
    /// Asset color spaces explicitly detected from media metadata.
    ///
    /// Missing entries mean the media was untagged or unsupported; render code
    /// must resolve them through the sequence missing-metadata policy.
    #[serde(default)]
    pub asset_color_spaces: HashMap<AssetId, ColorSpace>,
    /// Persistent user interpretation stored on asset-library records.
    #[serde(default)]
    pub asset_interpretations: HashMap<AssetId, AssetMediaInterpretation>,
    /// Per-asset media color diagnostics captured during app-side asset lookup.
    #[serde(default)]
    pub asset_color_diagnostics: HashMap<AssetId, VideoColorDiagnostic>,
    #[serde(default)]
    pub range: TimelineExportRange,
    /// 项目级色彩管理设置（所有序列默认继承）。
    #[serde(default)]
    pub project_color_management: mondrian_core::ProjectColorManagement,
}
