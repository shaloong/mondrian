//! 全局 Action 枚举
//!
//! 所有用户操作、快捷键、菜单、脚本、AI Agent 的输入，
//! 统一转为 [`Action`] 再派发给 [`EditorState`](crate::EditorState)。
//!
//! ## 设计原则
//!
//! * 每个 Action 描述 **"发生了什么"**，不描述"怎么做"
//! * Action 是纯数据（值对象），不含任何逻辑
//! * 新增 Action 只需在此处加枚举变体 + 在 `EditorDispatch::dispatch` 里处理
//! * `Custom` 变体提供无边界扩展，供脚本/AI/宏使用

use std::path::PathBuf;

use mondrian_core::{ClipId, EffectId, TimeCode, TrackId};
use serde::{Deserialize, Serialize};

use crate::state::{PanelKind, WorkspacePreset};

/// 选择目标
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SelectionTarget {
    Clip(ClipId),
    Track(TrackId),
    AllClips,
    AllTracks,
}

/// 效果应用目标
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EffectTarget {
    Clip(ClipId),
    Track(TrackId),
    SelectedClips,
}

/// 全局 Action —— 所有状态变更的统一入口
///
/// 来源可以是：UI 交互、键盘快捷键、菜单点击、AI Agent、脚本、宏录制。
/// 所有来源最终都调用 `dispatch(Action)`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    // ═══════════════════════════════════════════════════════════════════
    // 项目
    // ═══════════════════════════════════════════════════════════════════
    NewProject,
    OpenProject(PathBuf),
    SaveProject,
    SaveProjectAs(PathBuf),
    CloseProject,

    // ═══════════════════════════════════════════════════════════════════
    // 媒体
    // ═══════════════════════════════════════════════════════════════════
    ImportMedia(Vec<PathBuf>),

    // ═══════════════════════════════════════════════════════════════════
    // 时间线编辑
    // ═══════════════════════════════════════════════════════════════════
    DeleteSelection,
    SplitClipAtPlayhead,
    NudgeClip {
        clip_id: ClipId,
        delta_frames: i64,
    },
    MoveClipToTrack {
        clip_id: ClipId,
        target_track: TrackId,
        position: TimeCode,
    },
    TrimClipStart {
        clip_id: ClipId,
        new_source_in: TimeCode,
    },
    TrimClipEnd {
        clip_id: ClipId,
        new_source_out: TimeCode,
    },

    // ═══════════════════════════════════════════════════════════════════
    // 播放控制
    // ═══════════════════════════════════════════════════════════════════
    Play,
    Pause,
    TogglePlay,
    Seek(TimeCode),
    StepForward,
    StepBack,
    GoToStart,
    GoToEnd,

    // ═══════════════════════════════════════════════════════════════════
    // 选择
    // ═══════════════════════════════════════════════════════════════════
    Select(SelectionTarget),
    SelectAll,
    DeselectAll,

    // ═══════════════════════════════════════════════════════════════════
    // 效果
    // ═══════════════════════════════════════════════════════════════════
    ApplyEffect {
        target: EffectTarget,
        effect_id: EffectId,
    },
    RemoveEffect {
        clip_id: ClipId,
        effect_id: EffectId,
    },
    ReorderEffects {
        clip_id: ClipId,
        from: usize,
        to: usize,
    },

    // ═══════════════════════════════════════════════════════════════════
    // 撤销/重做
    // ═══════════════════════════════════════════════════════════════════
    Undo,
    Redo,

    // ═══════════════════════════════════════════════════════════════════
    // UI 操作
    // ═══════════════════════════════════════════════════════════════════
    TogglePanel(PanelKind),
    FocusPanel(PanelKind),
    SwitchWorkspace(WorkspacePreset),
    ToggleFullscreen,

    // ═══════════════════════════════════════════════════════════════════
    // 剪贴板
    // ═══════════════════════════════════════════════════════════════════
    Copy,
    Cut,
    Paste,
    Duplicate,

    // ═══════════════════════════════════════════════════════════════════
    // 可扩展
    // ═══════════════════════════════════════════════════════════════════
    /// 供脚本/AI Agent/宏使用的自定义 Action。
    /// `namespace` 避免命名冲突（如 `"mondrian.script"`, `"ai.agent"`）。
    Custom {
        namespace: String,
        name: String,
        payload: serde_json::Value,
    },
}
