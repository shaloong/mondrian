//! 项目共享数据模型
//!
//! 持久化项目文档由 `mondrian-project` 定义；这里保留跨 crate 共享的
//! 项目元数据和项目级设置。

use crate::types::*;
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

/// Project-wide color transform vocabulary and execution engine.
///
/// Every Sequence in a Project selects working and output spaces from this
/// exact, version-pinned engine. A Sequence never overrides or inherits an
/// engine, which keeps nested evaluation and cache identity unambiguous.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct ProjectColorEnvironment {
    /// Mondrian Standard, ACES, or a pinned Custom OCIO configuration.
    pub engine: ColorEngine,
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
}

impl Default for ProjectSettings {
    fn default() -> Self {
        Self {
            proxy_enabled: true,
            proxy_resolution: Resolution::HD,
            cache_dir: None,
            auto_save_interval: 300,
        }
    }
}
