//! 全局 Action 枚举
//!
//! 所有用户操作、快捷键、菜单、脚本、AI Agent 的输入，
//! 统一转为 [`Action`]，由应用组合根派发到对应的领域 Interface。
//!
//! ## 设计原则
//!
//! * 每个 Action 描述 **"发生了什么"**，不描述"怎么做"
//! * Action 是纯数据（值对象），不含任何逻辑
//! * 每个 Action 都表示真实语义意图；未形成命令的输入由 Adapter 保留为 `None`
//! * 新增 Action 必须由应用组合根映射到一个明确的领域 Interface
//! * `Custom` 变体提供无边界扩展，供脚本/AI/宏使用

use std::path::PathBuf;

use mondrian_core::{ClipId, FramePosition, TrackId};
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
    RippleDeleteSelection,
    SplitClipAtPlayhead,
    MarkInAtPlayhead,
    MarkOutAtPlayhead,
    NudgeClip {
        clip_id: ClipId,
        delta_frames: i64,
    },
    MoveClipToTrack {
        clip_id: ClipId,
        target_track: TrackId,
        /// Exact input coordinate in the active Sequence domain. The App
        /// converts the complete frame/time-base pair before one nearest-frame
        /// lowering onto that Sequence's evaluation grid.
        position: FramePosition,
    },
    TrimClipStart {
        clip_id: ClipId,
        new_source_in: FramePosition,
    },
    TrimClipEnd {
        clip_id: ClipId,
        new_source_out: FramePosition,
    },

    // ═══════════════════════════════════════════════════════════════════
    // 播放控制
    // ═══════════════════════════════════════════════════════════════════
    Play,
    Pause,
    TogglePlay,
    /// Seek to an exact input coordinate in the active Sequence domain.
    ///
    /// The `FramePosition` time base is authoritative input, not a display hint;
    /// the App converts it before lowering once onto the active Sequence grid.
    Seek(FramePosition),
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

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::{ClipId, FramePosition, Rational, TrackId};
    use serde_json;
    use uuid::Uuid;

    // ═══════════════════════════════════════════════════════════════════════
    // Helpers
    // ═══════════════════════════════════════════════════════════════════════

    fn test_path(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    fn test_clip_id() -> ClipId {
        ClipId(Uuid::new_v4())
    }

    fn test_track_id() -> TrackId {
        TrackId(Uuid::new_v4())
    }

    fn test_timecode() -> FramePosition {
        FramePosition { frame: 42, time_base: Rational::new(30000, 1001) }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Serialization round-trip for every Action variant
    // ═══════════════════════════════════════════════════════════════════════

    /// Helper: serialize then deserialize, check equality
    fn round_trip(action: &Action) -> Action {
        let json = serde_json::to_string(action).expect("serialize");
        serde_json::from_str(&json).expect("deserialize")
    }

    #[test]
    fn round_trip_new_project() {
        assert_eq!(round_trip(&Action::NewProject), Action::NewProject);
    }

    #[test]
    fn round_trip_open_project() {
        let a = Action::OpenProject(test_path("/tmp/test.mdp"));
        assert_eq!(round_trip(&a), a);
    }

    #[test]
    fn round_trip_save_project() {
        assert_eq!(round_trip(&Action::SaveProject), Action::SaveProject);
    }

    #[test]
    fn round_trip_save_project_as() {
        let a = Action::SaveProjectAs(test_path("/tmp/output.mdp"));
        assert_eq!(round_trip(&a), a);
    }

    #[test]
    fn round_trip_close_project() {
        assert_eq!(round_trip(&Action::CloseProject), Action::CloseProject);
    }

    #[test]
    fn round_trip_import_media() {
        let a = Action::ImportMedia(vec![test_path("/videos/a.mp4"), test_path("/videos/b.mov")]);
        assert_eq!(round_trip(&a), a);
    }

    #[test]
    fn round_trip_import_media_empty() {
        let a = Action::ImportMedia(vec![]);
        assert_eq!(round_trip(&a), a);
    }

    #[test]
    fn round_trip_delete_selection() {
        assert_eq!(
            round_trip(&Action::DeleteSelection),
            Action::DeleteSelection
        );
    }

    #[test]
    fn round_trip_ripple_delete_selection() {
        assert_eq!(
            round_trip(&Action::RippleDeleteSelection),
            Action::RippleDeleteSelection
        );
    }

    #[test]
    fn round_trip_split_clip() {
        assert_eq!(
            round_trip(&Action::SplitClipAtPlayhead),
            Action::SplitClipAtPlayhead
        );
    }

    #[test]
    fn round_trip_mark_in_out_at_playhead() {
        assert_eq!(
            round_trip(&Action::MarkInAtPlayhead),
            Action::MarkInAtPlayhead
        );
        assert_eq!(
            round_trip(&Action::MarkOutAtPlayhead),
            Action::MarkOutAtPlayhead
        );
    }

    #[test]
    fn round_trip_nudge_clip() {
        let a = Action::NudgeClip { clip_id: test_clip_id(), delta_frames: 5 };
        assert_eq!(round_trip(&a), a);
    }

    #[test]
    fn round_trip_nudge_clip_negative() {
        let a = Action::NudgeClip { clip_id: test_clip_id(), delta_frames: -3 };
        assert_eq!(round_trip(&a), a);
    }

    #[test]
    fn round_trip_move_clip_to_track() {
        let a = Action::MoveClipToTrack {
            clip_id: test_clip_id(),
            target_track: test_track_id(),
            position: test_timecode(),
        };
        assert_eq!(round_trip(&a), a);
    }

    #[test]
    fn round_trip_trim_clip_start() {
        let a = Action::TrimClipStart {
            clip_id: test_clip_id(),
            new_source_in: test_timecode(),
        };
        assert_eq!(round_trip(&a), a);
    }

    #[test]
    fn round_trip_trim_clip_end() {
        let a = Action::TrimClipEnd {
            clip_id: test_clip_id(),
            new_source_out: test_timecode(),
        };
        assert_eq!(round_trip(&a), a);
    }

    #[test]
    fn round_trip_play() {
        assert_eq!(round_trip(&Action::Play), Action::Play);
    }

    #[test]
    fn round_trip_pause() {
        assert_eq!(round_trip(&Action::Pause), Action::Pause);
    }

    #[test]
    fn round_trip_toggle_play() {
        assert_eq!(round_trip(&Action::TogglePlay), Action::TogglePlay);
    }

    #[test]
    fn round_trip_seek() {
        let a = Action::Seek(test_timecode());
        assert_eq!(round_trip(&a), a);
    }

    #[test]
    fn round_trip_step_forward() {
        assert_eq!(round_trip(&Action::StepForward), Action::StepForward);
    }

    #[test]
    fn round_trip_step_back() {
        assert_eq!(round_trip(&Action::StepBack), Action::StepBack);
    }

    #[test]
    fn round_trip_go_to_start() {
        assert_eq!(round_trip(&Action::GoToStart), Action::GoToStart);
    }

    #[test]
    fn round_trip_go_to_end() {
        assert_eq!(round_trip(&Action::GoToEnd), Action::GoToEnd);
    }

    #[test]
    fn round_trip_select_clip() {
        let a = Action::Select(SelectionTarget::Clip(test_clip_id()));
        assert_eq!(round_trip(&a), a);
    }

    #[test]
    fn round_trip_select_track() {
        let a = Action::Select(SelectionTarget::Track(test_track_id()));
        assert_eq!(round_trip(&a), a);
    }

    #[test]
    fn round_trip_select_all() {
        assert_eq!(round_trip(&Action::SelectAll), Action::SelectAll);
    }

    #[test]
    fn round_trip_deselect_all() {
        assert_eq!(round_trip(&Action::DeselectAll), Action::DeselectAll);
    }

    #[test]
    fn round_trip_undo() {
        assert_eq!(round_trip(&Action::Undo), Action::Undo);
    }

    #[test]
    fn round_trip_redo() {
        assert_eq!(round_trip(&Action::Redo), Action::Redo);
    }

    #[test]
    fn round_trip_toggle_panel() {
        let a = Action::TogglePanel(PanelKind::Timeline);
        assert_eq!(round_trip(&a), a);
    }

    #[test]
    fn round_trip_focus_panel() {
        let a = Action::FocusPanel(PanelKind::Viewer);
        assert_eq!(round_trip(&a), a);
    }

    #[test]
    fn round_trip_switch_workspace() {
        let a = Action::SwitchWorkspace(WorkspacePreset::Color);
        assert_eq!(round_trip(&a), a);
    }

    #[test]
    fn round_trip_toggle_fullscreen() {
        assert_eq!(
            round_trip(&Action::ToggleFullscreen),
            Action::ToggleFullscreen
        );
    }

    #[test]
    fn round_trip_copy() {
        assert_eq!(round_trip(&Action::Copy), Action::Copy);
    }

    #[test]
    fn round_trip_cut() {
        assert_eq!(round_trip(&Action::Cut), Action::Cut);
    }

    #[test]
    fn round_trip_paste() {
        assert_eq!(round_trip(&Action::Paste), Action::Paste);
    }

    #[test]
    fn round_trip_duplicate() {
        assert_eq!(round_trip(&Action::Duplicate), Action::Duplicate);
    }

    #[test]
    fn round_trip_custom() {
        let a = Action::Custom {
            namespace: "mondrian.script".into(),
            name: "arrange_clips".into(),
            payload: serde_json::json!({"order": "asc", "gap": 10}),
        };
        assert_eq!(round_trip(&a), a);
    }

    #[test]
    fn round_trip_custom_empty_payload() {
        let a = Action::Custom {
            namespace: "test".into(),
            name: "noop".into(),
            payload: serde_json::Value::Null,
        };
        assert_eq!(round_trip(&a), a);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // JSON format stability (ensures backward compat)
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn json_format_new_project() {
        let json = serde_json::to_string(&Action::NewProject).unwrap();
        assert_eq!(json, "\"NewProject\"");
    }

    #[test]
    fn json_rejects_removed_no_op_sentinel() {
        assert!(serde_json::from_str::<Action>("\"NoOp\"").is_err());
    }

    #[test]
    fn json_format_save_project() {
        let json = serde_json::to_string(&Action::SaveProject).unwrap();
        assert_eq!(json, "\"SaveProject\"");
    }

    #[test]
    fn json_format_undo() {
        let json = serde_json::to_string(&Action::Undo).unwrap();
        assert_eq!(json, "\"Undo\"");
    }

    #[test]
    fn json_format_seek_has_frame_key() {
        let a = Action::Seek(test_timecode());
        let json = serde_json::to_string(&a).unwrap();
        // FramePosition serializes as {"frame":42,"time_base":{"num":30000,"den":1001}}
        assert!(json.contains("\"frame\""));
        assert!(json.contains("\"time_base\""));
    }

    #[test]
    fn json_format_nudge_clip_is_object() {
        let a = Action::NudgeClip { clip_id: test_clip_id(), delta_frames: 10 };
        let json = serde_json::to_string(&a).unwrap();
        // Should be a JSON object {"NudgeClip": {...}}, not a string
        assert!(json.starts_with('{'));
        assert!(json.contains("\"NudgeClip\""));
        assert!(json.contains("\"delta_frames\""));
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Action is not huge (stack size check)
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn action_size_is_reasonable() {
        // The largest variant (Custom with PathBuf, String, serde_json::Value)
        // should not be excessively large. Warn if > 200 bytes.
        let size = std::mem::size_of::<Action>();
        assert!(size <= 256, "Action size {} bytes exceeds 256", size);
    }
}
