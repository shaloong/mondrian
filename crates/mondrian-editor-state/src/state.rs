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

    pub fn icon_name(self) -> &'static str {
        match self {
            Self::Viewer => "viewer",
            Self::Timeline => "timeline",
            Self::Assets => "assets",
            Self::Inspector => "inspector",
            Self::Effects => "effects",
            Self::Project => "project",
            Self::Console => "console",
            Self::NodeGraph => "node_graph",
            Self::Export => "export",
        }
    }

    pub const ALL: [Self; 9] = [
        Self::Viewer,
        Self::Timeline,
        Self::Assets,
        Self::Inspector,
        Self::Effects,
        Self::Project,
        Self::Console,
        Self::NodeGraph,
        Self::Export,
    ];
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

#[cfg(test)]
mod tests {
    use super::*;

    // ═══════════════════════════════════════════════════════════════════════
    // PanelKind
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn panel_kind_all_has_9_variants() {
        assert_eq!(PanelKind::ALL.len(), 9);
    }

    #[test]
    fn panel_kind_all_no_duplicates() {
        let mut seen = HashSet::new();
        for kind in PanelKind::ALL {
            assert!(seen.insert(kind), "Duplicate PanelKind: {kind:?}");
        }
    }

    #[test]
    fn panel_kind_display_name_non_empty() {
        for kind in PanelKind::ALL {
            assert!(!kind.display_name().is_empty(), "{kind:?} has empty display_name");
        }
    }

    #[test]
    fn panel_kind_icon_name_non_empty() {
        for kind in PanelKind::ALL {
            assert!(!kind.icon_name().is_empty(), "{kind:?} has empty icon_name");
        }
    }

    #[test]
    fn panel_kind_display_names_are_unique() {
        let names: Vec<&str> = PanelKind::ALL.iter().map(|k| k.display_name()).collect();
        let unique: HashSet<&str> = names.iter().copied().collect();
        assert_eq!(names.len(), unique.len(), "display_names are not unique");
    }

    #[test]
    fn panel_kind_serialization_round_trip() {
        for kind in PanelKind::ALL {
            let json = serde_json::to_string(&kind).unwrap();
            let back: PanelKind = serde_json::from_str(&json).unwrap();
            assert_eq!(kind, back);
        }
    }

    #[test]
    fn panel_kind_json_format() {
        // Verify stable format — changing these breaks saved workspaces
        assert_eq!(serde_json::to_string(&PanelKind::Viewer).unwrap(), "\"Viewer\"");
        assert_eq!(serde_json::to_string(&PanelKind::Timeline).unwrap(), "\"Timeline\"");
        assert_eq!(serde_json::to_string(&PanelKind::Console).unwrap(), "\"Console\"");
    }

    // ═══════════════════════════════════════════════════════════════════════
    // WorkspacePreset
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn workspace_preset_all_has_6_variants() {
        assert_eq!(WorkspacePreset::ALL.len(), 6);
    }

    #[test]
    fn workspace_preset_default_is_editing() {
        assert_eq!(WorkspacePreset::default(), WorkspacePreset::Editing);
    }

    #[test]
    fn workspace_preset_display_name_non_empty() {
        for preset in WorkspacePreset::ALL {
            assert!(!preset.display_name().is_empty());
        }
    }

    #[test]
    fn workspace_preset_serialization_round_trip() {
        for preset in WorkspacePreset::ALL {
            let json = serde_json::to_string(&preset).unwrap();
            let back: WorkspacePreset = serde_json::from_str(&json).unwrap();
            assert_eq!(preset, back);
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Language
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn language_default_is_chinese() {
        assert_eq!(Language::default(), Language::Chinese);
    }

    #[test]
    fn language_serialization_round_trip() {
        let json = serde_json::to_string(&Language::English).unwrap();
        let back: Language = serde_json::from_str(&json).unwrap();
        assert_eq!(Language::English, back);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // PlaybackState
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn playback_state_default_is_stopped() {
        assert_eq!(PlaybackState::default(), PlaybackState::Stopped);
    }

    #[test]
    fn playback_state_serialization_round_trip() {
        let states = [
            PlaybackState::Stopped,
            PlaybackState::Playing { timecode_frames: 100 },
            PlaybackState::Paused { timecode_frames: 50 },
        ];
        for state in &states {
            let json = serde_json::to_string(state).unwrap();
            let back: PlaybackState = serde_json::from_str(&json).unwrap();
            assert_eq!(*state, back);
        }
    }

    #[test]
    fn playback_state_playing_json_contains_frame() {
        let state = PlaybackState::Playing { timecode_frames: 42 };
        let json = serde_json::to_string(&state).unwrap();
        assert!(json.contains("42"));
        assert!(json.contains("timecode_frames"));
    }

    // ═══════════════════════════════════════════════════════════════════════
    // SelectionState
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn selection_state_default_is_empty() {
        let sel = SelectionState::default();
        assert!(sel.selected_clip_ids.is_empty());
        assert!(sel.selected_track_ids.is_empty());
    }

    #[test]
    fn selection_state_serialization_round_trip() {
        let sel = SelectionState {
            selected_clip_ids: vec![mondrian_core::ClipId(uuid::Uuid::new_v4())],
            selected_track_ids: vec![],
        };
        let json = serde_json::to_string(&sel).unwrap();
        let back: SelectionState = serde_json::from_str(&json).unwrap();
        assert_eq!(sel.selected_clip_ids, back.selected_clip_ids);
        assert_eq!(sel.selected_track_ids, back.selected_track_ids);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // EditorState
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn editor_state_new_has_no_project() {
        let state = EditorState::new();
        assert!(!state.has_open_project());
        assert!(state.project.is_none());
    }

    #[test]
    fn editor_state_new_has_default_workspace() {
        let state = EditorState::new();
        assert_eq!(state.workspace, WorkspacePreset::Editing);
    }

    #[test]
    fn editor_state_new_has_empty_sequences() {
        let state = EditorState::new();
        assert!(state.sequences.is_empty());
        assert_eq!(state.active_sequence_id, None);
    }

    #[test]
    fn editor_state_new_has_stopped_playback() {
        let state = EditorState::new();
        assert_eq!(state.playback, PlaybackState::Stopped);
    }

    #[test]
    fn editor_state_new_has_empty_active_panels() {
        let state = EditorState::new();
        assert!(state.active_panels.is_empty());
    }

    #[test]
    fn editor_state_new_has_chinese_language() {
        let state = EditorState::new();
        assert_eq!(state.language, Language::Chinese);
    }

    #[test]
    fn editor_state_new_has_event_bus() {
        let state = EditorState::new();
        // EventBus exists and is not a dangling Arc
        assert_eq!(Arc::strong_count(&state.event_bus), 1);
    }

    #[test]
    fn editor_state_default_equals_new() {
        let new_state = EditorState::new();
        let default_state = EditorState::default();
        // Note: can't compare directly (EventBus is not PartialEq), but we
        // can compare the fields that are comparable
        assert_eq!(new_state.has_open_project(), default_state.has_open_project());
        assert_eq!(new_state.workspace, default_state.workspace);
        assert_eq!(new_state.playback, default_state.playback);
    }

    #[test]
    fn editor_state_has_open_project_with_project() {
        let mut state = EditorState::new();
        assert!(!state.has_open_project());
        state.project = Some(ProjectHandle {
            path: PathBuf::from("/test/project.mdp"),
            name: "Test".into(),
        });
        assert!(state.has_open_project());
    }

    #[test]
    fn editor_state_active_sequence_returns_none_when_empty() {
        let mut state = EditorState::new();
        assert!(state.active_sequence().is_none());
        assert!(state.active_sequence_mut().is_none());
    }

    #[test]
    fn editor_state_active_sequence_returns_none_when_id_not_found() {
        let mut state = EditorState::new();
        state.active_sequence_id = Some(SequenceId(uuid::Uuid::new_v4()));
        assert!(state.active_sequence().is_none());
    }

    // ═══════════════════════════════════════════════════════════════════════
    // EventBus integration
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn event_bus_can_be_cloned_and_shared() {
        let state = EditorState::new();
        let bus1 = Arc::clone(&state.event_bus);
        let bus2 = Arc::clone(&state.event_bus);
        assert_eq!(Arc::strong_count(&state.event_bus), 3);
        drop(bus1);
        drop(bus2);
        assert_eq!(Arc::strong_count(&state.event_bus), 1);
    }
}
