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
