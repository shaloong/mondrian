//! 事件路由器
//!
//! 管理 hover/focus/capture 状态，将事件分发给正确的 Widget。
//! 使用真实的 FocusManager + ShortcutManager 实现。

use mondrian_editor_state::Action;
use mondrian_platform::NoopPlatformService;
use mondrian_ui_core::types::{EventResult, Point, UiEvent, WidgetId};
use mondrian_ui_core::widget::EventContext;
use mondrian_ui_core::WidgetTree;

use crate::focus_manager::FocusManagerImpl;
use crate::hit_test::hit_test_deepest;
use crate::shortcut_manager::ShortcutManagerImpl;

/// 事件路由器
///
/// 拥有 FocusManager + ShortcutManager，在 route() 时将其注入 EventContext。
pub struct EventRouter {
    #[allow(dead_code)]
    root_widget_id: WidgetId,
    hovered: Option<WidgetId>,
    focused: Option<WidgetId>,
    captured: Option<WidgetId>,

    focus_mgr: FocusManagerImpl,
    shortcut_mgr: ShortcutManagerImpl,
    platform: NoopPlatformService,
}

struct DummyTooltip;
impl mondrian_ui_core::tooltip::TooltipManager for DummyTooltip {
    fn show(&mut self, _: String, _: Point) {}
    fn hide(&mut self) {}
    fn current(&self) -> Option<&mondrian_ui_core::tooltip::TooltipState> {
        None
    }
    fn update(&mut self, _: u64) {}
}

impl EventRouter {
    pub fn new(root_widget_id: WidgetId) -> Self {
        Self {
            root_widget_id,
            hovered: None,
            focused: None,
            captured: None,
            focus_mgr: FocusManagerImpl::new(),
            shortcut_mgr: ShortcutManagerImpl::new(),
            platform: NoopPlatformService,
        }
    }

    pub fn hovered(&self) -> Option<WidgetId> {
        self.hovered
    }
    pub fn focused(&self) -> Option<WidgetId> {
        self.focused
    }
    pub fn captured(&self) -> Option<WidgetId> {
        self.captured
    }

    pub fn set_capture(&mut self, widget: Option<WidgetId>) {
        self.captured = widget;
    }

    pub fn focus_manager(&self) -> &FocusManagerImpl {
        &self.focus_mgr
    }
    pub fn focus_manager_mut(&mut self) -> &mut FocusManagerImpl {
        &mut self.focus_mgr
    }
    pub fn shortcut_manager(&self) -> &ShortcutManagerImpl {
        &self.shortcut_mgr
    }
    pub fn shortcut_manager_mut(&mut self) -> &mut ShortcutManagerImpl {
        &mut self.shortcut_mgr
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

    #[allow(static_mut_refs)]
    fn make_event_context<'a>(&'a mut self, dispatch: &'a dyn Fn(Action)) -> EventContext<'a> {
        static mut TT: DummyTooltip = DummyTooltip;
        EventContext {
            focus: &mut self.focus_mgr,
            shortcut: &mut self.shortcut_mgr,
            tooltip: unsafe { &mut TT },
            dispatch,
            platform: &self.platform,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_editor_state::state::PanelKind;
    use mondrian_ui_core::focus::FocusManager;
    use mondrian_ui_core::shortcut::ShortcutManager;

    #[test]
    fn router_new_has_no_hover() {
        let router = EventRouter::new(WidgetId::new());
        assert_eq!(router.hovered(), None);
    }

    #[test]
    fn router_focus_manager_accessible() {
        let mut router = EventRouter::new(WidgetId::new());
        let id = WidgetId::new();
        router.focus_manager_mut().request_focus(id, PanelKind::Viewer);
        assert_eq!(router.focus_manager().focused_widget(), Some(id));
    }

    #[test]
    fn router_shortcut_manager_accessible() {
        let mut router = EventRouter::new(WidgetId::new());
        use mondrian_ui_core::shortcut::ShortcutBinding;
        router.shortcut_manager_mut().register_global(
            ShortcutBinding::ctrl(mondrian_ui_core::types::KeyCode::S),
            Action::SaveProject,
        );
        let found = router.shortcut_manager().resolve(
            mondrian_ui_core::types::KeyCode::S,
            mondrian_ui_core::types::Modifiers::ctrl(),
        );
        assert_eq!(found, Some(Action::SaveProject));
    }
}
