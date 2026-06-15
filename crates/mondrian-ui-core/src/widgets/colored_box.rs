//! ColoredBox —— 纯色矩形 Widget（用于测试）

use mondrian_core::Color;

use crate::types::{LayoutConstraint, Point, Rect, Size, WidgetId};
use crate::widget::{EventContext, PaintContext};
use crate::{EventResult, UiEvent, Widget};

/// 纯色矩形 Widget
pub struct ColoredBox {
    id: WidgetId,
    pub color: Color,
    hovered: bool,
    bounds: Rect,
    preferred: Size,
    pub label: &'static str,
}

impl ColoredBox {
    pub fn new(color: Color, width: f32, height: f32) -> Self {
        Self {
            id: WidgetId::new(),
            color,
            hovered: false,
            bounds: Rect::ZERO,
            preferred: Size::new(width, height),
            label: "",
        }
    }

    pub fn with_label(mut self, label: &'static str) -> Self {
        self.label = label;
        self
    }
}

impl Widget for ColoredBox {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, _constraint: LayoutConstraint) -> Size {
        self.preferred
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
    }

    fn event(&mut self, event: &UiEvent, _ctx: &mut EventContext) -> EventResult {
        match event {
            UiEvent::FocusGained => {
                self.hovered = true;
                EventResult::Handled
            }
            UiEvent::FocusLost => {
                self.hovered = false;
                EventResult::Handled
            }
            _ => EventResult::Ignored,
        }
    }

    fn paint(&self, ctx: &mut PaintContext) {
        ctx.encoder.draw_rect(self.bounds, self.color, 0.0);
        if !self.label.is_empty() {
            ctx.encoder.draw_text(
                self.label,
                12.0,
                Point::new(self.bounds.x + 4.0, self.bounds.y + 4.0),
                Color::WHITE,
            );
        }
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Modifiers;
    use crate::widget::{DrawCommandEncoder, EventRequests};
    use crate::EventContext;
    use glam::Vec2;
    use mondrian_editor_state::state::PanelKind;
    use mondrian_editor_state::Action;
    use mondrian_platform::NoopPlatformService;
    use mondrian_ui_theme::Theme;

    struct MockEncoder {
        pub draw_count: usize,
    }

    impl MockEncoder {
        fn new() -> Self {
            Self { draw_count: 0 }
        }
    }

    impl DrawCommandEncoder for MockEncoder {
        fn push_clip(&mut self, _bounds: Rect) {}
        fn pop_clip(&mut self) {}
        fn draw_rect(&mut self, _bounds: Rect, _color: Color, _corner_radius: f32) {
            self.draw_count += 1;
        }
        fn draw_line(&mut self, _start: Point, _end: Point, _width: f32, _color: Color) {}
        fn draw_text(&mut self, _text: &str, _font_size: f32, _position: Point, _color: Color) {}
        fn push_translate(&mut self, _offset: Vec2) {}
        fn pop_transform(&mut self) {}
    }

    struct MockFocus;

    impl crate::focus::FocusManager for MockFocus {
        fn focused_widget(&self) -> Option<WidgetId> {
            None
        }
        fn focused_panel(&self) -> Option<PanelKind> {
            None
        }
        fn request_focus(&mut self, _widget: WidgetId) {}
        fn release_focus(&mut self, _widget: WidgetId) {}
        fn focus_next(&mut self) {}
        fn focus_prev(&mut self) {}
        fn clear_focus(&mut self) {}
    }

    struct MockShortcut;

    impl crate::shortcut::ShortcutManager for MockShortcut {
        fn register(&mut self, _s: crate::ShortcutScope, _b: crate::ShortcutBinding, _a: Action) {}
        fn unregister(&mut self, _s: crate::ShortcutScope, _b: &crate::ShortcutBinding) {}
        fn resolve(
            &self,
            _k: crate::KeyCode,
            _m: Modifiers,
            _c: crate::ShortcutContext,
        ) -> Option<Action> {
            None
        }
        fn clear_scope(&mut self, _s: crate::ShortcutScope) {}
        fn clear_all(&mut self) {}
    }

    struct MockTooltip;

    impl crate::tooltip::TooltipManager for MockTooltip {
        fn show(&mut self, _text: String, _position: Point) {}
        fn hide(&mut self) {}
        fn current(&self) -> Option<&crate::TooltipState> {
            None
        }
        fn update(&mut self, _delta_ms: u64) {}
    }

    fn leak_theme() -> &'static Theme {
        Box::leak(Box::new(mondrian_ui_theme::ThemePreset::Dark.build()))
    }

    #[test]
    fn colored_box_measure_returns_preferred() {
        let w = ColoredBox::new(Color::from_hex(0xFF0000), 100.0, 50.0);
        assert_eq!(w.measure(LayoutConstraint::LOOSE), Size::new(100.0, 50.0));
    }

    #[test]
    fn colored_box_measure_ignores_constraint() {
        let w = ColoredBox::new(Color::from_hex(0xFF0000), 100.0, 50.0);
        // Even when constraint is tight 30x30, measure returns preferred
        assert_eq!(
            w.measure(LayoutConstraint::tight(30.0, 30.0)),
            Size::new(100.0, 50.0)
        );
    }

    #[test]
    fn colored_box_layout_updates_bounds() {
        let mut w = ColoredBox::new(Color::from_hex(0xFF0000), 100.0, 50.0);
        let bounds = Rect::new(10.0, 10.0, 200.0, 100.0);
        w.layout(bounds);
        assert!(w.hit_test(Point::new(110.0, 60.0)));
    }

    #[test]
    fn colored_box_hit_test_respects_bounds() {
        let mut w = ColoredBox::new(Color::from_hex(0xFF0000), 100.0, 50.0);
        w.layout(Rect::new(0.0, 0.0, 100.0, 50.0));
        assert!(w.hit_test(Point::new(0.0, 0.0)));
        assert!(w.hit_test(Point::new(100.0, 50.0)));
        assert!(!w.hit_test(Point::new(101.0, 0.0)));
        assert!(!w.hit_test(Point::new(-1.0, 0.0)));
    }

    #[test]
    fn colored_box_focus_gained_sets_hovered() {
        let mut w = ColoredBox::new(Color::from_hex(0xFF0000), 100.0, 50.0);
        assert!(!w.hovered);

        let _theme = leak_theme();
        let mut focus = MockFocus;
        let mut shortcut = MockShortcut;
        let mut tooltip = MockTooltip;
        let platform = NoopPlatformService;
        let mut requests = EventRequests::default();

        let mut ctx = EventContext {
            focus: &mut focus,
            shortcut: &mut shortcut,
            tooltip: &mut tooltip,
            dispatch: &|_| {},
            platform: &platform,
            requests: &mut requests,
        };

        let result = w.event(&UiEvent::FocusGained, &mut ctx);
        assert_eq!(result, EventResult::Handled);
        assert!(w.hovered);
    }

    #[test]
    fn colored_box_focus_lost_clears_hovered() {
        let mut w = ColoredBox::new(Color::from_hex(0xFF0000), 100.0, 50.0);
        w.hovered = true;

        let _theme = leak_theme();
        let mut focus = MockFocus;
        let mut shortcut = MockShortcut;
        let mut tooltip = MockTooltip;
        let platform = NoopPlatformService;
        let mut requests = EventRequests::default();
        let mut ctx = EventContext {
            focus: &mut focus,
            shortcut: &mut shortcut,
            tooltip: &mut tooltip,
            dispatch: &|_| {},
            platform: &platform,
            requests: &mut requests,
        };

        w.event(&UiEvent::FocusLost, &mut ctx);
        assert!(!w.hovered);
    }

    #[test]
    fn colored_box_unknown_event_is_ignored() {
        let mut w = ColoredBox::new(Color::from_hex(0xFF0000), 100.0, 50.0);
        let _theme = leak_theme();
        let mut focus = MockFocus;
        let mut shortcut = MockShortcut;
        let mut tooltip = MockTooltip;
        let platform = NoopPlatformService;
        let mut requests = EventRequests::default();
        let mut ctx = EventContext {
            focus: &mut focus,
            shortcut: &mut shortcut,
            tooltip: &mut tooltip,
            dispatch: &|_| {},
            platform: &platform,
            requests: &mut requests,
        };

        let result = w.event(
            &UiEvent::MouseDown {
                position: Point::ZERO,
                button: crate::MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(result, EventResult::Ignored);
    }

    #[test]
    fn colored_box_paint_encodes_rect() {
        let mut w = ColoredBox::new(Color::from_hex(0xFF0000), 100.0, 50.0);
        w.layout(Rect::new(0.0, 0.0, 100.0, 50.0));

        let theme = leak_theme();
        let mut encoder = MockEncoder::new();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme,
            clip_rect: Rect::new(0.0, 0.0, 1920.0, 1080.0),
        };

        w.paint(&mut ctx);
        assert_eq!(encoder.draw_count, 1);
    }
}
