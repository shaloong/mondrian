//! Slider 控件
//!
//! 拖拽滑块，用于数值调节。

use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};

/// Slider Widget —— 拖拽滑块控制数值
pub struct Slider {
    id: WidgetId,
    value: f32,
    min: f32,
    max: f32,
    bounds: Rect,
    dragging: bool,
    track_height: f32,
    thumb_size: f32,
}

impl Slider {
    pub fn new(value: f32, min: f32, max: f32) -> Self {
        Self {
            id: WidgetId::new(),
            value: value.clamp(min, max),
            min,
            max,
            bounds: Rect::ZERO,
            dragging: false,
            track_height: 6.0,
            thumb_size: 14.0,
        }
    }

    pub fn value(&self) -> f32 {
        self.value
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

    fn event(&mut self, event: &UiEvent, _ctx: &mut EventContext) -> EventResult {
        match event {
            UiEvent::MouseDown { position, button: MouseButton::Left, .. }
                if self.bounds.contains(*position) =>
            {
                self.dragging = true;
                self.update_value(position);
                EventResult::Handled
            }
            UiEvent::MouseMove { position, .. } if self.dragging => {
                self.update_value(position);
                EventResult::Handled
            }
            UiEvent::MouseUp { button: MouseButton::Left, .. } if self.dragging => {
                self.dragging = false;
                EventResult::Handled
            }
            _ => EventResult::Ignored,
        }
    }

    fn paint(&self, ctx: &mut PaintContext) {
        let tokens = &ctx.theme.colors;
        let spacing = &ctx.theme.spacing;

        let track_y = self.bounds.y + self.bounds.height * 0.5 - self.track_height * 0.5;
        let range = self.max - self.min;

        // Track background
        let track_bg = Rect::new(self.bounds.x, track_y, self.bounds.width, self.track_height);
        ctx.encoder.draw_rect(track_bg, tokens.accent, spacing.radius_sm);

        // Filled track (guard against zero range)
        let ratio = if range > 0.0 {
            ((self.value - self.min) / range).clamp(0.0, 1.0)
        } else {
            0.5
        };
        let fill_w = self.bounds.width * ratio;
        if fill_w > 0.0 {
            let track_fill = Rect::new(self.bounds.x, track_y, fill_w, self.track_height);
            ctx.encoder.draw_rect(track_fill, tokens.primary, spacing.radius_sm);
        }

        // Thumb
        let thumb_x = self.bounds.x + fill_w - self.thumb_size * 0.5;
        let thumb_x = thumb_x.clamp(
            self.bounds.x,
            self.bounds.x + self.bounds.width - self.thumb_size,
        );
        let thumb_rect = Rect::new(
            thumb_x,
            self.bounds.y + (self.bounds.height - self.thumb_size) * 0.5,
            self.thumb_size,
            self.thumb_size,
        );
        ctx.encoder.draw_rect(thumb_rect, tokens.primary, spacing.radius_full);
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }
}

impl Slider {
    fn update_value(&mut self, position: &Point) {
        let range = self.max - self.min;
        if range <= 0.0 {
            return;
        }
        let ratio = ((position.x - self.bounds.x) / self.bounds.width).clamp(0.0, 1.0);
        self.value = self.min + range * ratio;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{make_event_ctx, DummyFocus, DummyShortcut, DummyTooltip};

    fn event_ctx() -> EventContext<'static> {
        let f: &'static mut DummyFocus = Box::leak(Box::new(DummyFocus));
        let s: &'static mut DummyShortcut = Box::leak(Box::new(DummyShortcut));
        let t: &'static mut DummyTooltip = Box::leak(Box::new(DummyTooltip));
        make_event_ctx(f, s, t, &|_| {})
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
        s.event(
            &UiEvent::MouseMove {
                position: Point::new(150.0, 10.0),
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
}
