//! Dock panel container with a tab bar and panel content slot.
//!
//! This widget owns the common chrome used by docked editor panels: a
//! [`DockTabBar`], a [`PanelSlot`], active-tab synchronization, and overlay
//! forwarding for popups owned by panel content.

use crate::dock_tab_bar::{DockTabBar, TabInfo};
use crate::panel_slot::PanelSlot;
use mondrian_editor_state::state::PanelKind;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};

/// Function used by [`DockPanel`] to rebuild panel content for the active tab.
pub type DockPanelContentFactory = dyn FnMut(PanelKind, usize) -> Box<dyn Widget>;

/// Docked panel chrome: tab bar plus one active content slot.
pub struct DockPanel {
    id: WidgetId,
    kind: PanelKind,
    tab_bar: DockTabBar,
    content: Box<dyn Widget>,
    content_factory: Box<DockPanelContentFactory>,
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
            bounds: Rect::ZERO,
            last_active: initial_active,
            tab_height: 32.0,
        }
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
    use mondrian_ui_core::widget::DrawCommandEncoder;
    use mondrian_ui_theme::ThemePreset;
    use std::cell::Cell;
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
}
