//! Slider 控件
//!
//! 拖拽滑块，用于数值调节。

mod model;

use model::{SliderGeometry, SliderModel};
use mondrian_editor_state::Action;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{
    AccessibilityNode, AccessibilityRole, AccessibilityState, AccessibilityValue, EventContext,
    PaintContext,
};
use mondrian_ui_core::{EventResult, UiEvent, Widget};

/// Adapter that maps the current slider value to an editor [`Action`].
pub type SliderChangeAction = dyn Fn(f32) -> Action;

/// Slider Widget —— 拖拽滑块控制数值
pub struct Slider {
    id: WidgetId,
    model: SliderModel,
    bounds: Rect,
    enabled: bool,
    dragging: bool,
    focused: bool,
    focus_visible: bool,
    geometry: SliderGeometry,
    on_change: Option<Box<SliderChangeAction>>,
}

impl Slider {
    /// Create a slider with a clamped initial value.
    pub fn new(value: f32, min: f32, max: f32) -> Self {
        Self {
            id: WidgetId::new(),
            model: SliderModel::new(value, min, max),
            bounds: Rect::ZERO,
            enabled: true,
            dragging: false,
            focused: false,
            focus_visible: false,
            geometry: SliderGeometry::new(4.0, 12.0),
            on_change: None,
        }
    }

    /// Return the current value.
    pub fn value(&self) -> f32 {
        self.model.value()
    }

    /// Dispatch an action whenever user input changes the value.
    pub fn on_change(mut self, action: impl Fn(f32) -> Action + 'static) -> Self {
        self.on_change = Some(Box::new(action));
        self
    }

    /// Quantize user-edited values to a fixed step.
    ///
    /// The current value is left unchanged when this builder is called, so
    /// snapshots from app state stay exact. Pointer and keyboard edits are
    /// snapped to `min + n * step` and clamped to the slider bounds.
    pub fn with_step(mut self, step: f32) -> Self {
        self.model.set_step(step);
        self
    }

    /// Set whether the slider accepts user input and participates in focus.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        if !enabled {
            self.dragging = false;
            self.focused = false;
            self.focus_visible = false;
        }
        self
    }

    /// Disable the slider.
    pub fn disabled(self) -> Self {
        self.enabled(false)
    }

    /// Whether the slider is enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    fn range(&self) -> f32 {
        self.model.range()
    }

    fn ratio(&self) -> f32 {
        self.model.ratio()
    }

    fn track_rect(&self) -> Rect {
        self.geometry.track_rect(self.bounds)
    }

    fn thumb_rect(&self) -> Rect {
        self.geometry.thumb_rect(self.bounds, self.ratio())
    }

    fn set_value(&mut self, value: f32) -> bool {
        self.model.set_value(value)
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
            (ctx.dispatch)(action(self.value()));
        }
    }

    fn keyboard_step(&self, modifiers: Modifiers) -> f32 {
        self.model.keyboard_step(modifiers)
    }

    fn page_step(&self) -> f32 {
        self.model.page_step()
    }

    fn nudge(&mut self, delta: f32, ctx: &mut EventContext) -> EventResult {
        if self.range() <= 0.0 {
            return EventResult::Ignored;
        }
        if self.set_value_from_input(self.value() + delta, ctx) {
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
        constraint.constrain(self.geometry.measure_size())
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        if !self.enabled {
            if self.dragging {
                ctx.release_pointer_capture(self.id);
            }
            self.dragging = false;
            self.focused = false;
            self.focus_visible = false;
            return EventResult::Ignored;
        }
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
                let was_dragging = self.dragging;
                self.focused = false;
                self.focus_visible = false;
                self.dragging = false;
                if was_dragging {
                    ctx.release_pointer_capture(self.id);
                }
                EventResult::Handled
            }
            UiEvent::KeyDown { key, modifiers } if self.focused => {
                if modifiers.ctrl || modifiers.alt || modifiers.meta {
                    return EventResult::Ignored;
                }
                let step = self.keyboard_step(*modifiers);
                match key {
                    KeyCode::Left | KeyCode::Down => self.nudge(-step, ctx),
                    KeyCode::Right | KeyCode::Up => self.nudge(step, ctx),
                    KeyCode::PageDown => self.nudge(-self.page_step(), ctx),
                    KeyCode::PageUp => self.nudge(self.page_step(), ctx),
                    KeyCode::Home => {
                        if self.set_value_from_input(self.model.min(), ctx) {
                            EventResult::Handled
                        } else {
                            EventResult::Ignored
                        }
                    }
                    KeyCode::End => {
                        if self.set_value_from_input(self.model.max(), ctx) {
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
        let track_color = if self.enabled {
            tokens.surface_2
        } else {
            tokens.muted
        };
        let fill_color = if self.enabled {
            tokens.foreground
        } else {
            tokens.muted_foreground
        };
        ctx.encoder.draw_rect(track, track_color, spacing.radius_full);

        // Filled track
        let ratio = self.ratio();
        let fill_w = track.width * ratio;
        if fill_w > 0.0 {
            let track_fill = Rect::new(track.x, track.y, fill_w, track.height);
            ctx.encoder.draw_rect(track_fill, fill_color, spacing.radius_full);
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
        ctx.encoder.draw_rect(thumb_rect, fill_color, thumb_rect.height * 0.5);
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn can_focus(&self) -> bool {
        self.enabled
    }

    fn accessibility(&self) -> Option<AccessibilityNode> {
        Some(
            AccessibilityNode::new(self.id, AccessibilityRole::Slider)
                .with_name("Slider")
                .with_state(AccessibilityState {
                    focusable: self.enabled,
                    focused: self.focused,
                    disabled: !self.enabled,
                    ..AccessibilityState::default()
                })
                .with_value(AccessibilityValue::Number {
                    value: self.value(),
                    min: self.model.min(),
                    max: self.model.max(),
                }),
        )
    }
}

impl Slider {
    fn update_value(&mut self, position: &Point, ctx: &mut EventContext) {
        let range = self.range();
        if range <= 0.0 {
            return;
        }
        let ratio = self.geometry.ratio_at_position(self.bounds, *position);
        self.set_value_from_input(self.model.value_at_ratio(ratio), ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{make_event_ctx, DummyFocus, DummyShortcut, DummyTooltip};
    use mondrian_ui_core::widget::{DrawCommandEncoder, PointerCaptureRequest};
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

    fn assert_close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() <= 0.0001,
            "expected {expected}, got {actual}"
        );
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
    fn slider_new_orders_inverted_range() {
        let s = Slider::new(50.0, 100.0, 0.0);

        assert_eq!(s.value(), 50.0);
    }

    #[test]
    fn slider_new_recovers_non_finite_range_and_value() {
        let s = Slider::new(f32::NAN, f32::NAN, f32::INFINITY);

        assert_eq!(s.value(), 0.0);
    }

    #[test]
    fn slider_with_step_does_not_quantize_initial_snapshot() {
        let s = Slider::new(0.23, 0.0, 1.0).with_step(0.1);

        assert_close(s.value(), 0.23);
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
    fn slider_mouse_input_quantizes_to_step() {
        let mut s = Slider::new(0.0, 0.0, 100.0).with_step(25.0);
        s.layout(Rect::new(0.0, 0.0, 200.0, 20.0));
        let track = s.track_rect();
        let mut ctx = event_ctx();

        s.event(
            &UiEvent::MouseDown {
                position: Point::new(track.x + track.width * 0.62, 10.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_close(s.value(), 50.0);
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
    fn disabled_slider_ignores_mouse_and_focus() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut f = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut shortcut, &mut tooltip, &dispatch);
        let mut slider = Slider::new(0.0, 0.0, 100.0).on_change(value_action).disabled();
        slider.layout(Rect::new(0.0, 0.0, 200.0, 20.0));

        let result = slider.event(
            &UiEvent::MouseDown {
                position: Point::new(100.0, 10.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Ignored);
        assert_eq!(slider.value(), 0.0);
        assert!(!slider.can_focus());
        assert!(actions.borrow().is_empty());
        assert!(!ctx.requests.repaint);
    }

    #[test]
    fn slider_accessibility_exposes_numeric_range() {
        let slider = Slider::new(25.0, 0.0, 100.0);

        let node = slider.accessibility().expect("slider should expose accessibility");

        assert_eq!(node.role, AccessibilityRole::Slider);
        assert_eq!(node.name.as_deref(), Some("Slider"));
        assert!(node.state.focusable);
        assert_eq!(
            node.value,
            Some(AccessibilityValue::Number { value: 25.0, min: 0.0, max: 100.0 })
        );
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
        let track = s.track_rect();

        s.event(
            &UiEvent::MouseDown {
                position: Point::new(track.x, track.center().y),
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
        let track = s.track_rect();

        let mut ctx = event_ctx();
        s.event(
            &UiEvent::MouseDown {
                position: Point::new(track.x, track.center().y),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(s.value(), 0.0);

        s.event(
            &UiEvent::MouseMove {
                position: Point::new(track.x + track.width, track.center().y),
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
    fn slider_focus_lost_without_drag_does_not_release_pointer_capture() {
        let mut s = Slider::new(0.0, 0.0, 100.0);
        let mut ctx = event_ctx();

        assert_eq!(
            s.event(&UiEvent::FocusGained, &mut ctx),
            EventResult::Handled
        );
        assert_eq!(s.event(&UiEvent::FocusLost, &mut ctx), EventResult::Handled);

        assert_eq!(ctx.requests.pointer_capture, None);
    }

    #[test]
    fn slider_focus_lost_during_drag_releases_pointer_capture() {
        let mut s = Slider::new(0.0, 0.0, 100.0);
        s.layout(Rect::new(0.0, 0.0, 200.0, 20.0));
        let mut ctx = event_ctx();

        s.event(
            &UiEvent::MouseDown {
                position: Point::new(100.0, 10.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        ctx.requests.pointer_capture = None;

        assert_eq!(s.event(&UiEvent::FocusLost, &mut ctx), EventResult::Handled);

        assert_eq!(
            ctx.requests.pointer_capture,
            Some(PointerCaptureRequest::Release(s.id()))
        );
        assert!(!s.dragging);
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
    fn slider_keyboard_uses_configured_step() {
        let mut s = Slider::new(0.2, 0.0, 1.0).with_step(0.25);
        let mut ctx = event_ctx();

        s.event(&UiEvent::FocusGained, &mut ctx);
        s.event(
            &UiEvent::KeyDown { key: KeyCode::Right, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_close(s.value(), 0.5);

        s.event(
            &UiEvent::KeyDown { key: KeyCode::Left, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_close(s.value(), 0.25);
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
    fn slider_keyboard_shift_scales_configured_step() {
        let mut s = Slider::new(10.0, 0.0, 100.0).with_step(2.0);
        let mut ctx = event_ctx();

        s.event(&UiEvent::FocusGained, &mut ctx);
        s.event(
            &UiEvent::KeyDown { key: KeyCode::Up, modifiers: Modifiers::shift() },
            &mut ctx,
        );

        assert_close(s.value(), 30.0);
    }

    #[test]
    fn slider_keyboard_ignores_ctrl_alt_and_meta_chords() {
        let mut s = Slider::new(50.0, 0.0, 100.0);
        let mut ctx = event_ctx();

        s.event(&UiEvent::FocusGained, &mut ctx);
        for (key, modifiers) in [
            (KeyCode::Right, Modifiers::ctrl()),
            (
                KeyCode::Right,
                Modifiers { alt: true, ..Default::default() },
            ),
            (
                KeyCode::Home,
                Modifiers { meta: true, ..Default::default() },
            ),
            (
                KeyCode::End,
                Modifiers { alt: true, shift: true, ..Default::default() },
            ),
            (
                KeyCode::PageUp,
                Modifiers { ctrl: true, shift: true, ..Default::default() },
            ),
        ] {
            assert_eq!(
                s.event(&UiEvent::KeyDown { key, modifiers }, &mut ctx),
                EventResult::Ignored
            );
            assert_close(s.value(), 50.0);
        }
    }

    #[test]
    fn slider_invalid_step_keeps_default_keyboard_step() {
        let mut s = Slider::new(50.0, 0.0, 100.0).with_step(0.0);
        let mut ctx = event_ctx();

        s.event(&UiEvent::FocusGained, &mut ctx);
        s.event(
            &UiEvent::KeyDown { key: KeyCode::Right, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert_eq!(s.value(), 51.0);
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
