//! 事件路由器
//!
//! 管理 hover/focus/capture 状态，将事件分发给正确的 Widget。

use mondrian_editor_state::Action;
use mondrian_editor_state::state::PanelKind;
use mondrian_platform::NoopPlatformService;
use mondrian_ui_core::focus::FocusManager;
use mondrian_ui_core::shortcut::{ShortcutBinding, ShortcutManager, ShortcutScope};
use mondrian_ui_core::tooltip::{TooltipManager, TooltipState};
use mondrian_ui_core::types::{EventResult, KeyCode, Modifiers, Point, UiEvent, WidgetId};
use mondrian_ui_core::widget::EventContext;
use mondrian_ui_core::WidgetTree;

use crate::hit_test::hit_test_deepest;

/// 事件路由器
pub struct EventRouter {
    root_widget_id: WidgetId,
    hovered: Option<WidgetId>,
    focused: Option<WidgetId>,
    captured: Option<WidgetId>,
    // Dummy manager instances owned here for lifetime safety
    dummy_focus: DummyFocusManager,
    dummy_shortcut: DummyShortcutManager,
    dummy_tooltip: DummyTooltipManager,
    platform: NoopPlatformService,
}

impl EventRouter {
    pub fn new(root_widget_id: WidgetId) -> Self {
        Self {
            root_widget_id,
            hovered: None,
            focused: None,
            captured: None,
            dummy_focus: DummyFocusManager,
            dummy_shortcut: DummyShortcutManager,
            dummy_tooltip: DummyTooltipManager,
            platform: NoopPlatformService,
        }
    }

    pub fn hovered(&self) -> Option<WidgetId> { self.hovered }
    pub fn focused(&self) -> Option<WidgetId> { self.focused }
    pub fn captured(&self) -> Option<WidgetId> { self.captured }

    pub fn set_capture(&mut self, widget: Option<WidgetId>) {
        self.captured = widget;
    }

    /// 将事件路由到正确的 Widget
    pub fn route(
        &mut self,
        event: UiEvent,
        tree: &mut dyn WidgetTree,
        dispatch: &dyn Fn(Action),
    ) -> EventResult {
        match &event {
            UiEvent::MouseMove { position, .. } => {
                let target = if let Some(captured) = self.captured {
                    Some(captured)
                } else {
                    hit_test_deepest(tree, *position)
                };

                if target != self.hovered {
                    if let Some(old_id) = self.hovered {
                        if let Some(old) = tree.get_mut(old_id) {
                            let mut ctx = self.make_event_context(dispatch);
                            old.event(&UiEvent::FocusLost, &mut ctx);
                        }
                    }
                    if let Some(new_id) = target {
                        if let Some(new) = tree.get_mut(new_id) {
                            let mut ctx = self.make_event_context(dispatch);
                            new.event(&UiEvent::FocusGained, &mut ctx);
                        }
                    }
                    self.hovered = target;
                }
                EventResult::Ignored
            }
            _ => {
                let position = match &event {
                    UiEvent::MouseDown { position, .. }
                    | UiEvent::MouseUp { position, .. }
                    | UiEvent::MouseWheel { position, .. }
                    | UiEvent::DragEnter { position, .. }
                    | UiEvent::DragOver { position, .. }
                    | UiEvent::Drop { position, .. } => *position,
                    _ => Point::ZERO,
                };

                let target = if let Some(captured) = self.captured {
                    Some(captured)
                } else {
                    hit_test_deepest(tree, position)
                };

                if let Some(target_id) = target {
                    let mut current = Some(target_id);
                    while let Some(id) = current {
                        if let Some(widget) = tree.get_mut(id) {
                            let mut ctx = self.make_event_context(dispatch);
                            match widget.event(&event, &mut ctx) {
                                EventResult::Handled => return EventResult::Handled,
                                EventResult::Ignored => {
                                    current = tree.parent_id(id);
                                }
                            }
                        } else {
                            break;
                        }
                    }
                }

                EventResult::Ignored
            }
        }
    }

    fn make_event_context<'a>(
        &'a mut self,
        dispatch: &'a dyn Fn(Action),
    ) -> EventContext<'a> {
        EventContext {
            focus: &mut self.dummy_focus,
            shortcut: &mut self.dummy_shortcut,
            tooltip: &mut self.dummy_tooltip,
            dispatch,
            platform: &self.platform,
        }
    }
}

// ── Dummy managers ──

struct DummyFocusManager;
impl FocusManager for DummyFocusManager {
    fn focused_widget(&self) -> Option<WidgetId> { None }
    fn focused_panel(&self) -> Option<PanelKind> { None }
    fn request_focus(&mut self, _w: WidgetId, _p: PanelKind) {}
    fn release_focus(&mut self, _w: WidgetId) {}
    fn focus_next(&mut self) {}
    fn focus_prev(&mut self) {}
    fn clear_focus(&mut self) {}
}

struct DummyShortcutManager;
impl ShortcutManager for DummyShortcutManager {
    fn register(&mut self, _scope: ShortcutScope, _binding: ShortcutBinding, _action: Action) {}
    fn unregister(&mut self, _scope: ShortcutScope, _binding: &ShortcutBinding) {}
    fn resolve(&self, _key: KeyCode, _mods: Modifiers) -> Option<Action> { None }
    fn clear_scope(&mut self, _scope: ShortcutScope) {}
    fn clear_all(&mut self) {}
}

struct DummyTooltipManager;
impl TooltipManager for DummyTooltipManager {
    fn show(&mut self, _text: String, _position: Point) {}
    fn hide(&mut self) {}
    fn current(&self) -> Option<&TooltipState> { None }
    fn update(&mut self, _delta_ms: u64) {}
}
