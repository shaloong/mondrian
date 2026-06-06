//! 绘制命令 —— UI 渲染的中间表示
//!
//! [`DrawCommand`] 是平台无关的 2D 绘制原语。
//! [`DrawEncoder`] 收集这些命令，然后由 [`UiRenderer`](crate::UiRenderer) 提交到 GPU。

use glam::Vec2;
use mondrian_core::Color;
use mondrian_ui_core::types::{Point, Rect};
use mondrian_ui_core::widget::DrawCommandEncoder;
use mondrian_ui_theme::typography::TextStyle;

/// 2D 绘制命令
///
/// 每个命令描述一个 GPU 可执行的绘制操作。
/// 命令序列由 DrawEncoder 收集，由 UiRenderer 批次化后提交。
#[derive(Debug, Clone)]
pub enum DrawCommand {
    /// 填充矩形（可带圆角）
    Rect {
        bounds: Rect,
        color: Color,
        corner_radius: f32,
    },

    /// 文字
    Text {
        text: String,
        style: TextStyle,
        position: Point,
        color: Color,
    },

    /// 图像（从纹理图集采样）
    Image {
        bounds: Rect,
        uv_rect: Rect,
        tint: Color,
    },

    /// 线段
    Line {
        start: Point,
        end: Point,
        width: f32,
        color: Color,
    },

    /// 裁剪区域（后续命令在裁剪区域内绘制）
    PushClip {
        bounds: Rect,
    },

    /// 弹出最近的裁剪区域
    PopClip,

    /// 平移变换
    PushTranslate {
        offset: Vec2,
    },

    /// 弹出最近的平移变换
    PopTransform,
}

/// 绘制命令收集器
///
/// 在 Widget::paint() 中使用，将绘制命令追加到内部缓冲区。
/// 支持裁剪栈和变换栈。
#[derive(Debug, Clone, Default)]
pub struct DrawEncoder {
    commands: Vec<DrawCommand>,
    clip_depth: u32,
    transform_depth: u32,
}

impl DrawEncoder {
    pub fn new() -> Self {
        Self {
            commands: Vec::new(),
            clip_depth: 0,
            transform_depth: 0,
        }
    }

    pub fn push_clip(&mut self, bounds: Rect) {
        self.clip_depth += 1;
        self.commands.push(DrawCommand::PushClip { bounds });
    }

    pub fn pop_clip(&mut self) {
        if self.clip_depth > 0 {
            self.clip_depth -= 1;
            self.commands.push(DrawCommand::PopClip);
        }
    }

    pub fn push_translate(&mut self, offset: Vec2) {
        self.transform_depth += 1;
        self.commands.push(DrawCommand::PushTranslate { offset });
    }

    pub fn pop_transform(&mut self) {
        if self.transform_depth > 0 {
            self.transform_depth -= 1;
            self.commands.push(DrawCommand::PopTransform);
        }
    }

    pub fn draw_rect(&mut self, bounds: Rect, color: Color, corner_radius: f32) {
        self.commands.push(DrawCommand::Rect {
            bounds,
            color,
            corner_radius,
        });
    }

    pub fn draw_text(&mut self, text: &str, style: &TextStyle, position: Point, color: Color) {
        self.commands.push(DrawCommand::Text {
            text: text.to_string(),
            style: style.clone(),
            position,
            color,
        });
    }

    pub fn draw_image(&mut self, bounds: Rect, uv_rect: Rect, tint: Color) {
        self.commands
            .push(DrawCommand::Image { bounds, uv_rect, tint });
    }

    pub fn draw_line(&mut self, start: Point, end: Point, width: f32, color: Color) {
        self.commands.push(DrawCommand::Line {
            start,
            end,
            width,
            color,
        });
    }

    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    pub fn command_count(&self) -> usize {
        self.commands.len()
    }

    /// 消耗编码器，返回收集到的命令列表
    pub fn finish(self) -> Vec<DrawCommand> {
        assert_eq!(
            self.clip_depth, 0,
            "DrawEncoder: unbalanced clip push/pop"
        );
        assert_eq!(
            self.transform_depth, 0,
            "DrawEncoder: unbalanced transform push/pop"
        );
        self.commands
    }
}

// ═══════════════════════════════════════════════════════════════════════════════════
// DrawCommandEncoder trait impl — bridges the circular dep between ui-core and ui-renderer
// ═══════════════════════════════════════════════════════════════════════════════════

impl DrawCommandEncoder for DrawEncoder {
    fn push_clip(&mut self, bounds: Rect) {
        self.push_clip(bounds);
    }

    fn pop_clip(&mut self) {
        self.pop_clip();
    }

    fn draw_rect(&mut self, bounds: Rect, color: Color, corner_radius: f32) {
        self.draw_rect(bounds, color, corner_radius);
    }

    fn draw_line(&mut self, start: Point, end: Point, width: f32, color: Color) {
        self.draw_line(start, end, width, color);
    }

    fn push_translate(&mut self, offset: Vec2) {
        self.push_translate(offset);
    }

    fn pop_transform(&mut self) {
        self.pop_transform();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ═══════════════════════════════════════════════════════════════════════
    // Helpers
    // ═══════════════════════════════════════════════════════════════════════

    fn rect() -> Rect {
        Rect::new(0.0, 0.0, 100.0, 50.0)
    }

    fn color() -> Color {
        Color::WHITE
    }

    // ═══════════════════════════════════════════════════════════════════════
    // DrawEncoder collection
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn encoder_new_is_empty() {
        let enc = DrawEncoder::new();
        assert!(enc.is_empty());
        assert_eq!(enc.command_count(), 0);
    }

    #[test]
    fn encoder_draw_rect_adds_command() {
        let mut enc = DrawEncoder::new();
        enc.draw_rect(rect(), color(), 4.0);
        assert_eq!(enc.command_count(), 1);
        assert!(!enc.is_empty());
    }

    #[test]
    fn encoder_multiple_draws() {
        let mut enc = DrawEncoder::new();
        enc.draw_rect(rect(), color(), 0.0);
        enc.draw_rect(rect(), Color::BLACK, 0.0);
        enc.draw_rect(rect(), Color::TRANSPARENT, 8.0);
        assert_eq!(enc.command_count(), 3);
    }

    #[test]
    fn encoder_draw_line() {
        let mut enc = DrawEncoder::new();
        enc.draw_line(Point::new(0.0, 0.0), Point::new(100.0, 100.0), 2.0, color());
        assert_eq!(enc.command_count(), 1);
    }

    #[test]
    fn encoder_draw_text() {
        let mut enc = DrawEncoder::new();
        let style = TextStyle {
            font_size: 14.0,
            line_height: 20.0,
            font_weight: mondrian_ui_theme::typography::FontWeight::Regular,
            letter_spacing: 0.0,
        };
        enc.draw_text("hello", &style, Point::ZERO, color());
        assert_eq!(enc.command_count(), 1);
    }

    #[test]
    fn encoder_draw_image() {
        let mut enc = DrawEncoder::new();
        enc.draw_image(rect(), rect(), color());
        assert_eq!(enc.command_count(), 1);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Clip push/pop
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn encoder_clip_push_pop_balanced() {
        let mut enc = DrawEncoder::new();
        enc.push_clip(rect());
        enc.draw_rect(rect(), color(), 0.0);
        enc.pop_clip();
        let cmds = enc.finish();
        assert_eq!(cmds.len(), 3); // PushClip, Rect, PopClip
    }

    #[test]
    fn encoder_nested_clips() {
        let mut enc = DrawEncoder::new();
        enc.push_clip(rect());
        enc.draw_rect(rect(), color(), 0.0);
        enc.push_clip(rect());
        enc.draw_rect(rect(), color(), 0.0);
        enc.pop_clip();
        enc.pop_clip();
        let cmds = enc.finish();
        assert_eq!(cmds.len(), 6);
    }

    #[test]
    #[should_panic(expected = "unbalanced clip push/pop")]
    fn encoder_finish_panics_on_unbalanced_clip() {
        let mut enc = DrawEncoder::new();
        enc.push_clip(rect());
        // No matching pop
        enc.finish();
    }

    #[test]
    fn encoder_extra_pop_clip_is_silently_ignored() {
        let mut enc = DrawEncoder::new();
        enc.pop_clip(); // unbalanced pop — silently ignored
        let cmds = enc.finish();
        assert!(cmds.is_empty());
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Transform push/pop
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn encoder_transform_push_pop_balanced() {
        let mut enc = DrawEncoder::new();
        enc.push_translate(Vec2::new(10.0, 20.0));
        enc.draw_rect(rect(), color(), 0.0);
        enc.pop_transform();
        let cmds = enc.finish();
        assert_eq!(cmds.len(), 3);
    }

    #[test]
    #[should_panic(expected = "unbalanced transform push/pop")]
    fn encoder_finish_panics_on_unbalanced_transform() {
        let mut enc = DrawEncoder::new();
        enc.push_translate(Vec2::new(1.0, 0.0));
        enc.finish();
    }

    #[test]
    fn encoder_default_is_empty() {
        let enc = DrawEncoder::default();
        assert!(enc.is_empty());
    }

    // ═══════════════════════════════════════════════════════════════════════
    // DrawCommandEncoder trait impl
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn draw_encoder_implements_trait() {
        let mut enc = DrawEncoder::new();
        // Use the trait methods
        <DrawEncoder as DrawCommandEncoder>::draw_rect(&mut enc, rect(), color(), 4.0);
        <DrawEncoder as DrawCommandEncoder>::push_clip(&mut enc, rect());
        <DrawEncoder as DrawCommandEncoder>::pop_clip(&mut enc);
        <DrawEncoder as DrawCommandEncoder>::draw_line(
            &mut enc,
            Point::ZERO,
            Point::new(10.0, 10.0),
            1.0,
            color(),
        );
        <DrawEncoder as DrawCommandEncoder>::push_translate(&mut enc, Vec2::ZERO);
        <DrawEncoder as DrawCommandEncoder>::pop_transform(&mut enc);
        let cmds = enc.finish();
        assert_eq!(cmds.len(), 6);
    }

    #[test]
    fn draw_encoder_as_trait_object() {
        let mut enc = DrawEncoder::new();
        let dyn_enc: &mut dyn DrawCommandEncoder = &mut enc;
        dyn_enc.draw_rect(rect(), color(), 0.0);
        assert_eq!(enc.command_count(), 1);
    }
}
