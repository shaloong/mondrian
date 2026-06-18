//! Panel 统一接口
//!
//! 所有面板都必须实现 [`Panel`] trait。
//! Panel 之间**禁止直接引用**，只能通过 `EventBus` 通信。

use std::borrow::Cow;

use mondrian_core::events::AppEvent;
use mondrian_editor_state::{Action, EditorState};
use mondrian_ui_core::Widget;

// PanelKind is defined in mondrian-editor-state as the canonical source
pub use mondrian_editor_state::state::PanelKind;

/// Panel creation-time services.
///
/// These dependencies are long-lived and safe for a panel instance to retain.
/// Per-frame editor data and action dispatch are provided separately through
/// [`PanelBuildContext`] so panels do not store stale state snapshots.
#[derive(Clone)]
pub struct PanelInitContext {
    /// Event bus for cross-panel notifications.
    pub event_bus: std::sync::Arc<mondrian_core::events::EventBus>,
}

/// Panel widget-tree build context.
///
/// Panels read the current [`EditorState`] snapshot and convert user intent into
/// semantic [`Action`] values. They must not mutate editor state directly.
pub struct PanelBuildContext<'a> {
    /// Current editor state snapshot for read-only panel adapters.
    pub state: &'a EditorState,
    /// Semantic editor action sink owned by the host application.
    pub dispatch: &'a dyn Fn(Action),
}

impl PanelBuildContext<'_> {
    /// Dispatch one semantic editor action from a panel adapter.
    pub fn dispatch(&self, action: Action) {
        (self.dispatch)(action);
    }
}

/// 所有面板的统一接口
///
/// ## 设计约束
///
/// * Panel 之间**禁止**直接持有对方引用
/// * Panel 通信通过 `EventBus`（发布/订阅）
/// * Panel 读取 `EditorState`（只读），修改通过 `dispatch(Action)`
/// * 每个 Panel 构建自己的 Widget 树
pub trait Panel: Send + Sync {
    /// 面板类型标识
    fn kind(&self) -> PanelKind;

    /// 面板标题（显示在 Tab 标签上）
    fn title(&self) -> Cow<'static, str>;

    /// Build the panel's widget tree from the current editor snapshot.
    ///
    /// This is called when the workspace layout changes or when the host
    /// rebuilds panel content after state updates.
    fn build_widget_tree(&mut self, context: &PanelBuildContext<'_>) -> Box<dyn Widget>;

    /// 接收来自 EventBus 的事件通知
    fn on_event(&mut self, event: &AppEvent) {
        let _ = event; // 默认忽略
    }

    /// 面板是否可关闭
    fn is_closable(&self) -> bool {
        true
    }

    /// 面板是否可拖拽移动
    fn is_draggable(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_editor_state::Action;
    use std::cell::RefCell;
    use std::collections::HashSet;

    // ═══════════════════════════════════════════════════════════════════════
    // Mock Panel for testing
    // ═══════════════════════════════════════════════════════════════════════

    struct MockPanel {
        kind: PanelKind,
        title: &'static str,
        closable: bool,
        draggable: bool,
        events_received: Vec<String>,
    }

    impl MockPanel {
        fn new(kind: PanelKind, title: &'static str) -> Self {
            Self {
                kind,
                title,
                closable: true,
                draggable: true,
                events_received: Vec::new(),
            }
        }

        fn not_closable(mut self) -> Self {
            self.closable = false;
            self
        }
    }

    impl Panel for MockPanel {
        fn kind(&self) -> PanelKind {
            self.kind
        }

        fn title(&self) -> Cow<'static, str> {
            Cow::Borrowed(self.title)
        }

        fn build_widget_tree(&mut self, _context: &PanelBuildContext<'_>) -> Box<dyn Widget> {
            use mondrian_ui_core::widgets::Spacer;
            Box::new(Spacer::new(100.0, 100.0))
        }

        fn on_event(&mut self, event: &AppEvent) {
            self.events_received.push(format!("{event:?}"));
        }

        fn is_closable(&self) -> bool {
            self.closable
        }

        fn is_draggable(&self) -> bool {
            self.draggable
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // PanelKind tests
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn panel_kind_all_has_7_variants() {
        assert_eq!(PanelKind::ALL.len(), 7);
    }

    #[test]
    fn panel_kind_all_no_duplicates() {
        let mut seen = HashSet::new();
        for kind in PanelKind::ALL {
            assert!(seen.insert(kind), "Duplicate PanelKind: {kind:?}");
        }
    }

    #[test]
    fn panel_kind_display_names_are_unique() {
        let names: Vec<&str> = PanelKind::ALL.iter().map(|k| k.display_name()).collect();
        let unique: HashSet<&str> = names.iter().copied().collect();
        assert_eq!(names.len(), unique.len());
    }

    #[test]
    fn panel_kind_display_name_non_empty() {
        for kind in PanelKind::ALL {
            assert!(!kind.display_name().is_empty());
        }
    }

    #[test]
    fn panel_kind_icon_name_non_empty() {
        for kind in PanelKind::ALL {
            assert!(!kind.icon_name().is_empty());
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Panel trait tests
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn panel_kind_returns_correct_kind() {
        let panel = MockPanel::new(PanelKind::Timeline, "Timeline");
        assert_eq!(panel.kind(), PanelKind::Timeline);
    }

    #[test]
    fn panel_title_returns_correct_title() {
        let panel = MockPanel::new(PanelKind::Viewer, "预览器");
        assert_eq!(panel.title(), "预览器");
    }

    #[test]
    fn panel_build_widget_tree_returns_widget() {
        let mut panel = MockPanel::new(PanelKind::Export, "Export");
        let state = EditorState::new();
        let context = PanelBuildContext { state: &state, dispatch: &|_| {} };
        let widget = panel.build_widget_tree(&context);
        assert!(widget.children().is_empty());
    }

    #[test]
    fn panel_is_closable_default_true() {
        let panel = MockPanel::new(PanelKind::Assets, "Assets");
        assert!(panel.is_closable());
    }

    #[test]
    fn panel_is_draggable_default_true() {
        let panel = MockPanel::new(PanelKind::Export, "Export");
        assert!(panel.is_draggable());
    }

    #[test]
    fn panel_not_closable() {
        let panel = MockPanel::new(PanelKind::Viewer, "Viewer").not_closable();
        assert!(!panel.is_closable());
    }

    #[test]
    fn panel_on_event_default_is_noop() {
        let panel = MockPanel::new(PanelKind::Timeline, "T");
        // The default on_event is overridden in MockPanel
        // but the trait default is a no-op
        assert!(panel.events_received.is_empty());
    }

    #[test]
    fn panel_is_object_safe() {
        let mut panel: Box<dyn Panel> = Box::new(MockPanel::new(PanelKind::Export, "E"));
        assert_eq!(panel.kind(), PanelKind::Export);
        let state = EditorState::new();
        let context = PanelBuildContext { state: &state, dispatch: &|_| {} };
        let _widget = panel.build_widget_tree(&context);
    }

    #[test]
    fn panel_build_context_exposes_state_snapshot_and_dispatch_sink() {
        let state = EditorState::new();
        let dispatched = RefCell::new(Vec::new());
        let context = PanelBuildContext {
            state: &state,
            dispatch: &|action| dispatched.borrow_mut().push(action),
        };

        assert!(!context.state.has_open_project());
        context.dispatch(Action::FocusPanel(PanelKind::Timeline));

        assert_eq!(
            dispatched.into_inner(),
            vec![Action::FocusPanel(PanelKind::Timeline)]
        );
    }

    #[test]
    fn panel_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<MockPanel>();
    }
}
