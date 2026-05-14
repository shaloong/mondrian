//! 项目数据模型
//!
//! `Project` 是最顶层的容器，包含多个 `Sequence`（时间线）和全局项目设置。

use crate::types::*;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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
    pub settings: ProjectSettings,
    pub save_path: Option<PathBuf>,
}

/// 项目色彩管理设置
///
/// 所有序列默认继承此配置，序列可以单独覆盖。
/// 类似于达芬奇项目设置中的色彩科学选择器。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ProjectColorManagement {
    /// 色彩引擎。默认 [`ColorEngine::MondrianSmart`]。
    #[serde(default)]
    pub engine: ColorEngine,
}

/// 项目全局设置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectSettings {
    pub proxy_enabled: bool,
    pub proxy_resolution: Resolution,
    pub cache_dir: Option<PathBuf>,
    pub auto_save_interval: u32,
    /// 项目级色彩管理（所有序列默认继承）。
    #[serde(default)]
    pub color_management: ProjectColorManagement,
}

impl Default for ProjectSettings {
    fn default() -> Self {
        Self {
            proxy_enabled: true,
            proxy_resolution: Resolution::HD,
            cache_dir: None,
            auto_save_interval: 300,
            color_management: ProjectColorManagement::default(),
        }
    }
}

impl Project {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            meta: ProjectMeta::new(name),
            settings: ProjectSettings::default(),
            save_path: None,
        }
    }

    pub fn touch(&mut self) {
        self.meta.updated_at = Utc::now();
        self.meta.version += 1;
    }
}
