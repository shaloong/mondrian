//! Slider 控件
//!
//! 拖拽滑块，用于数值调节。

use mondrian_editor_state::Action;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};

/// Adapter that maps the current slider value to an editor [`Action`].
pub type SliderChangeAction = dyn Fn(f32) -> Action;

/// Slider Widget —— 拖拽滑块控制数值
pub struct Slider {
    id: WidgetId,
    value: f32,
    min: f32,
    max: f32,
    bounds: Rect,
    dragging: bool,
    focused: bool,
    focus_visible: bool,
    track_height: f32,
    thumb_size: f32,
    on_change: Option<Box<SliderChangeAction>>,
}

impl Slider {
    /// Create a slider with a clamped initial value.
    pub fn new(value: f32, min: f32, max: f32) -> Self {
        Self {
            id: WidgetId::new(),
            value: value.clamp(min, max),
            min,
            max,
            bounds: Rect::ZERO,
            dragging: false,
            focused: false,
            focus_visible: false,
            track_height: 6.0,
            thumb_size: 14.0,
            on_change: None,
        }
    }

    /// Return the current value.
    pub fn value(&self) -> f32 {
        self.value
    }

    /// Dispatch an action whenever user input changes the value.
    pub fn on_change(mut self, action: impl Fn(f32) -> Action + 'static) -> Self {
        self.on_change = Some(Box::new(action));
        self
    }

    fn range(&self) -> f32 {
        self.max - self.min
    }

    fn ratio(&self) -> f32 {
        let range = self.range();
        if range > 0.0 {
            ((self.value - self.min) / range).clamp(0.0, 1.0)
        } else {
            0.5
        }
    }

    fn effective_thumb_size(&self) -> f32 {
        self.thumb_size.min((self.bounds.height - 2.0).max(1.0))
    }

    fn track_rect(&self) -> Rect {
        let thumb_size = self.effective_thumb_size();
        let track_height = self.track_height.min(self.bounds.height.max(1.0));
        let track_y = self.bounds.y + self.bounds.height * 0.5 - track_height * 0.5;
        Rect::new(
            self.bounds.x + thumb_size * 0.5,
            track_y,
            (self.bounds.width - thumb_size).max(0.0),
            track_height,
        )
    }

    fn thumb_rect(&self) -> Rect {
        let thumb_size = self.effective_thumb_size();
        let track = self.track_rect();
        let center_x = track.x + track.width * self.ratio();
        let x = center_x - thumb_size * 0.5;
        let y = self.bounds.y + (self.bounds.height - thumb_size) * 0.5;
        Rect::new(x, y, thumb_size, thumb_size)
    }

    fn set_value(&mut self, value: f32) -> bool {
        let next = value.clamp(self.min, self.max);
        if (next - self.value).abs() <= f32::EPSILON {
            return false;
        }
        self.value = next;
        true
    }

    fn set_value_from_input(&mut self, value: f32, ctx: &mut EventContext) -> bool {
        if !self.set_value(value) {
            return false;
        }
        self.dispatch_change(ctx);
        ctx.request_repaint();
        true
    }

    fn dispatch_change(&self, ctx: &mut EventContext) {
        if let Some(action) = &self.on_change {
            (ctx.dispatch)(action(self.value));
        }
    }

    fn keyboard_step(&self, modifiers: Modifiers) -> f32 {
        let range = self.range().abs();
        if modifiers.alt {
            range / 1000.0
        } else if modifiers.shift {
            range / 10.0
        } else {
            range / 100.0
        }
    }

    fn nudge(&mut self, delta: f32, ctx: &mut EventContext) -> EventResult {
        if self.range() <= 0.0 {
            return EventResult::Ignored;
        }
        if self.set_value_from_input(self.value + delta, ctx) {
            EventResult::Handled
        } else {
            EventResult::Ignored
        }
    }
}

impl Widget for Slider {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        constraint.constrain(Size::new(100.0, self.thumb_size + 4.0))
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        match event {
            UiEvent::MouseDown { position, button: MouseButton::Left, .. }
                if self.bounds.contains(*position) =>
            {
                self.dragging = true;
                self.focused = true;
                self.focus_visible = false;
                ctx.request_pointer_capture(self.id);
                self.update_value(position, ctx);
                EventResult::Handled
            }
            UiEvent::MouseMove { position, .. } if self.dragging => {
                self.update_value(position, ctx);
                EventResult::Handled
            }
            UiEvent::MouseUp { button: MouseButton::Left, .. } if self.dragging => {
                self.dragging = false;
                ctx.release_pointer_capture(self.id);
                EventResult::Handled
            }
            UiEvent::FocusGained => {
                self.focused = true;
                self.focus_visible = true;
                EventResult::Handled
            }
            UiEvent::FocusLost => {
                self.focused = false;
                self.focus_visible = false;
                self.dragging = false;
                ctx.release_pointer_capture(self.id);
                EventResult::Handled
            }
            UiEvent::KeyDown { key, modifiers } if self.focused => {
                let step = self.keyboard_step(*modifiers);
                match key {
                    KeyCode::Left | KeyCode::Down => self.nudge(-step, ctx),
                    KeyCode::Right | KeyCode::Up => self.nudge(step, ctx),
                    KeyCode::PageDown => self.nudge(-(self.range().abs() / 10.0), ctx),
                    KeyCode::PageUp => self.nudge(self.range().abs() / 10.0, ctx),
                    KeyCode::Home => {
                        if self.set_value_from_input(self.min, ctx) {
                            EventResult::Handled
                        } else {
                            EventResult::Ignored
                        }
                    }
                    KeyCode::End => {
                        if self.set_value_from_input(self.max, ctx) {
                            EventResult::Handled
                        } else {
                            EventResult::Ignored
                        }
                    }
                    _ => EventResult::Ignored,
                }
            }
            _ => EventResult::Ignored,
        }
    }

    fn paint(&self, ctx: &mut PaintContext) {
        let tokens = &ctx.theme.colors;
        let spacing = &ctx.theme.spacing;

        // Track background
        let track = self.track_rect();
        ctx.encoder.draw_rect(track, tokens.accent, spacing.radius_full);

        // Filled track
        let ratio = self.ratio();
        let fill_w = track.width * ratio;
        if fill_w > 0.0 {
            let track_fill = Rect::new(track.x, track.y, fill_w, track.height);
            ctx.encoder.draw_rect(track_fill, tokens.primary, spacing.radius_full);
        }

        let thumb_rect = self.thumb_rect();
        if self.focus_visible {
            let mut ring = tokens.ring;
            ring.a = 0.38;
            let halo = Rect::new(
                thumb_rect.x - 3.0,
                thumb_rect.y - 3.0,
                thumb_rect.width + 6.0,
                thumb_rect.height + 6.0,
            );
            ctx.encoder.draw_rect(halo, ring, halo.height * 0.5);
        }
        ctx.encoder.draw_rect(thumb_rect, tokens.primary, thumb_rect.height * 0.5);
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn can_focus(&self) -> bool {
        true
    }
}

impl Slider {
    fn update_value(&mut self, position: &Point, ctx: &mut EventContext) {
        let range = self.range();
        if range <= 0.0 {
            return;
        }
        let track = self.track_rect();
        let ratio = if track.width > 0.0 {
            ((position.x - track.x) / track.width).clamp(0.0, 1.0)
        } else if position.x >= self.bounds.x + self.bounds.width * 0.5 {
            1.0
        } else {
            0.0
        };
        self.set_value_from_input(self.min + range * ratio, ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{make_event_ctx, DummyFocus, DummyShortcut, DummyTooltip};
    use mondrian_ui_core::widget::DrawCommandEncoder;
    use mondrian_ui_theme::ThemePreset;
    use std::cell::RefCell;

    #[derive(Default)]
    struct RecordingEncoder {
        rects: Vec<Rect>,
    }

    impl DrawCommandEncoder for RecordingEncoder {
        fn push_clip(&mut self, _bounds: Rect) {}

        fn pop_clip(&mut self) {}

        fn draw_rect(&mut self, bounds: Rect, _color: mondrian_core::Color, _corner_radius: f32) {
            self.rects.push(bounds);
        }

        fn draw_line(
            &mut self,
            _start: Point,
            _end: Point,
            _width: f32,
            _color: mondrian_core::Color,
        ) {
        }

        fn draw_text(
            &mut self,
            _text: &str,
            _font_size: f32,
            _position: Point,
            _color: mondrian_core::Color,
        ) {
        }

        fn push_translate(&mut self, _offset: glam::Vec2) {}

        fn pop_transform(&mut self) {}
    }

    fn event_ctx() -> EventContext<'static> {
        let f: &'static mut DummyFocus = Box::leak(Box::new(DummyFocus));
        let s: &'static mut DummyShortcut = Box::leak(Box::new(DummyShortcut));
        let t: &'static mut DummyTooltip = Box::leak(Box::new(DummyTooltip));
        make_event_ctx(f, s, t, &|_| {})
    }

    fn value_action(value: f32) -> Action {
        Action::Custom {
            namespace: "test.slider".into(),
            name: format!("change:{value:.1}"),
            payload: Default::default(),
        }
    }

    #[test]
    fn slider_new_value_clamped() {
        let s = Slider::new(150.0, 0.0, 100.0);
        assert_eq!(s.value(), 100.0);
    }

    #[test]
    fn slider_new_value_in_range() {
        let s = Slider::new(50.0, 0.0, 100.0);
        assert_eq!(s.value(), 50.0);
    }

    #[test]
    fn slider_mouse_down_starts_drag_and_updates_value() {
        let mut s = Slider::new(0.0, 0.0, 100.0);
        s.layout(Rect::new(0.0, 0.0, 200.0, 20.0));

        let mut ctx = event_ctx();
        // Click at x=100 (50%)
        s.event(
            &UiEvent::MouseDown {
                position: Point::new(100.0, 10.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(s.value(), 50.0);
    }

    #[test]
    fn slider_mouse_down_dispatches_change_and_requests_repaint_when_value_changes() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut f = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut shortcut, &mut tooltip, &dispatch);
        let mut s = Slider::new(0.0, 0.0, 100.0).on_change(value_action);
        s.layout(Rect::new(0.0, 0.0, 200.0, 20.0));

        s.event(
            &UiEvent::MouseDown {
                position: Point::new(100.0, 10.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(actions.borrow().as_slice(), &[value_action(50.0)]);
        assert!(ctx.requests.repaint);
    }

    #[test]
    fn slider_mouse_down_at_same_value_does_not_dispatch_or_repaint() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut f = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut shortcut, &mut tooltip, &dispatch);
        let mut s = Slider::new(0.0, 0.0, 100.0).on_change(value_action);
        s.layout(Rect::new(0.0, 0.0, 200.0, 20.0));

        s.event(
            &UiEvent::MouseDown {
                position: Point::new(7.0, 10.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert!(actions.borrow().is_empty());
        assert!(!ctx.requests.repaint);
    }

    #[test]
    fn slider_drag_uses_thumb_center_track_range() {
        let mut s = Slider::new(0.0, 0.0, 100.0);
        s.layout(Rect::new(0.0, 0.0, 200.0, 20.0));

        let mut ctx = event_ctx();
        s.event(
            &UiEvent::MouseDown {
                position: Point::new(7.0, 10.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(s.value(), 0.0);

        s.event(
            &UiEvent::MouseMove {
                position: Point::new(193.0, 10.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(s.value(), 100.0);
    }

    #[test]
    fn slider_mouse_move_updates_value_during_drag() {
        let mut s = Slider::new(0.0, 0.0, 100.0);
        s.layout(Rect::new(0.0, 0.0, 200.0, 20.0));

        let mut ctx = event_ctx();
        s.event(
            &UiEvent::MouseDown {
                position: Point::new(0.0, 10.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        let track = s.track_rect();
        let x_75 = track.x + track.width * 0.75;
        s.event(
            &UiEvent::MouseMove {
                position: Point::new(x_75, 10.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(s.value(), 75.0);
    }

    #[test]
    fn slider_mouse_up_stops_drag() {
        let mut s = Slider::new(0.0, 0.0, 100.0);
        s.layout(Rect::new(0.0, 0.0, 200.0, 20.0));

        let mut ctx = event_ctx();
        s.event(
            &UiEvent::MouseDown {
                position: Point::new(0.0, 10.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        s.event(
            &UiEvent::MouseUp {
                position: Point::new(0.0, 10.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        // After MouseUp, MouseMove should NOT update value
        s.event(
            &UiEvent::MouseMove {
                position: Point::new(200.0, 10.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(s.value(), 0.0); // unchanged from MouseDown at x=0
    }

    #[test]
    fn slider_keyboard_ignores_without_focus() {
        let mut s = Slider::new(50.0, 0.0, 100.0);
        let mut ctx = event_ctx();

        let result = s.event(
            &UiEvent::KeyDown { key: KeyCode::Right, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Ignored);
        assert_eq!(s.value(), 50.0);
    }

    #[test]
    fn slider_arrow_keys_adjust_when_focused() {
        let mut s = Slider::new(50.0, 0.0, 100.0);
        let mut ctx = event_ctx();

        s.event(&UiEvent::FocusGained, &mut ctx);
        s.event(
            &UiEvent::KeyDown { key: KeyCode::Right, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(s.value(), 51.0);

        s.event(
            &UiEvent::KeyDown { key: KeyCode::Left, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(s.value(), 50.0);
    }

    #[test]
    fn slider_keyboard_shift_uses_large_step() {
        let mut s = Slider::new(50.0, 0.0, 100.0);
        let mut ctx = event_ctx();

        s.event(&UiEvent::FocusGained, &mut ctx);
        s.event(
            &UiEvent::KeyDown { key: KeyCode::Up, modifiers: Modifiers::shift() },
            &mut ctx,
        );

        assert_eq!(s.value(), 60.0);
    }

    #[test]
    fn slider_keyboard_dispatches_once_per_changed_nudge() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut f = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut shortcut, &mut tooltip, &dispatch);
        let mut s = Slider::new(50.0, 0.0, 100.0).on_change(value_action);

        s.event(&UiEvent::FocusGained, &mut ctx);
        s.event(
            &UiEvent::KeyDown { key: KeyCode::Right, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert_eq!(actions.borrow().as_slice(), &[value_action(51.0)]);
        assert!(ctx.requests.repaint);
    }

    #[test]
    fn slider_keyboard_at_boundary_does_not_dispatch_duplicate_change() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut f = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut shortcut, &mut tooltip, &dispatch);
        let mut s = Slider::new(0.0, 0.0, 100.0).on_change(value_action);

        s.event(&UiEvent::FocusGained, &mut ctx);
        let result = s.event(
            &UiEvent::KeyDown { key: KeyCode::Home, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Ignored);
        assert!(actions.borrow().is_empty());
        assert!(!ctx.requests.repaint);
    }

    #[test]
    fn slider_home_end_jump_to_bounds() {
        let mut s = Slider::new(50.0, 0.0, 100.0);
        let mut ctx = event_ctx();

        s.event(&UiEvent::FocusGained, &mut ctx);
        s.event(
            &UiEvent::KeyDown { key: KeyCode::Home, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(s.value(), 0.0);

        s.event(
            &UiEvent::KeyDown { key: KeyCode::End, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(s.value(), 100.0);
    }

    #[test]
    fn slider_value_clamped_at_bounds() {
        let mut s = Slider::new(0.0, 0.0, 100.0);
        s.layout(Rect::new(0.0, 0.0, 100.0, 20.0));

        let mut ctx = event_ctx();
        // Click at leftmost edge → value = min
        s.event(
            &UiEvent::MouseDown {
                position: Point::new(0.0, 10.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(s.value(), 0.0);
        // Click at rightmost edge → value = max
        s.event(
            &UiEvent::MouseMove {
                position: Point::new(101.0, 10.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(s.value(), 100.0); // clamped to max via update_value ratio clamp
    }

    #[test]
    fn slider_thumb_rect_stays_inside_bounds() {
        let mut s = Slider::new(50.0, 0.0, 100.0);
        s.layout(Rect::new(10.0, 20.0, 160.0, 18.0));

        let thumb = s.thumb_rect();

        assert!(thumb.x >= s.bounds.x);
        assert!(thumb.y >= s.bounds.y);
        assert!(thumb.x + thumb.width <= s.bounds.x + s.bounds.width);
        assert!(thumb.y + thumb.height <= s.bounds.y + s.bounds.height);
    }

    #[test]
    fn slider_paint_shrinks_thumb_for_short_bounds() {
        let mut s = Slider::new(50.0, 0.0, 100.0);
        s.layout(Rect::new(0.0, 0.0, 100.0, 10.0));
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 100.0, 10.0),
        };

        s.paint(&mut ctx);

        let thumb = encoder.rects.last().expect("paint should draw a thumb");
        assert!(thumb.y >= s.bounds.y);
        assert!(thumb.y + thumb.height <= s.bounds.y + s.bounds.height);
    }
}
