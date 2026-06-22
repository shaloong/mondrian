//! Dock panel container with a tab bar and panel content slot.
//!
//! This widget owns the common chrome used by docked editor panels: a
//! [`DockTabBar`], a [`PanelSlot`], active-tab synchronization, and overlay
//! forwarding for popups owned by panel content.

use crate::dock_tab_bar::{DockTabBar, DockTabDropAction, TabInfo};
use crate::paint::color_with_alpha;
use crate::panel_slot::PanelSlot;
use mondrian_editor_state::state::PanelKind;
use mondrian_editor_state::Action;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};
use std::rc::Rc;

/// Function used by [`DockPanel`] to rebuild panel content for the active tab.
pub type DockPanelContentFactory = dyn FnMut(PanelKind, usize) -> Box<dyn Widget>;

/// Drop region selected when dragging one dock panel tab over another panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DockPanelDropArea {
    /// Add the dragged panel as a tab in the target group.
    Center,
    /// Split to the left of the target group.
    Left,
    /// Split to the right of the target group.
    Right,
    /// Split above the target group.
    Top,
    /// Split below the target group.
    Bottom,
}

/// Function used by [`DockPanel`] to map a panel-tab drop into an app action.
pub type DockPanelDropAction =
    dyn Fn(PanelKind, PanelKind, DockPanelDropArea, Option<usize>) -> Action + 'static;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DockPanelDockHover {
    dragged: PanelKind,
    area: Option<DockPanelDropArea>,
}

/// Docked panel chrome: tab bar plus one active content slot.
pub struct DockPanel {
    id: WidgetId,
    kind: PanelKind,
    tab_bar: DockTabBar,
    content: Box<dyn Widget>,
    content_factory: Box<DockPanelContentFactory>,
    on_panel_drop: Option<Rc<DockPanelDropAction>>,
    dock_hover: Option<DockPanelDockHover>,
    bounds: Rect,
    last_active: usize,
    tab_height: f32,
}

impl DockPanel {
    /// Create a dock panel from tabs and a content factory.
    ///
    /// The factory is called immediately for the initially active tab and again
    /// whenever the active tab changes.
    pub fn new(
        kind: PanelKind,
        tabs: Vec<TabInfo>,
        mut content_factory: impl FnMut(PanelKind, usize) -> Box<dyn Widget> + 'static,
    ) -> Self {
        let initial_active = tabs.iter().position(|tab| tab.active).unwrap_or(0);
        let active_kind = tabs.get(initial_active).and_then(|tab| tab.panel_kind).unwrap_or(kind);
        let content = Box::new(PanelSlot::new(
            active_kind,
            content_factory(active_kind, initial_active),
        ));
        Self {
            id: WidgetId::new(),
            kind,
            tab_bar: DockTabBar::new(tabs),
            content,
            content_factory: Box::new(content_factory),
            on_panel_drop: None,
            dock_hover: None,
            bounds: Rect::ZERO,
            last_active: initial_active,
            tab_height: 32.0,
        }
    }

    /// Attach an app action factory for panel-tab dock drops.
    pub fn on_panel_drop(
        mut self,
        action: impl Fn(PanelKind, PanelKind, DockPanelDropArea, Option<usize>) -> Action + 'static,
    ) -> Self {
        let action: Rc<DockPanelDropAction> = Rc::new(action);
        let tab_action: Rc<DockTabDropAction> = {
            let action = Rc::clone(&action);
            Rc::new(move |panel, target, insert_index| {
                action(panel, target, DockPanelDropArea::Center, Some(insert_index))
            })
        };
        self.tab_bar.set_tab_drop_action(Some(tab_action));
        self.on_panel_drop = Some(action);
        self
    }

    /// Current active tab index.
    pub fn active_index(&self) -> usize {
        self.tab_bar.active_index()
    }

    /// Number of visible tabs currently owned by this dock panel.
    pub fn tab_count(&self) -> usize {
        self.tab_bar.tab_count()
    }

    /// Panel kinds represented by visible tabs in this dock panel.
    pub fn tab_kinds(&self) -> Vec<PanelKind> {
        self.tab_bar.tab_panel_kinds()
    }

    /// Activate one tab and rebuild content when the active tab changes.
    pub fn set_active_index(&mut self, index: usize) {
        self.tab_bar.set_active(index);
        self.sync_active_tab();
    }

    /// Panel kind carried by the content slot.
    pub fn kind(&self) -> PanelKind {
        self.kind
    }

    fn sync_active_tab(&mut self) {
        let active = self.tab_bar.active_index();
        if active == self.last_active {
            return;
        }

        self.last_active = active;
        let active_kind = self.tab_bar.active_panel_kind().unwrap_or(self.kind);
        self.content = Box::new(PanelSlot::new(
            active_kind,
            (self.content_factory)(active_kind, active),
        ));
        if self.bounds.width > 0.0 && self.bounds.height > 0.0 {
            self.layout(self.bounds);
        }
    }

    fn content_bounds(&self) -> Rect {
        Rect::new(
            self.bounds.x,
            self.bounds.y + self.tab_height,
            self.bounds.width,
            (self.bounds.height - self.tab_height).max(0.0),
        )
    }

    fn dock_guide_rects(&self) -> Option<(Rect, Rect)> {
        let content = self.content_bounds();
        if content.width <= 0.0 || content.height <= 0.0 {
            return None;
        }
        let outer_size = content.width.min(content.height).clamp(96.0, 148.0);
        let inner_size = (outer_size * 0.44).clamp(46.0, 66.0);
        let center = content.center();
        let outer = Rect::new(
            center.x - outer_size * 0.5,
            center.y - outer_size * 0.5,
            outer_size,
            outer_size,
        );
        let inner = Rect::new(
            center.x - inner_size * 0.5,
            center.y - inner_size * 0.5,
            inner_size,
            inner_size,
        );
        Some((outer, inner))
    }

    fn drop_area_at(&self, position: Point) -> Option<DockPanelDropArea> {
        let (outer, inner) = self.dock_guide_rects()?;
        if inner.contains(position) {
            return Some(DockPanelDropArea::Center);
        }
        if !outer.contains(position) {
            return None;
        }
        if position.y < inner.y {
            Some(DockPanelDropArea::Top)
        } else if position.y > inner.y + inner.height {
            Some(DockPanelDropArea::Bottom)
        } else if position.x < inner.x {
            Some(DockPanelDropArea::Left)
        } else if position.x > inner.x + inner.width {
            Some(DockPanelDropArea::Right)
        } else {
            None
        }
    }

    fn drop_preview_rect(&self, area: DockPanelDropArea) -> Rect {
        let content = self.content_bounds();
        match area {
            DockPanelDropArea::Center => content.inset(8.0, 8.0),
            DockPanelDropArea::Left => {
                Rect::new(content.x, content.y, content.width * 0.5, content.height)
            }
            DockPanelDropArea::Right => Rect::new(
                content.x + content.width * 0.5,
                content.y,
                content.width * 0.5,
                content.height,
            ),
            DockPanelDropArea::Top => {
                Rect::new(content.x, content.y, content.width, content.height * 0.5)
            }
            DockPanelDropArea::Bottom => Rect::new(
                content.x,
                content.y + content.height * 0.5,
                content.width,
                content.height * 0.5,
            ),
        }
    }

    fn target_panel_for_drop(
        &self,
        dragged: PanelKind,
        area: DockPanelDropArea,
    ) -> Option<PanelKind> {
        let active = self.tab_bar.active_panel_kind().unwrap_or(self.kind);
        if active != dragged {
            return Some(active);
        }
        if area != DockPanelDropArea::Center {
            return self.tab_bar.tab_panel_kinds().into_iter().find(|kind| *kind != dragged);
        }
        None
    }

    fn update_panel_drop_hover(
        &mut self,
        dragged: PanelKind,
        position: Point,
        ctx: &mut EventContext,
    ) -> EventResult {
        if !self.content_bounds().contains(position) {
            return EventResult::Ignored;
        }
        let area = self
            .drop_area_at(position)
            .filter(|area| self.target_panel_for_drop(dragged, *area).is_some());
        let next = Some(DockPanelDockHover { dragged, area });
        if self.dock_hover != next {
            self.dock_hover = next;
            ctx.request_repaint();
        }
        EventResult::Handled
    }

    fn drop_panel_tab(
        &mut self,
        dragged: PanelKind,
        position: Point,
        ctx: &mut EventContext,
    ) -> EventResult {
        self.dock_hover = None;
        let Some(area) = self.drop_area_at(position) else {
            ctx.request_repaint();
            return EventResult::Ignored;
        };
        let Some(target) = self.target_panel_for_drop(dragged, area) else {
            ctx.request_repaint();
            return EventResult::Ignored;
        };
        let Some(action) = &self.on_panel_drop else {
            ctx.request_repaint();
            return EventResult::Ignored;
        };

        (ctx.dispatch)(action(dragged, target, area, None));
        ctx.request_repaint();
        EventResult::Handled
    }
}

impl Widget for DockPanel {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        constraint.constrain(Size::new(100.0, 100.0))
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        self.tab_bar
            .layout(Rect::new(bounds.x, bounds.y, bounds.width, self.tab_height));
        self.content.layout(Rect::new(
            bounds.x,
            bounds.y + self.tab_height,
            bounds.width,
            (bounds.height - self.tab_height).max(0.0),
        ));
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        match event {
            UiEvent::DragEnter { payload: DragPayload::PanelTab(panel), position } => {
                return self.update_panel_drop_hover(*panel, *position, ctx);
            }
            UiEvent::DragOver { position } => {
                if let Some(hover) = self.dock_hover {
                    return self.update_panel_drop_hover(hover.dragged, *position, ctx);
                }
            }
            UiEvent::DragLeave if self.dock_hover.is_some() => {
                self.dock_hover = None;
                ctx.request_repaint();
                return EventResult::Handled;
            }
            UiEvent::Drop { payload: DragPayload::PanelTab(panel), position } => {
                return self.drop_panel_tab(*panel, *position, ctx);
            }
            _ => {}
        }

        let result = self.tab_bar.event(event, ctx);
        self.sync_active_tab();
        if result == EventResult::Handled {
            return EventResult::Handled;
        }
        self.content.event(event, ctx)
    }

    fn after_child_event(&mut self, _event: &UiEvent, _ctx: &mut EventContext) -> EventResult {
        self.sync_active_tab();
        EventResult::Ignored
    }

    fn paint(&self, ctx: &mut PaintContext) {
        self.tab_bar.paint(ctx);
        self.content.paint(ctx);
    }

    fn paint_overlay(&self, ctx: &mut PaintContext) {
        self.content.paint_overlay(ctx);
        if let Some(hover) = self.dock_hover {
            let tokens = &ctx.theme.colors;
            if let Some(area) = hover.area {
                let rect = self.drop_preview_rect(area).inset(4.0, 4.0);
                ctx.encoder.draw_rect(rect, color_with_alpha(tokens.primary, 0.14), 6.0);
            }
            if let Some((outer, inner)) = self.dock_guide_rects() {
                let base = color_with_alpha(tokens.popover, 0.88);
                let line = color_with_alpha(tokens.border, 0.70);
                let active = color_with_alpha(tokens.primary, 0.30);
                let top = Rect::new(outer.x, outer.y, outer.width, inner.y - outer.y);
                let bottom = Rect::new(
                    outer.x,
                    inner.y + inner.height,
                    outer.width,
                    outer.y + outer.height - (inner.y + inner.height),
                );
                let left = Rect::new(outer.x, inner.y, inner.x - outer.x, inner.height);
                let right = Rect::new(
                    inner.x + inner.width,
                    inner.y,
                    outer.x + outer.width - (inner.x + inner.width),
                    inner.height,
                );
                for (area, rect) in [
                    (DockPanelDropArea::Top, top),
                    (DockPanelDropArea::Right, right),
                    (DockPanelDropArea::Bottom, bottom),
                    (DockPanelDropArea::Left, left),
                ] {
                    ctx.encoder.draw_rect(
                        rect,
                        if hover.area == Some(area) {
                            active
                        } else {
                            base
                        },
                        4.0,
                    );
                }
                ctx.encoder.draw_rect(
                    inner,
                    if hover.area == Some(DockPanelDropArea::Center) {
                        active
                    } else {
                        color_with_alpha(tokens.popover, 0.76)
                    },
                    5.0,
                );
                ctx.encoder.draw_rect(Rect::new(outer.x, outer.y, outer.width, 1.0), line, 0.0);
                ctx.encoder.draw_rect(
                    Rect::new(outer.x, outer.y + outer.height - 1.0, outer.width, 1.0),
                    line,
                    0.0,
                );
                ctx.encoder.draw_rect(Rect::new(outer.x, outer.y, 1.0, outer.height), line, 0.0);
                ctx.encoder.draw_rect(
                    Rect::new(outer.x + outer.width - 1.0, outer.y, 1.0, outer.height),
                    line,
                    0.0,
                );
            }
        }
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn child_count(&self) -> usize {
        2
    }

    fn child(&self, index: usize) -> Option<&dyn Widget> {
        match index {
            0 => Some(&self.tab_bar),
            1 => Some(self.content.as_ref()),
            _ => None,
        }
    }

    fn child_mut(&mut self, index: usize) -> Option<&mut dyn Widget> {
        match index {
            0 => Some(&mut self.tab_bar),
            1 => Some(self.content.as_mut()),
            _ => None,
        }
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{make_event_ctx, DummyFocus, DummyShortcut, DummyTooltip};
    use mondrian_core::Color;
    use mondrian_editor_state::state::PanelKind;
    use mondrian_editor_state::Action;
    use mondrian_ui_core::widget::DrawCommandEncoder;
    use mondrian_ui_theme::ThemePreset;
    use std::cell::Cell;
    use std::cell::RefCell;
    use std::rc::Rc;

    struct ProbeContent {
        id: WidgetId,
        bounds: Rect,
        overlay_painted: Rc<Cell<bool>>,
        laid_out: Rc<Cell<bool>>,
    }

    impl ProbeContent {
        fn new(overlay_painted: Rc<Cell<bool>>, laid_out: Rc<Cell<bool>>) -> Self {
            Self {
                id: WidgetId::new(),
                bounds: Rect::ZERO,
                overlay_painted,
                laid_out,
            }
        }
    }

    impl Widget for ProbeContent {
        fn id(&self) -> WidgetId {
            self.id
        }

        fn measure(&self, constraint: LayoutConstraint) -> Size {
            constraint.constrain(Size::new(80.0, 24.0))
        }

        fn layout(&mut self, bounds: Rect) {
            self.bounds = bounds;
            self.laid_out.set(true);
        }

        fn event(&mut self, _event: &UiEvent, _ctx: &mut EventContext) -> EventResult {
            EventResult::Ignored
        }

        fn paint(&self, _ctx: &mut PaintContext) {}

        fn paint_overlay(&self, _ctx: &mut PaintContext) {
            self.overlay_painted.set(true);
        }

        fn hit_test(&self, point: Point) -> bool {
            self.bounds.contains(point)
        }
    }

    #[derive(Default)]
    struct NoopEncoder;

    impl DrawCommandEncoder for NoopEncoder {
        fn push_clip(&mut self, _bounds: Rect) {}

        fn pop_clip(&mut self) {}

        fn draw_rect(&mut self, _bounds: Rect, _color: Color, _corner_radius: f32) {}

        fn draw_line(&mut self, _start: Point, _end: Point, _width: f32, _color: Color) {}

        fn draw_text(&mut self, _text: &str, _font_size: f32, _position: Point, _color: Color) {}

        fn push_translate(&mut self, _offset: glam::Vec2) {}

        fn pop_transform(&mut self) {}
    }

    fn tabs() -> Vec<TabInfo> {
        vec![
            TabInfo { label: "A".into(), active: true, panel_kind: None },
            TabInfo { label: "B".into(), active: false, panel_kind: None },
        ]
    }

    fn panel_tabs() -> Vec<TabInfo> {
        vec![
            TabInfo {
                label: "素材".into(),
                active: true,
                panel_kind: Some(PanelKind::Assets),
            },
            TabInfo {
                label: "效果".into(),
                active: false,
                panel_kind: Some(PanelKind::Effects),
            },
        ]
    }

    #[test]
    fn dock_panel_switches_tabs_and_rebuilds_content() {
        let build_count = Rc::new(Cell::new(0));
        let laid_out = Rc::new(Cell::new(false));
        let mut panel = DockPanel::new(PanelKind::Inspector, tabs(), {
            let build_count = Rc::clone(&build_count);
            let laid_out = Rc::clone(&laid_out);
            move |_kind, _active| {
                build_count.set(build_count.get() + 1);
                Box::new(ProbeContent::new(
                    Rc::new(Cell::new(false)),
                    Rc::clone(&laid_out),
                ))
            }
        });
        panel.layout(Rect::new(0.0, 0.0, 200.0, 120.0));

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let dispatch = |_| {};
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch);

        let result = panel.event(
            &UiEvent::MouseDown {
                position: Point::new(60.0, 12.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(panel.active_index(), 1);
        assert_eq!(build_count.get(), 2);
        assert!(laid_out.get());
    }

    #[test]
    fn dock_panel_set_active_index_rebuilds_content() {
        let build_count = Rc::new(Cell::new(0));
        let laid_out = Rc::new(Cell::new(false));
        let mut panel = DockPanel::new(PanelKind::Inspector, tabs(), {
            let build_count = Rc::clone(&build_count);
            let laid_out = Rc::clone(&laid_out);
            move |_kind, _active| {
                build_count.set(build_count.get() + 1);
                Box::new(ProbeContent::new(
                    Rc::new(Cell::new(false)),
                    Rc::clone(&laid_out),
                ))
            }
        });
        panel.layout(Rect::new(0.0, 0.0, 200.0, 120.0));

        panel.set_active_index(1);

        assert_eq!(panel.active_index(), 1);
        assert_eq!(build_count.get(), 2);
        assert!(laid_out.get());
    }

    #[test]
    fn dock_panel_paint_overlay_forwards_to_content() {
        let overlay_painted = Rc::new(Cell::new(false));
        let panel = DockPanel::new(PanelKind::Inspector, tabs(), {
            let overlay_painted = Rc::clone(&overlay_painted);
            move |_kind, _active| {
                Box::new(ProbeContent::new(
                    Rc::clone(&overlay_painted),
                    Rc::new(Cell::new(false)),
                ))
            }
        });
        let mut encoder = NoopEncoder;
        let theme = ThemePreset::Dark.build();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 200.0, 120.0),
        };

        panel.paint_overlay(&mut ctx);

        assert!(overlay_painted.get());
    }

    #[test]
    fn dock_panel_drop_panel_tab_dispatches_drop_action() {
        let actions = Rc::new(RefCell::new(Vec::new()));
        let mut panel = DockPanel::new(PanelKind::Assets, panel_tabs(), |_kind, _active| {
            Box::new(ProbeContent::new(
                Rc::new(Cell::new(false)),
                Rc::new(Cell::new(false)),
            ))
        })
        .on_panel_drop(|panel, target, area, tab_index| {
            assert_eq!(panel, PanelKind::Inspector);
            assert_eq!(target, PanelKind::Assets);
            assert_eq!(area, DockPanelDropArea::Center);
            assert_eq!(tab_index, None);
            Action::FocusPanel(panel)
        });
        panel.layout(Rect::new(0.0, 0.0, 240.0, 160.0));

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let dispatch_actions = Rc::clone(&actions);
        let dispatch = move |action| dispatch_actions.borrow_mut().push(action);
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch);

        let result = panel.event(
            &UiEvent::Drop {
                payload: DragPayload::PanelTab(PanelKind::Inspector),
                position: Point::new(120.0, 96.0),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(
            actions.borrow().as_slice(),
            &[Action::FocusPanel(PanelKind::Inspector)]
        );
    }

    #[test]
    fn dock_panel_drag_enter_tracks_edge_drop_preview() {
        let mut panel = DockPanel::new(PanelKind::Assets, panel_tabs(), |_kind, _active| {
            Box::new(ProbeContent::new(
                Rc::new(Cell::new(false)),
                Rc::new(Cell::new(false)),
            ))
        });
        panel.layout(Rect::new(0.0, 0.0, 240.0, 160.0));

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        let result = panel.event(
            &UiEvent::DragEnter {
                payload: DragPayload::PanelTab(PanelKind::Inspector),
                position: Point::new(62.0, 96.0),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(
            panel.dock_hover,
            Some(DockPanelDockHover {
                dragged: PanelKind::Inspector,
                area: Some(DockPanelDropArea::Left),
            })
        );
        assert!(ctx.requests.repaint);
    }

    #[test]
    fn dock_panel_content_hover_without_guide_zone_has_no_valid_drop_area() {
        let mut panel = DockPanel::new(PanelKind::Assets, panel_tabs(), |_kind, _active| {
            Box::new(ProbeContent::new(
                Rc::new(Cell::new(false)),
                Rc::new(Cell::new(false)),
            ))
        });
        panel.layout(Rect::new(0.0, 0.0, 240.0, 160.0));

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        let result = panel.event(
            &UiEvent::DragEnter {
                payload: DragPayload::PanelTab(PanelKind::Inspector),
                position: Point::new(20.0, 150.0),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(
            panel.dock_hover,
            Some(DockPanelDockHover { dragged: PanelKind::Inspector, area: None })
        );
    }

    #[test]
    fn dock_panel_ignores_center_drop_of_own_only_tab() {
        let mut panel = DockPanel::new(
            PanelKind::Inspector,
            vec![TabInfo {
                label: "检查器".into(),
                active: true,
                panel_kind: Some(PanelKind::Inspector),
            }],
            |_kind, _active| {
                Box::new(ProbeContent::new(
                    Rc::new(Cell::new(false)),
                    Rc::new(Cell::new(false)),
                ))
            },
        );
        panel.layout(Rect::new(0.0, 0.0, 240.0, 160.0));

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        let result = panel.event(
            &UiEvent::DragEnter {
                payload: DragPayload::PanelTab(PanelKind::Inspector),
                position: Point::new(120.0, 80.0),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(
            panel.dock_hover,
            Some(DockPanelDockHover { dragged: PanelKind::Inspector, area: None })
        );
    }
}
