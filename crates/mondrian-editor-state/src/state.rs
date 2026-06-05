//! 编辑器状态 —— 唯一真相源
//!
//! [`EditorState`] 集中管理所有编辑状态。UI 只读取它，不直接修改它。
//! 修改只能通过 [`crate::EditorDispatch::dispatch`] 进行。
//!
//! ## 注意
//!
//! 此结构是 **最终目标形态** 的骨架。Stage A 阶段，实际的状态数据
//! 仍由 `mondrian-app::AppState` 管理。`EditorState` 在新 UI 的
//! Panel 中使用，通过 `EditorDispatch` trait 桥接到 `AppState`。

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use mondrian_core::events::EventBus;
use mondrian_core::SequenceId;
use mondrian_timeline::command::CommandHistory;
use mondrian_timeline::Sequence;
use serde::{Deserialize, Serialize};

// ═══════════════════════════════════════════════════════════════════════════════════
// 共享枚举（mondrian-editor-state 定义，mondrian-app 和 mondrian-editor-ui 共用）
// ═══════════════════════════════════════════════════════════════════════════════════

/// 面板类型标识
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PanelKind {
    Viewer,
    Timeline,
    Assets,
    Inspector,
    Effects,
    Project,
    Console,
    NodeGraph,
    Export,
}

impl PanelKind {
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Viewer => "预览",
            Self::Timeline => "时间线",
            Self::Assets => "素材",
            Self::Inspector => "检查器",
            Self::Effects => "效果",
            Self::Project => "项目",
            Self::Console => "控制台",
            Self::NodeGraph => "节点图",
            Self::Export => "导出",
        }
    }
}

/// 预设工作区布局
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum WorkspacePreset {
    #[default]
    Editing,
    Color,
    Audio,
    Compositing,
    Export,
    Custom,
}

impl WorkspacePreset {
    pub const ALL: [Self; 6] = [
        Self::Editing,
        Self::Color,
        Self::Audio,
        Self::Compositing,
        Self::Export,
        Self::Custom,
    ];

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Editing => "编辑",
            Self::Color => "调色",
            Self::Audio => "音频",
            Self::Compositing => "合成",
            Self::Export => "导出",
            Self::Custom => "自定义",
        }
    }
}

/// 语言偏好
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Language {
    #[default]
    Chinese,
    English,
}

/// 播放状态（最终从 mondrian-app 迁移至此）
///
/// Stage A 中先在 mondrian-editor-state 定义独立副本，
/// mondrian-app 中的 `PlaybackState` 保持不变。
/// Stage F 时统一为这一个。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PlaybackState {
    #[default]
    Stopped,
    Playing {
        timecode_frames: i64,
    },
    Paused {
        timecode_frames: i64,
    },
}

/// 选择状态（最终从 mondrian-app 迁移至此）
///
/// Stage A 中为简化版。Stage F 时承载完整选择逻辑。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SelectionState {
    pub selected_clip_ids: Vec<mondrian_core::ClipId>,
    pub selected_track_ids: Vec<mondrian_core::TrackId>,
}

/// 项目句柄（轻量引用，不含全部运行时数据）
#[derive(Debug, Clone)]
pub struct ProjectHandle {
    pub path: PathBuf,
    pub name: String,
}

// ═══════════════════════════════════════════════════════════════════════════════════
// EditorState
// ═══════════════════════════════════════════════════════════════════════════════════

/// 编辑器的唯一真相源
///
/// ## 不变量
///
/// * 所有数据集中在此结构体中
/// * 修改只能通过 `dispatch(Action)` 进行
/// * UI 通过只读引用读取此结构
/// * Panel 之间通过 `EventBus` 通信，不直接引用
pub struct EditorState {
    // === 事件总线 ===
    pub event_bus: Arc<EventBus>,

    // === 项目 ===
    pub project: Option<ProjectHandle>,

    // === 时间线 ===
    pub sequences: Vec<Sequence>,
    pub active_sequence_id: Option<SequenceId>,
    pub cmd_history: CommandHistory,

    // === 播放 ===
    pub playback: PlaybackState,

    // === 选择 ===
    pub selection: SelectionState,

    // === 工作区 ===
    pub workspace: WorkspacePreset,
    pub active_panels: HashSet<PanelKind>,

    // === 编辑器设置 ===
    pub language: Language,
}

impl EditorState {
    /// 创建空的编辑器状态（启动时使用）
    pub fn new() -> Self {
        Self {
            event_bus: EventBus::new(),
            project: None,
            sequences: Vec::new(),
            active_sequence_id: None,
            cmd_history: CommandHistory::default(),
            playback: PlaybackState::default(),
            selection: SelectionState::default(),
            workspace: WorkspacePreset::default(),
            active_panels: HashSet::new(),
            language: Language::default(),
        }
    }

    /// 是否有已打开的项目
    pub fn has_open_project(&self) -> bool {
        self.project.is_some()
    }

    /// 获取当前活跃的序列
    pub fn active_sequence(&self) -> Option<&Sequence> {
        self.active_sequence_id
            .and_then(|id| self.sequences.iter().find(|s| s.id == id))
    }

    /// 获取当前活跃序列的可变引用
    pub fn active_sequence_mut(&mut self) -> Option<&mut Sequence> {
        self.active_sequence_id
            .and_then(|id| self.sequences.iter_mut().find(|s| s.id == id))
    }
}

impl Default for EditorState {
    fn default() -> Self {
        Self::new()
    }
}
