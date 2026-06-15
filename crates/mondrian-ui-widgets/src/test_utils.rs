//! Shared test utilities for mondrian-ui-widgets tests.

use mondrian_editor_state::Action;
use mondrian_platform::NoopPlatformService;
use mondrian_ui_core::focus::FocusManager;
use mondrian_ui_core::shortcut::{
    ShortcutBinding, ShortcutContext, ShortcutManager, ShortcutScope,
};
use mondrian_ui_core::tooltip::{TooltipManager, TooltipState};
use mondrian_ui_core::types::{KeyCode, Modifiers, Point, WidgetId};
use mondrian_ui_core::widget::{EventContext, EventRequests};

pub(crate) struct DummyFocus;
impl FocusManager for DummyFocus {
    fn focused_widget(&self) -> Option<WidgetId> {
        None
    }
    fn focused_panel(&self) -> Option<mondrian_editor_state::state::PanelKind> {
        None
    }
    fn request_focus(&mut self, _: WidgetId, _: mondrian_editor_state::state::PanelKind) {}
    fn release_focus(&mut self, _: WidgetId) {}
    fn focus_next(&mut self) {}
    fn focus_prev(&mut self) {}
    fn clear_focus(&mut self) {}
}

pub(crate) struct DummyShortcut;
impl ShortcutManager for DummyShortcut {
    fn register(&mut self, _: ShortcutScope, _: ShortcutBinding, _: Action) {}
    fn unregister(&mut self, _: ShortcutScope, _: &ShortcutBinding) {}
    fn resolve(&self, _: KeyCode, _: Modifiers, _: ShortcutContext) -> Option<Action> {
        None
    }
    fn clear_scope(&mut self, _: ShortcutScope) {}
    fn clear_all(&mut self) {}
}

pub(crate) struct DummyTooltip;
impl TooltipManager for DummyTooltip {
    fn show(&mut self, _: String, _: Point) {}
    fn hide(&mut self) {}
    fn current(&self) -> Option<&TooltipState> {
        None
    }
    fn update(&mut self, _: u64) {}
}

pub(crate) fn make_event_ctx<'a>(
    focus: &'a mut dyn FocusManager,
    shortcut: &'a mut dyn ShortcutManager,
    tooltip: &'a mut dyn TooltipManager,
    dispatch: &'a dyn Fn(Action),
) -> EventContext<'a> {
    let requests: &'a mut EventRequests = Box::leak(Box::new(EventRequests::default()));
    EventContext {
        focus,
        shortcut,
        tooltip,
        dispatch,
        platform: &NoopPlatformService,
        requests,
    }
}
