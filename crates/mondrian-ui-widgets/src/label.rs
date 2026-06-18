//! Label 控件
//!
//! 纯文本显示组件。不响应交互。

use glam::Vec2;
use mondrian_core::Color;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};

use crate::text_metrics::{measure_single_line, measure_text_box};

/// Semantic color source for a [`Label`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LabelColor {
    /// Theme foreground text.
    Foreground,
    /// Theme muted foreground text.
    Muted,
    /// Theme popover foreground text.
    PopoverForeground,
    /// Theme primary text/accent color.
    Primary,
    /// Theme destructive text color.
    Destructive,
    /// Explicit color supplied by the caller.
    Explicit(Color),
}

/// Label Widget —— 纯文本显示
pub struct Label {
    id: WidgetId,
    text: String,
    bounds: Rect,
    color: LabelColor,
    font_size: f32,
    padding: Vec2,
    max_width: Option<f32>,
    wrap: bool,
}

impl Label {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            id: WidgetId::new(),
            text: text.into(),
            bounds: Rect::ZERO,
            color: LabelColor::Foreground,
            font_size: 14.0,
            padding: Vec2::new(4.0, 2.0),
            max_width: None,
            wrap: false,
        }
    }

    pub fn with_color(mut self, color: Color) -> Self {
        self.color = LabelColor::Explicit(color);
        self
    }

    /// Use a semantic theme color.
    pub fn with_semantic_color(mut self, color: LabelColor) -> Self {
        self.color = color;
        self
    }

    pub fn with_font_size(mut self, size: f32) -> Self {
        self.font_size = size.max(1.0);
        self
    }

    /// Set label text inset within its layout bounds.
    pub fn with_padding(mut self, x: f32, y: f32) -> Self {
        self.padding = Vec2::new(x.max(0.0), y.max(0.0));
        self
    }

    /// Constrain text width and render using wrapped text commands.
    pub fn with_max_width(mut self, width: f32) -> Self {
        self.max_width = Some(width.max(1.0));
        self.wrap = true;
        self
    }

    /// Enable wrapped text using the laid-out content width.
    pub fn wrapped(mut self) -> Self {
        self.wrap = true;
        self
    }

    /// Shorthand for muted supporting text.
    pub fn muted(self) -> Self {
        self.with_semantic_color(LabelColor::Muted)
    }

    /// Shorthand for popover foreground text.
    pub fn popover_foreground(self) -> Self {
        self.with_semantic_color(LabelColor::PopoverForeground)
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn set_text(&mut self, text: String) {
        self.text = text;
    }

    fn resolved_color(&self, ctx: &PaintContext) -> Color {
        match self.color {
            LabelColor::Foreground => ctx.theme.colors.foreground,
            LabelColor::Muted => ctx.theme.colors.muted_foreground,
            LabelColor::PopoverForeground => ctx.theme.colors.popover_foreground,
            LabelColor::Primary => ctx.theme.colors.primary,
            LabelColor::Destructive => ctx.theme.colors.destructive_foreground,
            LabelColor::Explicit(color) => color,
        }
    }
}

impl Widget for Label {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        let horizontal_padding = self.padding.x * 2.0;
        let vertical_padding = self.padding.y * 2.0;
        let constrained_text_width = if constraint.max.width.is_finite() {
            (constraint.max.width - horizontal_padding).max(1.0)
        } else {
            f32::MAX
        };
        let content_width = self
            .max_width
            .unwrap_or(constrained_text_width)
            .min(constrained_text_width)
            .max(1.0);
        let (measured_text_width, measured_text_height) = if self.wrap {
            measure_text_box(&self.text, self.font_size, content_width)
        } else {
            measure_single_line(&self.text, self.font_size)
        };
        let fallback_height = self.font_size * 1.4;
        let height = measured_text_height.max(fallback_height) + vertical_padding;
        constraint.constrain(Size::new(
            measured_text_width.min(content_width) + horizontal_padding,
            height,
        ))
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
    }

    fn event(&mut self, _event: &UiEvent, _ctx: &mut EventContext) -> EventResult {
        EventResult::Ignored
    }

    fn paint(&self, ctx: &mut PaintContext) {
        if !self.text.is_empty() {
            let content = Rect::new(
                self.bounds.x + self.padding.x,
                self.bounds.y + self.padding.y,
                (self.bounds.width - self.padding.x * 2.0).max(1.0),
                (self.bounds.height - self.padding.y * 2.0).max(1.0),
            );
            let position = Point::new(
                self.bounds.x + self.padding.x,
                self.bounds.y + self.padding.y,
            );
            let color = self.resolved_color(ctx);
            ctx.push_clip(content);
            if self.wrap {
                let content_width = (self.bounds.width - self.padding.x * 2.0).max(1.0);
                let max_width = self.max_width.unwrap_or(content_width).min(content_width);
                ctx.encoder
                    .draw_text_box(&self.text, self.font_size, position, max_width, color);
            } else {
                ctx.encoder.draw_text(&self.text, self.font_size, position, color);
            }
            ctx.pop_clip();
        }
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_ui_core::widget::DrawCommandEncoder;
    use mondrian_ui_theme::ThemePreset;

    #[derive(Debug, PartialEq)]
    enum TextCommand {
        Text {
            text: String,
            font_size: f32,
            position: Point,
            color: Color,
        },
        TextBox {
            text: String,
            font_size: f32,
            position: Point,
            max_width: f32,
            color: Color,
        },
    }

    #[derive(Default)]
    struct Recorder {
        commands: Vec<TextCommand>,
        clips: Vec<Rect>,
        clip_pops: usize,
    }

    impl DrawCommandEncoder for Recorder {
        fn push_clip(&mut self, bounds: Rect) {
            self.clips.push(bounds);
        }

        fn pop_clip(&mut self) {
            self.clip_pops += 1;
        }

        fn draw_rect(&mut self, _bounds: Rect, _color: Color, _corner_radius: f32) {}

        fn draw_line(&mut self, _start: Point, _end: Point, _width: f32, _color: Color) {}

        fn draw_text(&mut self, text: &str, font_size: f32, position: Point, color: Color) {
            self.commands
                .push(TextCommand::Text { text: text.into(), font_size, position, color });
        }

        fn draw_text_box(
            &mut self,
            text: &str,
            font_size: f32,
            position: Point,
            max_width: f32,
            color: Color,
        ) {
            self.commands.push(TextCommand::TextBox {
                text: text.into(),
                font_size,
                position,
                max_width,
                color,
            });
        }

        fn push_translate(&mut self, _offset: glam::Vec2) {}

        fn pop_transform(&mut self) {}
    }

    fn paint_label(label: &Label, theme: &mondrian_ui_theme::Theme) -> Vec<TextCommand> {
        let mut recorder = Recorder::default();
        let mut ctx = PaintContext {
            encoder: &mut recorder,
            theme,
            clip_rect: Rect::new(0.0, 0.0, 200.0, 100.0),
        };
        label.paint(&mut ctx);
        recorder.commands
    }

    fn paint_label_recording(label: &Label, theme: &mondrian_ui_theme::Theme) -> Recorder {
        paint_label_recording_clipped(label, theme, Rect::new(0.0, 0.0, 200.0, 100.0))
    }

    fn paint_label_recording_clipped(
        label: &Label,
        theme: &mondrian_ui_theme::Theme,
        clip_rect: Rect,
    ) -> Recorder {
        let mut recorder = Recorder::default();
        let mut ctx = PaintContext { encoder: &mut recorder, theme, clip_rect };
        label.paint(&mut ctx);
        recorder
    }

    #[test]
    fn default_label_uses_theme_foreground() {
        let theme = ThemePreset::Dark.build();
        let mut label = Label::new("Title").with_padding(0.0, 0.0);
        label.layout(Rect::new(10.0, 20.0, 100.0, 24.0));

        let commands = paint_label(&label, &theme);

        assert_eq!(
            commands,
            vec![TextCommand::Text {
                text: "Title".into(),
                font_size: 14.0,
                position: Point::new(10.0, 20.0),
                color: theme.colors.foreground,
            }]
        );
    }

    #[test]
    fn explicit_label_color_overrides_theme() {
        let theme = ThemePreset::Light.build();
        let color = Color::from_hex(0xFF00FF);
        let mut label = Label::new("Accent").with_color(color).with_padding(1.0, 2.0);
        label.layout(Rect::new(4.0, 6.0, 100.0, 24.0));

        let commands = paint_label(&label, &theme);

        assert_eq!(
            commands,
            vec![TextCommand::Text {
                text: "Accent".into(),
                font_size: 14.0,
                position: Point::new(5.0, 8.0),
                color,
            }]
        );
    }

    #[test]
    fn wrapped_label_uses_text_box() {
        let theme = ThemePreset::Dark.build();
        let mut label = Label::new("A longer supporting sentence")
            .muted()
            .with_font_size(12.0)
            .with_padding(0.0, 0.0)
            .with_max_width(80.0);
        label.layout(Rect::new(0.0, 0.0, 120.0, 48.0));

        let commands = paint_label(&label, &theme);

        assert_eq!(
            commands,
            vec![TextCommand::TextBox {
                text: "A longer supporting sentence".into(),
                font_size: 12.0,
                position: Point::new(0.0, 0.0),
                max_width: 80.0,
                color: theme.colors.muted_foreground,
            }]
        );
    }

    #[test]
    fn label_paint_clips_text_to_content_bounds() {
        let theme = ThemePreset::Dark.build();
        let mut label = Label::new("A very long label that cannot fit").with_padding(3.0, 2.0);
        label.layout(Rect::new(10.0, 20.0, 40.0, 18.0));

        let recorder = paint_label_recording(&label, &theme);

        assert_eq!(
            recorder.commands,
            vec![TextCommand::Text {
                text: "A very long label that cannot fit".into(),
                font_size: 14.0,
                position: Point::new(13.0, 22.0),
                color: theme.colors.foreground,
            }]
        );
        assert_eq!(recorder.clips, vec![Rect::new(13.0, 22.0, 34.0, 14.0)]);
        assert_eq!(recorder.clip_pops, 1);
    }

    #[test]
    fn label_text_clip_is_narrowed_by_root_paint_clip() {
        let theme = ThemePreset::Dark.build();
        let mut label = Label::new("Root clipped text").with_padding(0.0, 0.0);
        label.layout(Rect::new(10.0, 20.0, 100.0, 24.0));

        let recorder =
            paint_label_recording_clipped(&label, &theme, Rect::new(30.0, 25.0, 50.0, 10.0));

        assert_eq!(recorder.clips, vec![Rect::new(30.0, 25.0, 50.0, 10.0)]);
        assert_eq!(recorder.clip_pops, 1);
    }

    #[test]
    fn wrapped_label_measure_respects_max_width() {
        let label = Label::new("A long label value")
            .with_font_size(10.0)
            .with_padding(2.0, 1.0)
            .with_max_width(30.0);

        let measured = label.measure(LayoutConstraint::LOOSE);

        assert!(measured.width <= 34.0);
        assert!(measured.width > 4.0);
        assert!(measured.height > 16.0);
    }

    #[test]
    fn wrapped_label_paints_with_actual_content_width_when_narrower_than_max_width() {
        let theme = ThemePreset::Dark.build();
        let mut label = Label::new("A longer supporting sentence")
            .with_padding(2.0, 0.0)
            .with_max_width(120.0);
        label.layout(Rect::new(10.0, 20.0, 64.0, 40.0));

        let commands = paint_label(&label, &theme);

        assert_eq!(
            commands,
            vec![TextCommand::TextBox {
                text: "A longer supporting sentence".into(),
                font_size: 14.0,
                position: Point::new(12.0, 20.0),
                max_width: 60.0,
                color: theme.colors.foreground,
            }]
        );
    }

    #[test]
    fn wrapped_label_without_max_width_reflows_to_layout_content_width() {
        let theme = ThemePreset::Dark.build();
        let mut label = Label::new("Scroll text should reflow with the viewport")
            .with_padding(5.0, 0.0)
            .wrapped();
        let measured =
            label.measure(LayoutConstraint { min: Size::ZERO, max: Size::new(72.0, f32::MAX) });
        label.layout(Rect::new(10.0, 20.0, 72.0, measured.height));

        let commands = paint_label(&label, &theme);

        assert!(measured.width <= 72.0);
        assert!(
            measured.height > 14.0 * 1.3,
            "narrow auto-wrapped label should increase height"
        );
        assert_eq!(
            commands,
            vec![TextCommand::TextBox {
                text: "Scroll text should reflow with the viewport".into(),
                font_size: 14.0,
                position: Point::new(15.0, 20.0),
                max_width: 62.0,
                color: theme.colors.foreground,
            }]
        );
    }

    #[test]
    fn cjk_wrapped_label_measure_uses_exact_text_layout() {
        let label = Label::new("很长的中文标签需要换行")
            .with_font_size(13.0)
            .with_padding(2.0, 1.0)
            .with_max_width(42.0);

        let measured = label.measure(LayoutConstraint::LOOSE);

        assert!(measured.width <= 46.0);
        assert!(measured.height > 13.0 * 1.3 + 2.0);
    }
}
