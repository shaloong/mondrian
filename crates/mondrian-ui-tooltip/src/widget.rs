//! Tooltip 弹出框 Widget
//!
//! 在指定锚点附近绘制带背景的文字提示。

use mondrian_ui_core::tooltip::TooltipState;
use mondrian_ui_core::types::{estimate_text_width, *};
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};

const DEFAULT_MAX_WIDTH: f32 = 320.0;
const DEFAULT_OFFSET: f32 = 8.0;
const HORIZONTAL_PADDING: f32 = 8.0;
const VERTICAL_PADDING: f32 = 5.0;

/// Tooltip 弹出框 Widget
///
/// 当 `TooltipState::visible` 时绘制圆角背景 + 文字。
/// 位置由 TooltipState 指定。
pub struct TooltipWidget {
    id: WidgetId,
    state: TooltipState,
}

impl TooltipWidget {
    pub fn new() -> Self {
        Self {
            id: WidgetId::new(),
            state: TooltipState {
                text: String::new(),
                position: Point::ZERO,
                visible: false,
            },
        }
    }

    pub fn update_state(&mut self, state: TooltipState) {
        self.state = state;
    }

    pub fn clear(&mut self) {
        self.state.visible = false;
    }

    fn estimated_size(&self, font_size: f32, max_width: f32) -> Size {
        if !self.state.visible || self.state.text.is_empty() {
            return Size::ZERO;
        }
        let width = (estimate_text_width(&self.state.text, font_size) + HORIZONTAL_PADDING * 2.0)
            .max(40.0)
            .min(max_width);
        let height = (font_size * 1.3 + VERTICAL_PADDING * 2.0).max(24.0);
        Size::new(width, height)
    }

    fn raw_rect(&self, font_size: f32, max_width: f32, offset: f32) -> Rect {
        let size = self.estimated_size(font_size, max_width);
        Rect::new(
            self.state.position.x + offset,
            self.state.position.y + offset,
            size.width,
            size.height,
        )
    }

    fn clamped_rect(&self, font_size: f32, max_width: f32, offset: f32, clip: Rect) -> Rect {
        let raw = self.raw_rect(font_size, max_width, offset);
        let x = raw.x.clamp(clip.x, (clip.x + clip.width - raw.width).max(clip.x));
        let y = raw.y.clamp(clip.y, (clip.y + clip.height - raw.height).max(clip.y));
        Rect::new(x, y, raw.width.min(clip.width), raw.height.min(clip.height))
    }
}

impl Widget for TooltipWidget {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, _constraint: LayoutConstraint) -> Size {
        self.estimated_size(14.0, DEFAULT_MAX_WIDTH)
    }

    fn layout(&mut self, _bounds: Rect) {}

    fn event(&mut self, _event: &UiEvent, _ctx: &mut EventContext) -> EventResult {
        EventResult::Ignored
    }

    fn paint(&self, ctx: &mut PaintContext) {
        if !self.state.visible || self.state.text.is_empty() {
            return;
        }

        let tokens = &ctx.theme.colors;
        let spacing = &ctx.theme.spacing;

        let font_size = ctx.theme.typography.body.font_size;
        let bg = self.clamped_rect(
            font_size,
            spacing.tooltip_max_width,
            spacing.tooltip_offset,
            ctx.clip_rect,
        );
        let border = tokens.ring;

        // Simple border by drawing a slightly larger rect behind the fill.
        let border_rect = bg.inset(-1.0, -1.0);
        ctx.encoder.draw_rect(border_rect, border, spacing.radius_sm);
        ctx.encoder.draw_rect(bg, tokens.popover, spacing.radius_sm);

        ctx.encoder.draw_text(
            &self.state.text,
            font_size,
            Point::new(bg.x + HORIZONTAL_PADDING, bg.y + VERTICAL_PADDING),
            tokens.popover_foreground,
        );
    }

    fn hit_test(&self, point: Point) -> bool {
        if !self.state.visible {
            return false;
        }
        self.raw_rect(14.0, DEFAULT_MAX_WIDTH, DEFAULT_OFFSET).contains(point)
    }
}

impl Default for TooltipWidget {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_ui_core::widget::DrawCommandEncoder;
    use mondrian_ui_theme::ThemePreset;

    #[derive(Default)]
    struct RecordingEncoder {
        rects: Vec<Rect>,
        texts: Vec<(String, Point)>,
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
            text: &str,
            _font_size: f32,
            position: Point,
            _color: mondrian_core::Color,
        ) {
            self.texts.push((text.into(), position));
        }

        fn push_translate(&mut self, _offset: glam::Vec2) {}

        fn pop_transform(&mut self) {}
    }

    #[test]
    fn hidden_tooltip_measures_zero() {
        let widget = TooltipWidget::new();
        assert_eq!(widget.measure(LayoutConstraint::LOOSE), Size::ZERO);
    }

    #[test]
    fn hit_test_uses_tooltip_position_not_layout_bounds() {
        let mut widget = TooltipWidget::new();
        widget.layout(Rect::new(0.0, 0.0, 1.0, 1.0));
        widget.update_state(TooltipState {
            text: "hello".into(),
            position: Point::new(100.0, 50.0),
            visible: true,
        });

        assert!(widget.hit_test(Point::new(112.0, 62.0)));
        assert!(!widget.hit_test(Point::new(0.5, 0.5)));
    }

    #[test]
    fn paint_clamps_tooltip_inside_clip_rect() {
        let mut widget = TooltipWidget::new();
        widget.update_state(TooltipState {
            text: "hello".into(),
            position: Point::new(190.0, 90.0),
            visible: true,
        });
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let clip_rect = Rect::new(0.0, 0.0, 220.0, 120.0);

        {
            let mut ctx = PaintContext { encoder: &mut encoder, theme: &theme, clip_rect };
            widget.paint(&mut ctx);
        }

        let fill = encoder.rects.get(1).expect("paint should draw border and fill");
        assert!(fill.x + fill.width <= clip_rect.x + clip_rect.width + 0.1);
        assert!(fill.y + fill.height <= clip_rect.y + clip_rect.height + 0.1);
        assert_eq!(encoder.texts.len(), 1);
    }
}
