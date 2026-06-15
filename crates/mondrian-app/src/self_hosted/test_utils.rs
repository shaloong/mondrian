//! Test helpers for self-hosted shell widgets.

use mondrian_editor_state::state::PanelKind;
use mondrian_editor_state::Action;
use mondrian_platform::NoopPlatformService;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, EventRequests};
use mondrian_ui_core::{
    FocusManager, ShortcutBinding, ShortcutContext, ShortcutManager, ShortcutScope, TooltipManager,
    TooltipState,
};

pub(crate) struct DummyFocus;

impl FocusManager for DummyFocus {
    fn focused_widget(&self) -> Option<WidgetId> {
        None
    }

    fn focused_panel(&self) -> Option<PanelKind> {
        None
    }

    fn request_focus(&mut self, _widget: WidgetId, _panel: PanelKind) {}

    fn release_focus(&mut self, _widget: WidgetId) {}

    fn focus_next(&mut self) {}

    fn focus_prev(&mut self) {}

    fn clear_focus(&mut self) {}
}

pub(crate) struct DummyShortcut;

impl ShortcutManager for DummyShortcut {
    fn register(&mut self, _scope: ShortcutScope, _binding: ShortcutBinding, _action: Action) {}

    fn unregister(&mut self, _scope: ShortcutScope, _binding: &ShortcutBinding) {}

    fn resolve(
        &self,
        _key: KeyCode,
        _modifiers: Modifiers,
        _context: ShortcutContext,
    ) -> Option<Action> {
        None
    }

    fn clear_scope(&mut self, _scope: ShortcutScope) {}

    fn clear_all(&mut self) {}
}

pub(crate) struct DummyTooltip;

impl TooltipManager for DummyTooltip {
    fn show(&mut self, _text: String, _position: Point) {}

    fn hide(&mut self) {}

    fn current(&self) -> Option<&TooltipState> {
        None
    }

    fn update(&mut self, _delta_ms: u64) {}
}

pub(crate) fn event_ctx<'a>(
    focus: &'a mut DummyFocus,
    shortcut: &'a mut DummyShortcut,
    tooltip: &'a mut DummyTooltip,
    requests: &'a mut EventRequests,
    dispatch: &'a dyn Fn(Action),
) -> EventContext<'a> {
    EventContext {
        focus,
        shortcut,
        tooltip,
        dispatch,
        platform: &NoopPlatformService,
        requests,
    }
}
