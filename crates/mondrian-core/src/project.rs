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
    engine: ColorEngine,
}

impl ProjectColorEnvironment {
    /// Create a Project color environment around one exact product engine.
    pub const fn new(engine: ColorEngine) -> Self {
        Self { engine }
    }

    /// Exact Mondrian Standard, ACES, or Custom OCIO engine owned by the Project.
    pub const fn engine(&self) -> &ColorEngine {
        &self.engine
    }

    /// Consume the environment and return its exact product engine.
    pub fn into_engine(self) -> ColorEngine {
        self.engine
    }

    /// Pin one Custom OCIO environment for the complete Project color usage.
    ///
    /// The future-Sequence template and every existing Sequence contribute one
    /// working/output pair. The current Custom identity deliberately covers
    /// exactly one working space, while every distinct Program Output is bound
    /// atomically. A partial environment is never returned.
    pub fn custom_ocio(
        source: OcioConfigSource,
        sequence_color_contracts: &[(WorkingColorSpace, ColorSpace)],
    ) -> Result<Self, String> {
        if sequence_color_contracts.is_empty() {
            return Err(
                "Custom OCIO project requires at least one Sequence color contract".to_owned(),
            );
        }
        let mut working_spaces = Vec::new();
        let mut output_color_spaces = Vec::new();
        for (working_space, output_color_space) in sequence_color_contracts {
            if !working_spaces.contains(working_space) {
                working_spaces.push(*working_space);
            }
            if !output_color_spaces.contains(output_color_space) {
                output_color_spaces.push(*output_color_space);
            }
        }
        let [working_space] = working_spaces.as_slice() else {
            return Err(format!(
                "Custom OCIO project identity requires exactly one working space, got {working_spaces:?}; unify the future-Sequence template and all existing Sequences first"
            ));
        };
        ColorEngine::custom_ocio_for_outputs(source, *working_space, &output_color_spaces)
            .map(Self::new)
    }
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
