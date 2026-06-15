//! 事件路由器
//!
//! 管理 hover/focus/capture 状态，将事件分发给正确的 Widget。
//! 使用真实的 FocusManager + ShortcutManager 实现。

use mondrian_editor_state::Action;
use mondrian_platform::{NoopPlatformService, PlatformService};
use mondrian_ui_core::focus::FocusManager;
use mondrian_ui_core::shortcut::ShortcutManager;
use mondrian_ui_core::tooltip::{TooltipManager, TooltipState};
use mondrian_ui_core::types::{
    DragPayload, EventResult, KeyCode, MouseButton, Point, UiEvent, WidgetId,
};
use mondrian_ui_core::widget::{
    CursorRequest, DragRequest, EventContext, EventRequests, EyedropperRequest, ImeRequest,
    PointerCaptureRequest,
};
use mondrian_ui_core::{TreeWalker, WidgetTree};

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
    active_drag: Option<ActiveDrag>,

    focus_mgr: FocusManagerImpl,
    shortcut_mgr: ShortcutManagerImpl,
    platform: Box<dyn PlatformService>,
    tooltip: Box<dyn TooltipManager>,
    last_ime_request: Option<ImeRequest>,
    last_cursor_request: Option<CursorRequest>,
    last_eyedropper_request: Option<EyedropperRequest>,
    repaint_requested: bool,
}

#[derive(Debug, Clone)]
struct ActiveDrag {
    payload: DragPayload,
    target: Option<WidgetId>,
}

struct NoopTooltip;
impl TooltipManager for NoopTooltip {
    fn show(&mut self, _: String, _: Point) {}
    fn hide(&mut self) {}
    fn current(&self) -> Option<&TooltipState> {
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
            active_drag: None,
            focus_mgr: FocusManagerImpl::new(),
            shortcut_mgr: ShortcutManagerImpl::new(),
            platform: Box::new(NoopPlatformService),
            tooltip: Box::new(NoopTooltip),
            last_ime_request: None,
            last_cursor_request: None,
            last_eyedropper_request: None,
            repaint_requested: false,
        }
    }

    pub fn with_platform(root_widget_id: WidgetId, platform: Box<dyn PlatformService>) -> Self {
        Self {
            root_widget_id,
            hovered: None,
            focused: None,
            captured: None,
            active_drag: None,
            focus_mgr: FocusManagerImpl::new(),
            shortcut_mgr: ShortcutManagerImpl::new(),
            platform,
            tooltip: Box::new(NoopTooltip),
            last_ime_request: None,
            last_cursor_request: None,
            last_eyedropper_request: None,
            repaint_requested: false,
        }
    }

    /// Create a router with explicit platform and tooltip services.
    ///
    /// App shells use this when they want widget tooltip requests to be backed
    /// by a real tooltip manager instead of the default no-op service.
    pub fn with_platform_and_tooltip(
        root_widget_id: WidgetId,
        platform: Box<dyn PlatformService>,
        tooltip: Box<dyn TooltipManager>,
    ) -> Self {
        Self {
            root_widget_id,
            hovered: None,
            focused: None,
            captured: None,
            active_drag: None,
            focus_mgr: FocusManagerImpl::new(),
            shortcut_mgr: ShortcutManagerImpl::new(),
            platform,
            tooltip,
            last_ime_request: None,
            last_cursor_request: None,
            last_eyedropper_request: None,
            repaint_requested: false,
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

    pub fn active_drag_payload(&self) -> Option<&DragPayload> {
        self.active_drag.as_ref().map(|drag| &drag.payload)
    }

    /// Take the latest platform IME request emitted by a widget event.
    ///
    /// The router owns focus and capture state, but the application owns the
    /// native window, so IME requests are exposed for the app shell to apply to
    /// winit or another platform backend.
    pub fn take_ime_request(&mut self) -> Option<ImeRequest> {
        self.last_ime_request.take()
    }

    /// Take the latest cursor request emitted by a widget event.
    pub fn take_cursor_request(&mut self) -> Option<CursorRequest> {
        self.last_cursor_request.take()
    }

    /// Take the latest eyedropper request emitted by a widget event.
    pub fn take_eyedropper_request(&mut self) -> Option<EyedropperRequest> {
        self.last_eyedropper_request.take()
    }

    /// Peek the latest eyedropper request without consuming it.
    pub fn peek_eyedropper_request(&self) -> Option<&EyedropperRequest> {
        self.last_eyedropper_request.as_ref()
    }

    /// Take whether any widget requested another frame since the last call.
    pub fn take_repaint_request(&mut self) -> bool {
        let requested = self.repaint_requested;
        self.repaint_requested = false;
        requested
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

    /// Return the currently visible tooltip, if the injected manager has one.
    pub fn current_tooltip(&self) -> Option<&TooltipState> {
        self.tooltip.current()
    }

    /// Advance tooltip timers in the injected manager.
    pub fn update_tooltip(&mut self, delta_ms: u64) {
        self.tooltip.update(delta_ms);
    }

    /// Return when the injected tooltip manager next needs a timer update.
    pub fn next_tooltip_update_in_ms(&self) -> Option<u64> {
        self.tooltip.next_update_in_ms()
    }

    /// 将事件路由到正确的 Widget
    pub fn route(
        &mut self,
        event: UiEvent,
        tree: &mut dyn WidgetTree,
        dispatch: &dyn Fn(Action),
    ) -> EventResult {
        self.prune_stale_widget_state(tree, dispatch);
        if self.active_drag.is_some() {
            match &event {
                UiEvent::MouseMove { position, .. } => {
                    return self.route_active_drag_move(*position, tree, dispatch);
                }
                UiEvent::MouseUp { position, button: MouseButton::Left, .. } => {
                    return self.route_active_drag_drop(*position, tree, dispatch);
                }
                UiEvent::FocusLost | UiEvent::KeyDown { key: KeyCode::Escape, .. } => {
                    self.cancel_active_drag(tree, dispatch);
                    return EventResult::Handled;
                }
                _ => {}
            }
        }
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
                // ── Tab / Shift+Tab: framework-level focus traversal ──────────
                if let UiEvent::KeyDown { key: KeyCode::Tab, modifiers } = &event {
                    let reverse = modifiers.shift;
                    let current = self.focus_mgr.focused_widget();
                    // Preserve the panel from the currently focused widget.
                    let panel = self.focus_mgr.focused_panel();
                    let traversal_origin = current.unwrap_or_default();
                    let next = if reverse {
                        TreeWalker::focus_prev(tree, traversal_origin)
                    } else {
                        TreeWalker::focus_next(tree, traversal_origin)
                    };
                    if next == current {
                        self.focused = self.focus_mgr.focused_widget();
                        return EventResult::Handled;
                    }
                    // Blur current
                    if let Some(current_id) = current {
                        self.send_focus_lost(tree, current_id, dispatch);
                    }
                    // Focus next
                    if let Some(next_id) = next {
                        self.send_focus_gained(tree, next_id, dispatch);
                    }
                    self.focus_mgr.set_focused_widget(next, panel);
                    self.focused = self.focus_mgr.focused_widget();
                    return EventResult::Handled;
                }

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

                if matches!(&event, UiEvent::MouseDown { .. }) {
                    let current_focused = self.focus_mgr.focused_widget();
                    let target_is_focusable =
                        target.is_some_and(|id| tree.get(id).is_some_and(|w| w.can_focus()));
                    if let Some(clicked_id) = target {
                        if Some(clicked_id) != current_focused {
                            if target_is_focusable {
                                // Click-to-focus: blur old, focus new before
                                // delivering MouseDown so widgets can set their
                                // internal focused state while MouseDown can
                                // still suppress keyboard-only focus rings.
                                if let Some(old) = current_focused {
                                    self.send_focus_lost(tree, old, dispatch);
                                }
                                let panel = self.focus_mgr.focused_panel();
                                self.send_focus_gained(tree, clicked_id, dispatch);
                                self.focus_mgr.set_focused_widget(Some(clicked_id), panel);
                            } else if let Some(old) = current_focused {
                                let clicked_inside_focus =
                                    self.is_ancestor_or_self(tree, old, clicked_id);
                                if !clicked_inside_focus {
                                    self.send_focus_lost(tree, old, dispatch);
                                    self.focus_mgr.release_focus(old);
                                }
                            }
                            self.focused = self.focus_mgr.focused_widget();
                        }
                    } else if let Some(old) = current_focused {
                        self.send_focus_lost(tree, old, dispatch);
                        self.focus_mgr.release_focus(old);
                        self.focused = self.focus_mgr.focused_widget();
                    }
                }

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

                if let UiEvent::KeyDown { key, modifiers } = &event {
                    if let Some(action) = self.shortcut_mgr.resolve(*key, *modifiers) {
                        dispatch(action);
                        return EventResult::Handled;
                    }
                }

                EventResult::Ignored
            }
        }
    }

    fn route_active_drag_move(
        &mut self,
        position: Point,
        tree: &mut dyn WidgetTree,
        dispatch: &dyn Fn(Action),
    ) -> EventResult {
        let Some(payload) = self.active_drag.as_ref().map(|drag| drag.payload.clone()) else {
            return EventResult::Ignored;
        };
        let target = hit_test_deepest(tree, position);
        let old_target = self.active_drag.as_ref().and_then(|drag| drag.target);
        if old_target != target {
            if let Some(old) = old_target {
                self.dispatch_direct_event(tree, old, &UiEvent::DragLeave, dispatch);
            }
            if let Some(new_target) = target {
                self.dispatch_bubbling_event(
                    tree,
                    new_target,
                    &UiEvent::DragEnter { payload, position },
                    dispatch,
                );
            }
            if let Some(drag) = self.active_drag.as_mut() {
                drag.target = target;
            }
        } else if let Some(target) = target {
            self.dispatch_bubbling_event(tree, target, &UiEvent::DragOver { position }, dispatch);
        }
        EventResult::Handled
    }

    fn route_active_drag_drop(
        &mut self,
        position: Point,
        tree: &mut dyn WidgetTree,
        dispatch: &dyn Fn(Action),
    ) -> EventResult {
        let Some(active_drag) = self.active_drag.take() else {
            return EventResult::Ignored;
        };
        self.captured = None;
        let target = hit_test_deepest(tree, position).or(active_drag.target);
        if let Some(target) = target {
            return self.dispatch_bubbling_event(
                tree,
                target,
                &UiEvent::Drop { payload: active_drag.payload, position },
                dispatch,
            );
        }
        EventResult::Handled
    }

    fn cancel_active_drag(&mut self, tree: &mut dyn WidgetTree, dispatch: &dyn Fn(Action)) {
        if let Some(active_drag) = self.active_drag.take() {
            if let Some(target) = active_drag.target {
                self.dispatch_direct_event(tree, target, &UiEvent::DragLeave, dispatch);
            }
        }
        self.captured = None;
    }

    fn dispatch_direct_event(
        &mut self,
        tree: &mut dyn WidgetTree,
        target: WidgetId,
        event: &UiEvent,
        dispatch: &dyn Fn(Action),
    ) -> EventResult {
        let Some(widget) = tree.get_mut(target) else {
            return EventResult::Ignored;
        };
        let mut requests = EventRequests::default();
        let result = {
            let mut ctx = self.make_event_context(dispatch, &mut requests);
            widget.event(event, &mut ctx)
        };
        self.focused = self.focus_mgr.focused_widget();
        self.apply_event_requests(requests);
        result
    }

    fn dispatch_bubbling_event(
        &mut self,
        tree: &mut dyn WidgetTree,
        target: WidgetId,
        event: &UiEvent,
        dispatch: &dyn Fn(Action),
    ) -> EventResult {
        let mut current = Some(target);
        while let Some(id) = current {
            let parent = tree.parent_id(id);
            if self.dispatch_direct_event(tree, id, event, dispatch) == EventResult::Handled {
                self.after_child_handled(tree, id, event, dispatch);
                return EventResult::Handled;
            }
            current = parent;
        }
        EventResult::Ignored
    }

    fn is_ancestor_or_self(
        &self,
        tree: &dyn WidgetTree,
        maybe_ancestor: WidgetId,
        mut child: WidgetId,
    ) -> bool {
        loop {
            if child == maybe_ancestor {
                return true;
            }
            let Some(parent) = tree.parent_id(child) else {
                return false;
            };
            child = parent;
        }
    }

    fn prune_stale_widget_state(&mut self, tree: &mut dyn WidgetTree, dispatch: &dyn Fn(Action)) {
        if self.captured.is_some_and(|id| tree.get(id).is_none()) {
            self.captured = None;
        }
        if self.hovered.is_some_and(|id| tree.get(id).is_none()) {
            self.hovered = None;
        }
        if self
            .active_drag
            .as_ref()
            .and_then(|drag| drag.target)
            .is_some_and(|id| tree.get(id).is_none())
        {
            if let Some(drag) = self.active_drag.as_mut() {
                drag.target = None;
            }
        }
        if let Some(focused) = self.focus_mgr.focused_widget() {
            match tree.get(focused) {
                Some(widget) if widget.can_focus() => {}
                Some(_) => {
                    self.send_focus_lost(tree, focused, dispatch);
                    self.focus_mgr.release_focus(focused);
                    self.last_ime_request = Some(ImeRequest { enabled: false, cursor_area: None });
                }
                None => {
                    self.focus_mgr.release_focus(focused);
                    self.last_ime_request = Some(ImeRequest { enabled: false, cursor_area: None });
                }
            }
        }
        self.focused = self.focus_mgr.focused_widget();
    }

    fn send_focus_lost(
        &mut self,
        tree: &mut dyn WidgetTree,
        widget_id: WidgetId,
        dispatch: &dyn Fn(Action),
    ) {
        if let Some(widget) = tree.get_mut(widget_id) {
            let mut requests = EventRequests::default();
            {
                let mut ctx = self.make_event_context(dispatch, &mut requests);
                widget.event(&UiEvent::FocusLost, &mut ctx);
            }
            self.apply_event_requests(requests);
        }
    }

    fn send_focus_gained(
        &mut self,
        tree: &mut dyn WidgetTree,
        widget_id: WidgetId,
        dispatch: &dyn Fn(Action),
    ) {
        if let Some(widget) = tree.get_mut(widget_id) {
            let mut requests = EventRequests::default();
            {
                let mut ctx = self.make_event_context(dispatch, &mut requests);
                widget.event(&UiEvent::FocusGained, &mut ctx);
            }
            self.apply_event_requests(requests);
        }
    }

    fn make_event_context<'a>(
        &'a mut self,
        dispatch: &'a dyn Fn(Action),
        requests: &'a mut EventRequests,
    ) -> EventContext<'a> {
        EventContext {
            focus: &mut self.focus_mgr,
            shortcut: &mut self.shortcut_mgr,
            tooltip: self.tooltip.as_mut(),
            dispatch,
            platform: self.platform.as_ref(),
            requests,
        }
    }

    fn apply_event_requests(&mut self, requests: EventRequests) {
        if let Some(ime) = requests.ime {
            self.last_ime_request = Some(ime);
        }
        if let Some(cursor) = requests.cursor {
            self.last_cursor_request = Some(cursor);
        }
        if let Some(eyedropper) = requests.eyedropper {
            self.last_eyedropper_request = Some(eyedropper);
        }
        if requests.repaint {
            self.repaint_requested = true;
        }
        match requests.pointer_capture {
            Some(PointerCaptureRequest::Capture(id)) => self.captured = Some(id),
            Some(PointerCaptureRequest::Release(id)) if self.captured == Some(id) => {
                self.captured = None;
            }
            Some(PointerCaptureRequest::Clear) => self.captured = None,
            _ => {}
        }
        match requests.drag {
            Some(DragRequest::Begin(payload)) => {
                self.active_drag = Some(ActiveDrag { payload, target: None });
                self.captured = None;
                self.repaint_requested = true;
            }
            Some(DragRequest::Cancel) => {
                self.active_drag = None;
                self.captured = None;
                self.repaint_requested = true;
            }
            None => {}
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
    use mondrian_core::types::AssetId;
    use mondrian_editor_state::state::PanelKind;
    use mondrian_ui_core::focus::FocusManager;
    use mondrian_ui_core::shortcut::ShortcutManager;
    use mondrian_ui_core::types::{LayoutConstraint, MouseButton, Rect, Size};
    use mondrian_ui_core::widget::PaintContext;
    use mondrian_ui_core::Widget;
    use std::cell::{Cell, RefCell};
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
        focusable: Rc<Cell<bool>>,
    }

    impl RecordingWidget {
        fn new(bounds: Rect, log: Rc<RefCell<Vec<String>>>) -> Self {
            Self {
                id: WidgetId::new(),
                bounds,
                log,
                focusable: Rc::new(Cell::new(true)),
            }
        }

        fn with_focusable_flag(
            bounds: Rect,
            log: Rc<RefCell<Vec<String>>>,
            focusable: Rc<Cell<bool>>,
        ) -> Self {
            Self { id: WidgetId::new(), bounds, log, focusable }
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
                    ctx.set_ime_enabled(true, Some(Rect::new(1.0, 2.0, 3.0, 4.0)));
                    self.log.borrow_mut().push("down".into());
                    EventResult::Handled
                }
                UiEvent::MouseMove { position, .. } => {
                    ctx.tooltip.show("tip".into(), *position);
                    ctx.request_repaint();
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
                UiEvent::DragEnter { .. } => {
                    self.log.borrow_mut().push("drag-enter".into());
                    EventResult::Handled
                }
                UiEvent::DragOver { .. } => {
                    self.log.borrow_mut().push("drag-over".into());
                    EventResult::Handled
                }
                UiEvent::DragLeave => {
                    self.log.borrow_mut().push("drag-leave".into());
                    EventResult::Handled
                }
                UiEvent::Drop { .. } => {
                    self.log.borrow_mut().push("drop".into());
                    EventResult::Handled
                }
                UiEvent::FocusLost => {
                    ctx.set_ime_enabled(false, None);
                    self.log.borrow_mut().push("focus-lost".into());
                    EventResult::Handled
                }
                _ => EventResult::Ignored,
            }
        }

        fn paint(&self, _ctx: &mut PaintContext) {}

        fn hit_test(&self, point: Point) -> bool {
            self.bounds.contains(point)
        }

        fn can_focus(&self) -> bool {
            self.focusable.get()
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

    struct KeyHandlingWidget {
        id: WidgetId,
        bounds: Rect,
    }

    impl KeyHandlingWidget {
        fn new(bounds: Rect) -> Self {
            Self { id: WidgetId::new(), bounds }
        }
    }

    impl Widget for KeyHandlingWidget {
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
            match event {
                UiEvent::KeyDown { .. } => EventResult::Handled,
                _ => EventResult::Ignored,
            }
        }

        fn paint(&self, _ctx: &mut PaintContext) {}

        fn hit_test(&self, point: Point) -> bool {
            self.bounds.contains(point)
        }

        fn can_focus(&self) -> bool {
            true
        }
    }

    #[derive(Default)]
    struct ImmediateTooltip {
        state: Option<TooltipState>,
    }

    impl TooltipManager for ImmediateTooltip {
        fn show(&mut self, text: String, position: Point) {
            self.state = Some(TooltipState { text, position, visible: true });
        }

        fn hide(&mut self) {
            self.state = None;
        }

        fn current(&self) -> Option<&TooltipState> {
            self.state.as_ref()
        }

        fn update(&mut self, _delta_ms: u64) {}
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
        let ime = router.take_ime_request().expect("focused text widget should enable IME");
        assert!(ime.enabled);
        assert_eq!(ime.cursor_area, Some(Rect::new(1.0, 2.0, 3.0, 4.0)));
        assert!(router.take_ime_request().is_none());

        let result = router.route(UiEvent::ImeCommit("你好".into()), &mut tree, &|_| {});
        assert_eq!(result, EventResult::Handled);
        assert!(log.borrow().contains(&"commit:你好".into()));
    }

    #[test]
    fn router_dispatches_shortcut_when_keydown_is_unhandled() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let widget = RecordingWidget::new(Rect::new(0.0, 0.0, 100.0, 30.0), Rc::clone(&log));
        let root = widget.id();
        let mut tree = TestTree::single(widget);
        let mut router = EventRouter::new(root);
        router.shortcut_manager_mut().register_global(
            mondrian_ui_core::shortcut::ShortcutBinding::ctrl(KeyCode::S),
            Action::SaveProject,
        );
        let dispatched = RefCell::new(Vec::new());

        let result = router.route(
            UiEvent::KeyDown {
                key: KeyCode::S,
                modifiers: mondrian_ui_core::types::Modifiers::ctrl(),
            },
            &mut tree,
            &|action| dispatched.borrow_mut().push(action),
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(dispatched.borrow().as_slice(), &[Action::SaveProject]);
    }

    #[test]
    fn router_does_not_dispatch_shortcut_when_focused_widget_handles_keydown() {
        let widget = KeyHandlingWidget::new(Rect::new(0.0, 0.0, 100.0, 30.0));
        let root = widget.id();
        let mut tree = TestTree::single(RecordingWidget::new(
            Rect::new(200.0, 200.0, 10.0, 10.0),
            Rc::new(RefCell::new(Vec::new())),
        ));
        tree.root = root;
        tree.nodes.clear();
        tree.nodes.insert(root, Box::new(widget));
        let mut router = EventRouter::new(root);
        router.focus_manager_mut().request_focus(root, PanelKind::Console);
        router.shortcut_manager_mut().register_global(
            mondrian_ui_core::shortcut::ShortcutBinding::ctrl(KeyCode::S),
            Action::SaveProject,
        );
        let dispatched = RefCell::new(Vec::new());

        let result = router.route(
            UiEvent::KeyDown {
                key: KeyCode::S,
                modifiers: mondrian_ui_core::types::Modifiers::ctrl(),
            },
            &mut tree,
            &|action| dispatched.borrow_mut().push(action),
        );

        assert_eq!(result, EventResult::Handled);
        assert!(dispatched.borrow().is_empty());
    }

    #[test]
    fn router_blurs_focused_widget_on_pointer_down_outside() {
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
        assert_eq!(router.focused(), Some(root));
        let _ = router.take_ime_request();
        router.route(
            UiEvent::MouseUp {
                position: Point::new(10.0, 10.0),
                button: MouseButton::Left,
                modifiers: mondrian_ui_core::types::Modifiers::none(),
            },
            &mut tree,
            &|_| {},
        );
        assert_eq!(router.captured(), None);

        router.route(
            UiEvent::MouseDown {
                position: Point::new(500.0, 500.0),
                button: MouseButton::Left,
                modifiers: mondrian_ui_core::types::Modifiers::none(),
            },
            &mut tree,
            &|_| {},
        );

        assert_eq!(router.focused(), None);
        assert!(log.borrow().contains(&"focus-lost".into()));
        let ime = router.take_ime_request().expect("blur should disable IME");
        assert!(!ime.enabled);
        assert_eq!(ime.cursor_area, None);
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

    #[test]
    fn router_routes_active_drag_to_hit_target_until_drop() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let widget = RecordingWidget::new(Rect::new(0.0, 0.0, 100.0, 30.0), Rc::clone(&log));
        let root = widget.id();
        let mut tree = TestTree::single(widget);
        let mut router = EventRouter::new(root);
        let asset_id = AssetId::new();
        router.active_drag = Some(ActiveDrag {
            payload: DragPayload::Asset(asset_id),
            target: None,
        });
        router.set_capture(Some(WidgetId::new()));

        let result = router.route(
            UiEvent::MouseMove {
                position: Point::new(10.0, 10.0),
                modifiers: mondrian_ui_core::types::Modifiers::none(),
            },
            &mut tree,
            &|_| {},
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(
            router.active_drag.as_ref().and_then(|drag| drag.target),
            Some(root)
        );
        assert_eq!(log.borrow().as_slice(), ["drag-enter"]);
        assert_eq!(
            router.active_drag_payload(),
            Some(&DragPayload::Asset(asset_id))
        );

        router.route(
            UiEvent::MouseMove {
                position: Point::new(20.0, 10.0),
                modifiers: mondrian_ui_core::types::Modifiers::none(),
            },
            &mut tree,
            &|_| {},
        );
        assert_eq!(log.borrow().as_slice(), ["drag-enter", "drag-over"]);

        router.route(
            UiEvent::MouseUp {
                position: Point::new(20.0, 10.0),
                button: MouseButton::Left,
                modifiers: mondrian_ui_core::types::Modifiers::none(),
            },
            &mut tree,
            &|_| {},
        );

        assert_eq!(log.borrow().as_slice(), ["drag-enter", "drag-over", "drop"]);
        assert!(router.active_drag_payload().is_none());
        assert_eq!(router.captured(), None);
    }

    #[test]
    fn router_clears_stale_capture_when_widget_tree_rebuilds() {
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

        tree.nodes.remove(&root);

        let result = router.route(
            UiEvent::MouseMove {
                position: Point::new(500.0, 500.0),
                modifiers: mondrian_ui_core::types::Modifiers::none(),
            },
            &mut tree,
            &|_| {},
        );

        assert_eq!(result, EventResult::Ignored);
        assert_eq!(router.captured(), None);
    }

    #[test]
    fn router_clears_stale_focus_when_widget_tree_rebuilds() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let widget = RecordingWidget::new(Rect::new(0.0, 0.0, 100.0, 30.0), Rc::clone(&log));
        let root = widget.id();
        let mut tree = TestTree::single(widget);
        let mut router = EventRouter::new(root);

        router.focus_manager_mut().request_focus(root, PanelKind::Console);
        assert_eq!(router.focus_manager().focused_widget(), Some(root));

        tree.nodes.remove(&root);

        let result = router.route(UiEvent::ImeCommit("ignored".into()), &mut tree, &|_| {});

        assert_eq!(result, EventResult::Ignored);
        assert_eq!(router.focused(), None);
        assert_eq!(router.focus_manager().focused_widget(), None);
    }

    #[test]
    fn router_disables_ime_when_focused_widget_disappears() {
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
        let ime = router.take_ime_request().expect("focused text input should enable IME");
        assert!(ime.enabled);
        assert!(ime.cursor_area.is_some());

        tree.nodes.remove(&root);

        let result = router.route(UiEvent::ImeCommit("ignored".into()), &mut tree, &|_| {});

        assert_eq!(result, EventResult::Ignored);
        assert_eq!(router.focused(), None);
        assert_eq!(router.focus_manager().focused_widget(), None);
        let ime = router.take_ime_request().expect("stale focused widget should disable IME");
        assert!(!ime.enabled);
        assert_eq!(ime.cursor_area, None);
    }

    #[test]
    fn router_releases_focus_when_focused_widget_becomes_unfocusable() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let focusable = Rc::new(Cell::new(true));
        let widget = RecordingWidget::with_focusable_flag(
            Rect::new(0.0, 0.0, 100.0, 30.0),
            Rc::clone(&log),
            Rc::clone(&focusable),
        );
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
        assert_eq!(router.focused(), Some(root));
        let _ = router.take_ime_request();
        log.borrow_mut().clear();

        focusable.set(false);

        let result = router.route(
            UiEvent::KeyDown {
                key: KeyCode::Enter,
                modifiers: mondrian_ui_core::types::Modifiers::none(),
            },
            &mut tree,
            &|_| {},
        );

        assert_eq!(result, EventResult::Ignored);
        assert_eq!(router.focused(), None);
        assert_eq!(router.focus_manager().focused_widget(), None);
        assert_eq!(log.borrow().as_slice(), ["focus-lost"]);
        let ime = router
            .take_ime_request()
            .expect("unfocusable focused widget should disable IME");
        assert!(!ime.enabled);
        assert_eq!(ime.cursor_area, None);
    }

    #[test]
    fn router_tab_to_same_single_focusable_keeps_focus_stable() {
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
        router.route(
            UiEvent::MouseUp {
                position: Point::new(10.0, 10.0),
                button: MouseButton::Left,
                modifiers: mondrian_ui_core::types::Modifiers::none(),
            },
            &mut tree,
            &|_| {},
        );
        assert_eq!(router.focused(), Some(root));
        let _ = router.take_ime_request();
        log.borrow_mut().clear();

        let result = router.route(
            UiEvent::KeyDown {
                key: KeyCode::Tab,
                modifiers: mondrian_ui_core::types::Modifiers::none(),
            },
            &mut tree,
            &|_| {},
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(router.focused(), Some(root));
        assert!(log.borrow().is_empty());
        assert!(router.take_ime_request().is_none());
    }

    #[test]
    fn router_exposes_repaint_request_until_taken() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let widget = RecordingWidget::new(Rect::new(0.0, 0.0, 100.0, 30.0), Rc::clone(&log));
        let root = widget.id();
        let mut tree = TestTree::single(widget);
        let mut router = EventRouter::new(root);

        router.route(
            UiEvent::MouseMove {
                position: Point::new(10.0, 10.0),
                modifiers: mondrian_ui_core::types::Modifiers::none(),
            },
            &mut tree,
            &|_| {},
        );

        assert!(router.take_repaint_request());
        assert!(!router.take_repaint_request());
    }

    #[test]
    fn router_uses_injected_tooltip_manager() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let widget = RecordingWidget::new(Rect::new(0.0, 0.0, 100.0, 30.0), Rc::clone(&log));
        let root = widget.id();
        let mut tree = TestTree::single(widget);
        let mut router = EventRouter::with_platform_and_tooltip(
            root,
            Box::new(NoopPlatformService),
            Box::new(ImmediateTooltip::default()),
        );

        router.route(
            UiEvent::MouseMove {
                position: Point::new(10.0, 20.0),
                modifiers: mondrian_ui_core::types::Modifiers::none(),
            },
            &mut tree,
            &|_| {},
        );

        let tooltip = router.current_tooltip().expect("tooltip should be visible");
        assert_eq!(tooltip.text, "tip");
        assert_eq!(tooltip.position, Point::new(10.0, 20.0));
        assert!(tooltip.visible);
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

        fn can_focus(&self) -> bool {
            true
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

    #[test]
    fn router_click_to_focus_sends_focus_gained_before_mouse_down() {
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
            UiEvent::MouseDown {
                position: Point::new(10.0, 10.0),
                button: MouseButton::Left,
                modifiers: mondrian_ui_core::types::Modifiers::none(),
            },
            &mut tree,
            &|_| {},
        );

        assert_eq!(router.focused(), Some(root));
        assert_eq!(log.borrow().as_slice(), ["focus-gained"]);
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
