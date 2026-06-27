//! Tooltip 弹出框 Widget
//!
//! 在指定锚点附近绘制带背景的文字提示。

use mondrian_ui_core::tooltip::TooltipState;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};
use mondrian_ui_text::TextRenderer;
use std::cell::RefCell;

const DEFAULT_MAX_WIDTH: f32 = 320.0;
const DEFAULT_OFFSET: f32 = 8.0;
const HORIZONTAL_PADDING: f32 = 8.0;
const VERTICAL_PADDING: f32 = 5.0;
const MIN_WIDTH: f32 = 40.0;

#[derive(Debug, Clone, PartialEq)]
struct TooltipLayout {
    size: Size,
    content_width: f32,
}

thread_local! {
    static TEXT_MEASURER: RefCell<TextRenderer> = RefCell::new(TextRenderer::new());
}

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

    fn text_layout(&self, font_size: f32, max_width: f32) -> TooltipLayout {
        if !self.state.visible || self.state.text.is_empty() {
            return TooltipLayout { size: Size::ZERO, content_width: 0.0 };
        }

        let content_max_width = (max_width - HORIZONTAL_PADDING * 2.0).max(1.0);
        let (measured_w, measured_h) =
            measure_text_box(&self.state.text, font_size, content_max_width);
        let content_width = measured_w.min(content_max_width);
        let width = (content_width + HORIZONTAL_PADDING * 2.0).max(MIN_WIDTH).min(max_width);
        let height = (measured_h + VERTICAL_PADDING * 2.0).max(24.0);

        TooltipLayout { size: Size::new(width, height), content_width }
    }

    fn estimated_size(&self, font_size: f32, max_width: f32) -> Size {
        self.text_layout(font_size, max_width).size
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

    fn paint(&self, _ctx: &mut PaintContext) {}

    fn paint_overlay(&self, ctx: &mut PaintContext) {
        if !self.state.visible || self.state.text.is_empty() {
            return;
        }
        if !rect_has_paintable_area(ctx.clip_rect) {
            return;
        }

        let tokens = &ctx.theme.colors;
        let spacing = &ctx.theme.spacing;

        let font_size = ctx.theme.typography.small.font_size;
        let max_width = spacing.tooltip_max_width.min(ctx.clip_rect.width.max(1.0));
        let layout = self.text_layout(font_size, max_width);
        let bg = self.clamped_rect(font_size, max_width, spacing.tooltip_offset, ctx.clip_rect);

        // Shadow
        ctx.encoder.draw_soft_shadow(
            bg,
            mondrian_core::Color { r: 0.0, g: 0.0, b: 0.0, a: 0.35 },
            spacing.radius_md,
            0.0,
            0.0,
            glam::Vec2::new(0.0, spacing.xs),
        );
        // Border (clamped to clip for edge cases)
        let border_rect = clamp_rect_to_clip(bg.inset(-1.0, -1.0), ctx.clip_rect);
        ctx.encoder.draw_rect(border_rect, tokens.border, spacing.radius_md + 1.0);
        // Background
        ctx.encoder.draw_rect(bg, tokens.popover, spacing.radius_md);

        ctx.push_clip(bg);
        ctx.encoder.draw_text_box(
            &self.state.text,
            font_size,
            Point::new(bg.x + HORIZONTAL_PADDING, bg.y + VERTICAL_PADDING),
            layout.content_width.max(1.0),
            tokens.popover_foreground,
        );
        ctx.encoder.pop_clip();
    }

    fn hit_test(&self, point: Point) -> bool {
        if !self.state.visible {
            return false;
        }
        self.raw_rect(14.0, DEFAULT_MAX_WIDTH, DEFAULT_OFFSET).contains(point)
    }
}

fn measure_text_box(text: &str, font_size: f32, max_width: f32) -> (f32, f32) {
    TEXT_MEASURER.with_borrow_mut(|renderer| renderer.measure_text_box(text, font_size, max_width))
}

fn clamp_rect_to_clip(rect: Rect, clip: Rect) -> Rect {
    let left = rect.x.max(clip.x);
    let top = rect.y.max(clip.y);
    let right = (rect.x + rect.width).min(clip.x + clip.width);
    let bottom = (rect.y + rect.height).min(clip.y + clip.height);
    Rect::new(left, top, (right - left).max(0.0), (bottom - top).max(0.0))
}

fn rect_has_paintable_area(rect: Rect) -> bool {
    rect.x.is_finite()
        && rect.y.is_finite()
        && rect.width.is_finite()
        && rect.height.is_finite()
        && rect.width > 0.0
        && rect.height > 0.0
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
        clips: Vec<Rect>,
        texts: Vec<(String, Point)>,
        text_boxes: Vec<(String, Point, f32)>,
    }

    impl DrawCommandEncoder for RecordingEncoder {
        fn push_clip(&mut self, bounds: Rect) {
            self.clips.push(bounds);
        }

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

        fn draw_text_box(
            &mut self,
            text: &str,
            _font_size: f32,
            position: Point,
            max_width: f32,
            _color: mondrian_core::Color,
        ) {
            self.text_boxes.push((text.into(), position, max_width));
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
            widget.paint_overlay(&mut ctx);
        }

        let fill = encoder.rects.get(2).expect("paint should draw shadow, border and fill");
        assert!(fill.x + fill.width <= clip_rect.x + clip_rect.width + 0.1);
        assert!(fill.y + fill.height <= clip_rect.y + clip_rect.height + 0.1);
        assert_eq!(encoder.rects.len(), 3);
        assert_eq!(encoder.text_boxes.len(), 1);
    }

    #[test]
    fn paint_clamps_tooltip_border_inside_clip_rect() {
        let mut widget = TooltipWidget::new();
        widget.update_state(TooltipState {
            text: "edge tooltip".into(),
            position: Point::new(218.0, 118.0),
            visible: true,
        });
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let clip_rect = Rect::new(0.0, 0.0, 220.0, 120.0);

        {
            let mut ctx = PaintContext { encoder: &mut encoder, theme: &theme, clip_rect };
            widget.paint_overlay(&mut ctx);
        }

        let border = encoder.rects.get(1).expect("paint should draw border at index 1");
        assert!(border.x >= clip_rect.x);
        assert!(border.y >= clip_rect.y);
        assert!(border.x + border.width <= clip_rect.x + clip_rect.width + 0.1);
        assert!(border.y + border.height <= clip_rect.y + clip_rect.height + 0.1);
    }

    #[test]
    fn paint_skips_tooltip_when_clip_has_no_area() {
        let mut widget = TooltipWidget::new();
        widget.update_state(TooltipState {
            text: "hidden by empty viewport".into(),
            position: Point::new(10.0, 10.0),
            visible: true,
        });
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();

        {
            let mut ctx = PaintContext {
                encoder: &mut encoder,
                theme: &theme,
                clip_rect: Rect::new(0.0, 0.0, 0.0, 120.0),
            };
            widget.paint_overlay(&mut ctx);
        }

        assert!(encoder.rects.is_empty());
        assert!(encoder.clips.is_empty());
        assert!(encoder.text_boxes.is_empty());
    }

    #[test]
    fn paint_skips_tooltip_when_clip_is_not_finite() {
        let mut widget = TooltipWidget::new();
        widget.update_state(TooltipState {
            text: "hidden by invalid viewport".into(),
            position: Point::new(10.0, 10.0),
            visible: true,
        });
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();

        {
            let mut ctx = PaintContext {
                encoder: &mut encoder,
                theme: &theme,
                clip_rect: Rect::new(0.0, 0.0, f32::INFINITY, 120.0),
            };
            widget.paint_overlay(&mut ctx);
        }

        assert!(encoder.rects.is_empty());
        assert!(encoder.clips.is_empty());
        assert!(encoder.text_boxes.is_empty());
    }

    #[test]
    fn long_tooltip_text_uses_cosmic_text_box_measurement() {
        let mut widget = TooltipWidget::new();
        widget.update_state(TooltipState {
            text: "ThisIsAReallyLongTooltipTokenWithoutSpacesThatMustWrapInsideThePopover".into(),
            position: Point::new(10.0, 10.0),
            visible: true,
        });

        let font_size = 14.0;
        let max_width = 160.0;
        let layout = widget.text_layout(font_size, max_width);
        let single_line = measure_text_box(&widget.state.text, font_size, f32::MAX / 4.0);

        assert!(layout.size.width <= max_width);
        assert!(layout.content_width <= max_width - HORIZONTAL_PADDING * 2.0 + 0.1);
        assert!(layout.size.height > single_line.1 + VERTICAL_PADDING * 2.0);
    }

    #[test]
    fn cjk_tooltip_text_uses_cosmic_text_box_measurement() {
        let mut widget = TooltipWidget::new();
        widget.update_state(TooltipState {
            text: "这是一个很长很长的中文提示文本它需要在没有空格的时候自动换行".into(),
            position: Point::new(10.0, 10.0),
            visible: true,
        });

        let font_size = 14.0;
        let max_width = 150.0;
        let layout = widget.text_layout(font_size, max_width);

        assert!(layout.size.width <= max_width);
        assert!(layout.content_width <= max_width - HORIZONTAL_PADDING * 2.0 + 0.1);
        assert!(layout.size.height > font_size * 1.3 + VERTICAL_PADDING * 2.0);
    }

    #[test]
    fn paint_draws_wrapped_text_box_command() {
        let mut widget = TooltipWidget::new();
        widget.update_state(TooltipState {
            text: "Tooltip text that should wrap into more than one line at this width".into(),
            position: Point::new(10.0, 10.0),
            visible: true,
        });
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let clip_rect = Rect::new(0.0, 0.0, 180.0, 200.0);

        {
            let mut ctx = PaintContext { encoder: &mut encoder, theme: &theme, clip_rect };
            widget.paint_overlay(&mut ctx);
        }

        assert!(encoder.texts.is_empty());
        assert_eq!(encoder.text_boxes.len(), 1);
        assert!(encoder.text_boxes[0].2 <= clip_rect.width - HORIZONTAL_PADDING * 2.0 + 0.1);
    }

    #[test]
    fn paint_wraps_to_available_clip_width() {
        let mut widget = TooltipWidget::new();
        widget.update_state(TooltipState {
            text: "Tooltip text that must wrap to the narrow viewport width".into(),
            position: Point::new(0.0, 0.0),
            visible: true,
        });
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let clip_rect = Rect::new(0.0, 0.0, 120.0, 200.0);
        {
            let mut ctx = PaintContext { encoder: &mut encoder, theme: &theme, clip_rect };
            widget.paint_overlay(&mut ctx);
        }

        let fill = encoder.rects.get(2).expect("paint should draw shadow, border and fill");
        assert!(fill.width <= clip_rect.width);
        assert_eq!(encoder.text_boxes.len(), 1);
        assert!(encoder.text_boxes[0].2 <= clip_rect.width - HORIZONTAL_PADDING * 2.0 + 0.1);
    }

    #[test]
    fn paint_text_box_width_matches_clamped_fill_content_width() {
        let mut widget = TooltipWidget::new();
        widget.update_state(TooltipState {
            text: "A tooltip near the right edge should keep text inside the popover fill".into(),
            position: Point::new(174.0, 20.0),
            visible: true,
        });
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let clip_rect = Rect::new(0.0, 0.0, 220.0, 180.0);
        {
            let mut ctx = PaintContext { encoder: &mut encoder, theme: &theme, clip_rect };
            widget.paint_overlay(&mut ctx);
        }

        let fill = encoder.rects.get(2).expect("paint should draw shadow, border and fill");
        let clip = encoder.clips.first().expect("text should be clipped to the fill rect");
        let (_, text_position, text_width) =
            encoder.text_boxes.first().expect("tooltip should draw a text box");
        assert_eq!(*clip, *fill);
        assert!(text_position.x >= fill.x + HORIZONTAL_PADDING - 0.1);
        assert!(text_position.y >= fill.y + VERTICAL_PADDING - 0.1);
        assert!(
            text_position.x + text_width <= fill.x + fill.width - HORIZONTAL_PADDING + 0.1,
            "tooltip text box must not extend beyond the clamped fill rect"
        );
    }

    #[test]
    fn visible_tooltip_paints_only_in_overlay_layer() {
        let mut widget = TooltipWidget::new();
        widget.update_state(TooltipState {
            text: "Tooltip should be a top-layer popover".into(),
            position: Point::new(10.0, 10.0),
            visible: true,
        });
        let theme = ThemePreset::Dark.build();
        let clip_rect = Rect::new(0.0, 0.0, 180.0, 200.0);

        let mut normal_encoder = RecordingEncoder::default();
        {
            let mut ctx = PaintContext {
                encoder: &mut normal_encoder,
                theme: &theme,
                clip_rect,
            };
            widget.paint(&mut ctx);
        }
        assert!(normal_encoder.rects.is_empty());
        assert!(normal_encoder.text_boxes.is_empty());

        let mut overlay_encoder = RecordingEncoder::default();
        {
            let mut ctx = PaintContext {
                encoder: &mut overlay_encoder,
                theme: &theme,
                clip_rect,
            };
            widget.paint_overlay(&mut ctx);
        }
        assert_eq!(overlay_encoder.rects.len(), 3);
        assert_eq!(overlay_encoder.text_boxes.len(), 1);
    }

    #[test]
    fn component_visual_regression_scenario_keeps_tooltip_as_clipped_top_layer_popover() {
        let mut widget = TooltipWidget::new();
        widget.update_state(TooltipState {
            text: "Tooltip visual regression text wraps near the viewport edge".into(),
            position: Point::new(156.0, 84.0),
            visible: true,
        });
        let theme = ThemePreset::Dark.build();
        let clip_rect = Rect::new(0.0, 0.0, 220.0, 140.0);

        let mut normal_encoder = RecordingEncoder::default();
        {
            let mut ctx = PaintContext {
                encoder: &mut normal_encoder,
                theme: &theme,
                clip_rect,
            };
            widget.paint(&mut ctx);
        }
        assert!(
            normal_encoder.rects.is_empty() && normal_encoder.text_boxes.is_empty(),
            "tooltip visual chrome should be emitted only from the overlay pass"
        );

        let mut overlay_encoder = RecordingEncoder::default();
        {
            let mut ctx = PaintContext {
                encoder: &mut overlay_encoder,
                theme: &theme,
                clip_rect,
            };
            widget.paint_overlay(&mut ctx);
        }

        assert_eq!(
            overlay_encoder.rects.len(),
            3,
            "tooltip should paint shadow, border and fill"
        );
        let fill = overlay_encoder.rects[2];
        assert!(fill.x >= clip_rect.x);
        assert!(fill.y >= clip_rect.y);
        assert!(fill.x + fill.width <= clip_rect.x + clip_rect.width + 0.1);
        assert!(fill.y + fill.height <= clip_rect.y + clip_rect.height + 0.1);
        let text_clip = overlay_encoder
            .clips
            .first()
            .copied()
            .expect("tooltip text should be clipped to the visible popover fill");
        assert!((text_clip.x - fill.x).abs() <= 0.01);
        assert!((text_clip.y - fill.y).abs() <= 0.01);
        assert!((text_clip.width - fill.width).abs() <= 0.01);
        assert!((text_clip.height - fill.height).abs() <= 0.01);
        let (text, position, max_width) = overlay_encoder
            .text_boxes
            .first()
            .expect("visible tooltip should emit wrapped text");
        assert!(text.contains("Tooltip visual regression"));
        assert!(position.x >= fill.x + HORIZONTAL_PADDING - 0.1);
        assert!(position.y >= fill.y + VERTICAL_PADDING - 0.1);
        assert!(
            position.x + max_width <= fill.x + fill.width - HORIZONTAL_PADDING + 0.1,
            "tooltip wrapped text width should remain inside the clamped popover"
        );
    }
}
