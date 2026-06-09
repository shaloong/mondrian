//! 事件路由器
//!
//! 管理 hover/focus/capture 状态，将事件分发给正确的 Widget。
//! 使用真实的 FocusManager + ShortcutManager 实现。

use mondrian_editor_state::Action;
use mondrian_platform::{NoopPlatformService, PlatformService};
use mondrian_ui_core::focus::FocusManager;
use mondrian_ui_core::types::{EventResult, Point, UiEvent, WidgetId};
use mondrian_ui_core::widget::{EventContext, EventRequests, PointerCaptureRequest};
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
    platform: Box<dyn PlatformService>,
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
            platform: Box::new(NoopPlatformService),
        }
    }

    pub fn with_platform(root_widget_id: WidgetId, platform: Box<dyn PlatformService>) -> Self {
        Self {
            root_widget_id,
            hovered: None,
            focused: None,
            captured: None,
            focus_mgr: FocusManagerImpl::new(),
            shortcut_mgr: ShortcutManagerImpl::new(),
            platform,
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
                            let mut requests = EventRequests::default();
                            {
                                let mut ctx = self.make_event_context(dispatch, &mut requests);
                                old.event(&event, &mut ctx);
                            }
                            self.apply_event_requests(requests);
                        }
                    }
                    self.hovered = target;
                }
                if let Some(target_id) = target {
                    if self.captured.is_none()
                        && self.before_child_event(tree, target_id, &event, dispatch)
                    {
                        return EventResult::Handled;
                    }
                    let mut current = Some(target_id);
                    while let Some(id) = current {
                        if let Some(widget) = tree.get_mut(id) {
                            let mut requests = EventRequests::default();
                            let result = {
                                let mut ctx = self.make_event_context(dispatch, &mut requests);
                                widget.event(&event, &mut ctx)
                            };
                            self.focused = self.focus_mgr.focused_widget();
                            self.apply_event_requests(requests);
                            match result {
                                EventResult::Handled => {
                                    self.after_child_handled(tree, id, &event, dispatch);
                                    return EventResult::Handled;
                                }
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

                let is_keyboard = matches!(
                    &event,
                    UiEvent::KeyDown { .. }
                        | UiEvent::KeyUp { .. }
                        | UiEvent::TextInput(_)
                        | UiEvent::ImePreedit(_)
                        | UiEvent::ImeCommit(_)
                );

                let target = if let Some(captured) = self.captured {
                    Some(captured)
                } else if is_keyboard {
                    // Keyboard events go to the focused widget, not hit-tested
                    self.focus_mgr.focused_widget()
                } else {
                    hit_test_deepest(tree, position)
                };

                if let Some(target_id) = target {
                    if self.captured.is_none()
                        && !is_keyboard
                        && self.before_child_event(tree, target_id, &event, dispatch)
                    {
                        return EventResult::Handled;
                    }
                    let mut current = Some(target_id);
                    while let Some(id) = current {
                        if let Some(widget) = tree.get_mut(id) {
                            let mut requests = EventRequests::default();
                            let result = {
                                let mut ctx = self.make_event_context(dispatch, &mut requests);
                                widget.event(&event, &mut ctx)
                            };
                            self.focused = self.focus_mgr.focused_widget();
                            self.apply_event_requests(requests);
                            match result {
                                EventResult::Handled => {
                                    self.after_child_handled(tree, id, &event, dispatch);
                                    return EventResult::Handled;
                                }
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
    fn make_event_context<'a>(
        &'a mut self,
        dispatch: &'a dyn Fn(Action),
        requests: &'a mut EventRequests,
    ) -> EventContext<'a> {
        static mut TT: DummyTooltip = DummyTooltip;
        EventContext {
            focus: &mut self.focus_mgr,
            shortcut: &mut self.shortcut_mgr,
            tooltip: unsafe { &mut TT },
            dispatch,
            platform: self.platform.as_ref(),
            requests,
        }
    }

    fn apply_event_requests(&mut self, requests: EventRequests) {
        match requests.pointer_capture {
            Some(PointerCaptureRequest::Capture(id)) => self.captured = Some(id),
            Some(PointerCaptureRequest::Release(id)) if self.captured == Some(id) => {
                self.captured = None;
            }
            Some(PointerCaptureRequest::Clear) => self.captured = None,
            _ => {}
        }
    }

    fn after_child_handled(
        &mut self,
        tree: &mut dyn WidgetTree,
        child_id: WidgetId,
        event: &UiEvent,
        dispatch: &dyn Fn(Action),
    ) {
        let mut current = tree.parent_id(child_id);
        while let Some(id) = current {
            let parent_id = tree.parent_id(id);
            if let Some(widget) = tree.get_mut(id) {
                let mut requests = EventRequests::default();
                {
                    let mut ctx = self.make_event_context(dispatch, &mut requests);
                    let _ = widget.after_child_event(event, &mut ctx);
                }
                self.focused = self.focus_mgr.focused_widget();
                self.apply_event_requests(requests);
            }
            current = parent_id;
        }
    }

    fn before_child_event(
        &mut self,
        tree: &mut dyn WidgetTree,
        child_id: WidgetId,
        event: &UiEvent,
        dispatch: &dyn Fn(Action),
    ) -> bool {
        let mut current = tree.parent_id(child_id);
        while let Some(id) = current {
            let parent_id = tree.parent_id(id);
            if let Some(widget) = tree.get_mut(id) {
                let mut requests = EventRequests::default();
                let result = {
                    let mut ctx = self.make_event_context(dispatch, &mut requests);
                    widget.before_child_event(event, &mut ctx)
                };
                self.focused = self.focus_mgr.focused_widget();
                self.apply_event_requests(requests);
                if result == EventResult::Handled {
                    return true;
                }
            }
            current = parent_id;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_editor_state::state::PanelKind;
    use mondrian_ui_core::focus::FocusManager;
    use mondrian_ui_core::shortcut::ShortcutManager;
    use mondrian_ui_core::types::{LayoutConstraint, MouseButton, Rect, Size};
    use mondrian_ui_core::widget::PaintContext;
    use mondrian_ui_core::Widget;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

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

    struct RecordingWidget {
        id: WidgetId,
        bounds: Rect,
        log: Rc<RefCell<Vec<String>>>,
    }

    impl RecordingWidget {
        fn new(bounds: Rect, log: Rc<RefCell<Vec<String>>>) -> Self {
            Self { id: WidgetId::new(), bounds, log }
        }
    }

    impl Widget for RecordingWidget {
        fn id(&self) -> WidgetId {
            self.id
        }

        fn measure(&self, _constraint: LayoutConstraint) -> Size {
            Size::new(self.bounds.width, self.bounds.height)
        }

        fn layout(&mut self, bounds: Rect) {
            self.bounds = bounds;
        }

        fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
            match event {
                UiEvent::MouseDown { .. } => {
                    ctx.focus.request_focus(self.id, PanelKind::Console);
                    ctx.request_pointer_capture(self.id);
                    self.log.borrow_mut().push("down".into());
                    EventResult::Handled
                }
                UiEvent::MouseMove { .. } => {
                    self.log.borrow_mut().push("move".into());
                    EventResult::Handled
                }
                UiEvent::MouseUp { .. } => {
                    ctx.release_pointer_capture(self.id);
                    self.log.borrow_mut().push("up".into());
                    EventResult::Handled
                }
                UiEvent::ImeCommit(text) => {
                    self.log.borrow_mut().push(format!("commit:{text}"));
                    EventResult::Handled
                }
                _ => EventResult::Ignored,
            }
        }

        fn paint(&self, _ctx: &mut PaintContext) {}

        fn hit_test(&self, point: Point) -> bool {
            self.bounds.contains(point)
        }
    }

    struct TestTree {
        root: WidgetId,
        nodes: HashMap<WidgetId, Box<dyn Widget>>,
    }

    impl TestTree {
        fn single(widget: RecordingWidget) -> Self {
            let root = widget.id();
            let mut nodes = HashMap::new();
            nodes.insert(root, Box::new(widget) as Box<dyn Widget>);
            Self { root, nodes }
        }
    }

    impl WidgetTree for TestTree {
        fn get(&self, id: WidgetId) -> Option<&dyn Widget> {
            self.nodes.get(&id).map(|w| w.as_ref())
        }

        fn get_mut(&mut self, id: WidgetId) -> Option<&mut dyn Widget> {
            match self.nodes.get_mut(&id) {
                Some(widget) => Some(widget.as_mut()),
                None => None,
            }
        }

        fn root_id(&self) -> WidgetId {
            self.root
        }

        fn parent_id(&self, _id: WidgetId) -> Option<WidgetId> {
            None
        }

        fn children_ids(&self, _id: WidgetId) -> Vec<WidgetId> {
            vec![]
        }
    }

    #[test]
    fn router_sends_ime_commit_to_focused_widget() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let widget = RecordingWidget::new(Rect::new(0.0, 0.0, 100.0, 30.0), Rc::clone(&log));
        let root = widget.id();
        let mut tree = TestTree::single(widget);
        let mut router = EventRouter::new(root);

        let result = router.route(
            UiEvent::MouseDown {
                position: Point::new(10.0, 10.0),
                button: MouseButton::Left,
                modifiers: mondrian_ui_core::types::Modifiers::none(),
            },
            &mut tree,
            &|_| {},
        );
        assert_eq!(result, EventResult::Handled);
        assert_eq!(router.focused(), Some(root));

        let result = router.route(UiEvent::ImeCommit("你好".into()), &mut tree, &|_| {});
        assert_eq!(result, EventResult::Handled);
        assert!(log.borrow().contains(&"commit:你好".into()));
    }

    #[test]
    fn router_keeps_pointer_events_on_captured_widget() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let widget = RecordingWidget::new(Rect::new(0.0, 0.0, 100.0, 30.0), Rc::clone(&log));
        let root = widget.id();
        let mut tree = TestTree::single(widget);
        let mut router = EventRouter::new(root);

        router.route(
            UiEvent::MouseDown {
                position: Point::new(10.0, 10.0),
                button: MouseButton::Left,
                modifiers: mondrian_ui_core::types::Modifiers::none(),
            },
            &mut tree,
            &|_| {},
        );
        assert_eq!(router.captured(), Some(root));

        router.route(
            UiEvent::MouseMove {
                position: Point::new(500.0, 500.0),
                modifiers: mondrian_ui_core::types::Modifiers::none(),
            },
            &mut tree,
            &|_| {},
        );
        assert!(log.borrow().contains(&"move".into()));

        router.route(
            UiEvent::MouseUp {
                position: Point::new(500.0, 500.0),
                button: MouseButton::Left,
                modifiers: mondrian_ui_core::types::Modifiers::none(),
            },
            &mut tree,
            &|_| {},
        );
        assert_eq!(router.captured(), None);
    }

    struct FocusEventWidget {
        id: WidgetId,
        bounds: Rect,
        log: Rc<RefCell<Vec<String>>>,
    }

    impl Widget for FocusEventWidget {
        fn id(&self) -> WidgetId {
            self.id
        }

        fn measure(&self, _constraint: LayoutConstraint) -> Size {
            Size::new(self.bounds.width, self.bounds.height)
        }

        fn layout(&mut self, bounds: Rect) {
            self.bounds = bounds;
        }

        fn event(&mut self, event: &UiEvent, _ctx: &mut EventContext) -> EventResult {
            if matches!(event, UiEvent::FocusGained) {
                self.log.borrow_mut().push("focus-gained".into());
            }
            EventResult::Ignored
        }

        fn paint(&self, _ctx: &mut PaintContext) {}

        fn hit_test(&self, point: Point) -> bool {
            self.bounds.contains(point)
        }
    }

    #[test]
    fn router_mouse_hover_does_not_send_focus_gained() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let widget = FocusEventWidget {
            id: WidgetId::new(),
            bounds: Rect::new(0.0, 0.0, 100.0, 30.0),
            log: Rc::clone(&log),
        };
        let root = widget.id;
        let mut nodes = HashMap::new();
        nodes.insert(root, Box::new(widget) as Box<dyn Widget>);
        let mut tree = TestTree { root, nodes };
        let mut router = EventRouter::new(root);

        router.route(
            UiEvent::MouseMove {
                position: Point::new(10.0, 10.0),
                modifiers: mondrian_ui_core::types::Modifiers::none(),
            },
            &mut tree,
            &|_| {},
        );

        assert!(log.borrow().is_empty());
        assert_eq!(router.focused(), None);
    }

    struct ParentPostWidget {
        id: WidgetId,
        bounds: Rect,
        child_id: WidgetId,
        handle_before: bool,
        log: Rc<RefCell<Vec<String>>>,
    }

    impl Widget for ParentPostWidget {
        fn id(&self) -> WidgetId {
            self.id
        }

        fn measure(&self, _constraint: LayoutConstraint) -> Size {
            Size::new(self.bounds.width, self.bounds.height)
        }

        fn layout(&mut self, bounds: Rect) {
            self.bounds = bounds;
        }

        fn event(&mut self, _event: &UiEvent, _ctx: &mut EventContext) -> EventResult {
            EventResult::Ignored
        }

        fn before_child_event(&mut self, _event: &UiEvent, _ctx: &mut EventContext) -> EventResult {
            if self.handle_before {
                self.log.borrow_mut().push("parent-before".into());
                EventResult::Handled
            } else {
                EventResult::Ignored
            }
        }

        fn after_child_event(&mut self, _event: &UiEvent, _ctx: &mut EventContext) -> EventResult {
            self.log.borrow_mut().push("parent-after".into());
            EventResult::Handled
        }

        fn paint(&self, _ctx: &mut PaintContext) {}

        fn hit_test(&self, point: Point) -> bool {
            self.bounds.contains(point)
        }
    }

    struct ParentChildTree {
        parent: ParentPostWidget,
        child: RecordingWidget,
    }

    impl WidgetTree for ParentChildTree {
        fn get(&self, id: WidgetId) -> Option<&dyn Widget> {
            if id == self.parent.id {
                Some(&self.parent)
            } else if id == self.child.id {
                Some(&self.child)
            } else {
                None
            }
        }

        fn get_mut(&mut self, id: WidgetId) -> Option<&mut dyn Widget> {
            if id == self.parent.id {
                Some(&mut self.parent)
            } else if id == self.child.id {
                Some(&mut self.child)
            } else {
                None
            }
        }

        fn root_id(&self) -> WidgetId {
            self.parent.id
        }

        fn parent_id(&self, id: WidgetId) -> Option<WidgetId> {
            if id == self.child.id {
                Some(self.parent.id)
            } else {
                None
            }
        }

        fn children_ids(&self, id: WidgetId) -> Vec<WidgetId> {
            if id == self.parent.id {
                vec![self.parent.child_id]
            } else {
                vec![]
            }
        }
    }

    #[test]
    fn router_calls_parent_after_child_event() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let child = RecordingWidget::new(Rect::new(0.0, 0.0, 100.0, 30.0), Rc::clone(&log));
        let child_id = child.id();
        let parent = ParentPostWidget {
            id: WidgetId::new(),
            bounds: Rect::new(0.0, 0.0, 100.0, 30.0),
            child_id,
            handle_before: false,
            log: Rc::clone(&log),
        };
        let root = parent.id;
        let mut tree = ParentChildTree { parent, child };
        let mut router = EventRouter::new(root);

        let result = router.route(
            UiEvent::MouseDown {
                position: Point::new(10.0, 10.0),
                button: MouseButton::Left,
                modifiers: mondrian_ui_core::types::Modifiers::none(),
            },
            &mut tree,
            &|_| {},
        );

        assert_eq!(result, EventResult::Handled);
        assert!(log.borrow().contains(&"parent-after".into()));
    }

    #[test]
    fn router_allows_parent_before_child_to_handle_pointer_event() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let child = RecordingWidget::new(Rect::new(0.0, 0.0, 100.0, 30.0), Rc::clone(&log));
        let child_id = child.id();
        let parent = ParentPostWidget {
            id: WidgetId::new(),
            bounds: Rect::new(0.0, 0.0, 100.0, 30.0),
            child_id,
            handle_before: true,
            log: Rc::clone(&log),
        };
        let root = parent.id;
        let mut tree = ParentChildTree { parent, child };
        let mut router = EventRouter::new(root);

        let result = router.route(
            UiEvent::MouseDown {
                position: Point::new(10.0, 10.0),
                button: MouseButton::Left,
                modifiers: mondrian_ui_core::types::Modifiers::none(),
            },
            &mut tree,
            &|_| {},
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(log.borrow().as_slice(), ["parent-before"]);
    }
}
