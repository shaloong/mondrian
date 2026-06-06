//! Slider 控件
//!
//! 拖拽滑块，用于数值调节。

use mondrian_core::Color;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{DrawCommandEncoder, EventContext, PaintContext};
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

    fn measure(&self, _constraint: LayoutConstraint) -> Size {
        Size::new(100.0, self.thumb_size + 4.0)
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
    }

    fn event(&mut self, event: &UiEvent, _ctx: &mut EventContext) -> EventResult {
        match event {
            UiEvent::MouseDown { position, button: MouseButton::Left, .. } if self.bounds.contains(*position) => {
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

        // Track background
        let track_bg = Rect::new(self.bounds.x, track_y, self.bounds.width, self.track_height);
        ctx.encoder.draw_rect(track_bg, tokens.bg_surface_hover, spacing.radius_sm);

        // Filled track
        let ratio = (self.value - self.min) / (self.max - self.min);
        let fill_w = self.bounds.width * ratio;
        if fill_w > 0.0 {
            let track_fill = Rect::new(self.bounds.x, track_y, fill_w, self.track_height);
            ctx.encoder.draw_rect(track_fill, tokens.interaction_highlight, spacing.radius_sm);
        }

        // Thumb
        let thumb_x = self.bounds.x + fill_w - self.thumb_size * 0.5;
        let thumb_x = thumb_x.clamp(self.bounds.x, self.bounds.x + self.bounds.width - self.thumb_size);
        let thumb_rect = Rect::new(
            thumb_x,
            self.bounds.y + (self.bounds.height - self.thumb_size) * 0.5,
            self.thumb_size,
            self.thumb_size,
        );
        ctx.encoder.draw_rect(thumb_rect, tokens.interaction_highlight, spacing.radius_full);
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }
}

impl Slider {
    fn update_value(&mut self, position: &Point) {
        let ratio = ((position.x - self.bounds.x) / self.bounds.width).clamp(0.0, 1.0);
        self.value = self.min + (self.max - self.min) * ratio;
    }
}
