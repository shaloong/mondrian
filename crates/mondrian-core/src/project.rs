//! 项目数据模型
//!
//! `Project` 是最顶层的容器，包含多个 `Sequence`（时间线）和全局项目设置。

use crate::types::*;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 序列（Sequence）设置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SequenceSettings {
    pub resolution: Resolution,
    pub frame_rate: Rational,
    pub sample_rate: u32,   // 音频采样率（Hz），通常 48000
    pub audio_channels: u8, // 声道数（2 = 立体声）
    pub color_space: ColorSpace,
    pub pixel_aspect: Rational, // 像素宽高比（通常 1:1）
}

impl Default for SequenceSettings {
    fn default() -> Self {
        Self {
            resolution: Resolution::FHD,
            frame_rate: Rational::FPS_25,
            sample_rate: 48_000,
            audio_channels: 2,
            color_space: ColorSpace::Rec709,
            pixel_aspect: Rational::new(1, 1),
        }
    }
}

/// 项目元数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectMeta {
    pub id: ProjectId,
    pub name: String,
    pub description: String,
    pub author: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub version: u32, // 保存版本，用于迁移
}

impl ProjectMeta {
    pub fn new(name: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            id: ProjectId::new(),
            name: name.into(),
            description: String::new(),
            author: String::new(),
            created_at: now,
            updated_at: now,
            version: 1,
        }
    }
}

/// 项目（顶层容器）
///
/// 一个 `.mondrian` 文件对应一个 `Project`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub meta: ProjectMeta,
    pub sequences: Vec<SequenceRef>, // 序列引用（完整数据在 timeline crate）
    pub active_sequence: Option<SequenceId>,
    pub settings: ProjectSettings,
    pub save_path: Option<PathBuf>,
}

/// 序列引用（轻量）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SequenceRef {
    pub id: SequenceId,
    pub name: String,
    pub settings: SequenceSettings,
}

/// 项目全局设置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectSettings {
    pub proxy_enabled: bool,
    pub proxy_resolution: Resolution,
    pub cache_dir: Option<PathBuf>,
    pub auto_save_interval: u32, // 自动保存间隔（秒）
    pub color_management: bool,
}

impl Default for ProjectSettings {
    fn default() -> Self {
        Self {
            proxy_enabled: true,
            proxy_resolution: Resolution::HD,
            cache_dir: None,
            auto_save_interval: 300, // 5 分钟
            color_management: true,
        }
    }
}

impl Project {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            meta: ProjectMeta::new(name),
            sequences: Vec::new(),
            active_sequence: None,
            settings: ProjectSettings::default(),
            save_path: None,
        }
    }

    pub fn add_sequence(
        &mut self,
        name: impl Into<String>,
        settings: SequenceSettings,
    ) -> SequenceId {
        let id = SequenceId::new();
        self.sequences.push(SequenceRef { id, name: name.into(), settings });
        if self.active_sequence.is_none() {
            self.active_sequence = Some(id);
        }
        id
    }

    pub fn touch(&mut self) {
        self.meta.updated_at = Utc::now();
        self.meta.version += 1;
    }
}
