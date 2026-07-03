//! 项目共享数据模型
//!
//! 持久化项目文档由 `mondrian-project` 定义；这里保留跨 crate 共享的
//! 项目元数据和项目级设置。

use crate::{types::*, DisplayManagementPolicy};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 项目元数据
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProjectMeta {
    /// User-facing project name.
    pub name: String,
    /// Optional user-facing project description.
    pub description: String,
    /// Optional user-facing project author.
    pub author: String,
    /// UTC timestamp when the project document was created.
    pub created_at: DateTime<Utc>,
    /// UTC timestamp when the project metadata was last touched.
    pub updated_at: DateTime<Utc>,
}

impl ProjectMeta {
    pub fn new(name: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            name: name.into(),
            description: String::new(),
            author: String::new(),
            created_at: now,
            updated_at: now,
        }
    }

    pub fn touch(&mut self) {
        self.updated_at = Utc::now();
    }
}

/// 项目色彩管理设置
///
/// 所有序列默认继承此配置，序列可以单独覆盖。
/// 类似于达芬奇项目设置中的色彩科学选择器。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ProjectColorManagement {
    /// 色彩引擎。默认 [`ColorEngine::MondrianSmart`]，即 Mondrian Standard
    /// policy over the bundled OCIO config.
    #[serde(default)]
    pub engine: ColorEngine,
    /// Project-level display-management policy inherited by sequences.
    #[serde(default)]
    pub display_management: DisplayManagementPolicy,
}

/// 项目全局设置
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProjectSettings {
    /// Whether automatic proxy generation and proxy-aware workflows are enabled.
    pub proxy_enabled: bool,
    /// Default proxy resolution for generated proxy media.
    pub proxy_resolution: Resolution,
    /// Optional project cache directory override.
    pub cache_dir: Option<PathBuf>,
    /// Autosave interval in seconds.
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
