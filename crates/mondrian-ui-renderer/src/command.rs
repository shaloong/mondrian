//! 绘制命令 —— UI 渲染的中间表示
//!
//! [`DrawCommand`] 是平台无关的 2D 绘制原语。
//! [`DrawEncoder`] 收集这些命令，然后由 [`UiRenderer`](crate::UiRenderer) 提交到 GPU。

use glam::Vec2;
use mondrian_core::Color;
use mondrian_ui_core::types::{Point, Rect};
use mondrian_ui_core::widget::DrawCommandEncoder;
use mondrian_ui_theme::typography::TextStyle;
use std::sync::Arc;

fn floor_if_finite(value: f32) -> f32 {
    if value.is_finite() {
        value.floor()
    } else {
        value
    }
}

fn ceil_if_finite(value: f32) -> f32 {
    if value.is_finite() {
        value.ceil()
    } else {
        value
    }
}

fn conservative_clip_rect(rect: Rect) -> Rect {
    let left = floor_if_finite(rect.x);
    let top = floor_if_finite(rect.y);
    let right = ceil_if_finite(rect.x + rect.width);
    let bottom = ceil_if_finite(rect.y + rect.height);
    Rect::new(left, top, (right - left).max(0.0), (bottom - top).max(0.0))
}

pub(crate) fn raster_image_payload_len(width: u32, height: u32) -> Option<usize> {
    width.checked_mul(height)?.checked_mul(4).map(|len| len as usize)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShapeMask {
    pub bounds: Rect,
    pub corner_radius: f32,
}

/// 2D 绘制命令
///
/// 每个命令描述一个 GPU 可执行的绘制操作。
/// 命令序列由 DrawEncoder 收集，由 UiRenderer 批次化后提交。
#[derive(Debug, Clone)]
pub enum DrawCommand {
    /// 填充矩形（可带圆角）。corner_radius in pixels; shader clamps automatically.
    Rect {
        bounds: Rect,
        color: Color,
        corner_radius: f32, // px; 0 = sharp
    },

    /// Analytic soft shadow cast by a rounded rectangle.
    ///
    /// `bounds` describes the caster before offset/spread expansion.
    SoftShadow {
        bounds: Rect,
        color: Color,
        corner_radius: f32,
        blur_radius: f32,
        spread: f32,
        offset: Vec2,
    },

    /// GPU-interpolated rectangle gradient.
    ///
    /// Color order is top-left, top-right, bottom-left, bottom-right.
    GradientRect {
        bounds: Rect,
        colors: [Color; 4],
        corner_radius: f32,
    },

    /// 文字
    Text {
        text: String,
        style: TextStyle,
        position: Point,
        /// Maximum paragraph width in pixels. `None` keeps single-line layout.
        max_width: Option<f32>,
        color: Color,
    },

    /// 图像（从纹理图集采样）
    Image {
        bounds: Rect,
        uv_rect: Rect,
        tint: Color,
    },

    /// RGBA image data to cache in the renderer-owned image atlas.
    RasterImage {
        key: String,
        bounds: Rect,
        width: u32,
        height: u32,
        rgba: Arc<[u8]>,
        tint: Color,
    },

    /// Resolved renderer image-atlas draw. This is produced internally from
    /// [`DrawCommand::RasterImage`] after atlas allocation.
    RasterAtlasImage {
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

    /// 填充三角形列表。每三个点构成一个独立三角形。
    Triangles { vertices: Vec<Point>, color: Color },

    /// Per-vertex colored triangle list. Every three vertices form one triangle.
    ///
    /// `mask` lets non-rectangular UI like color wheels reuse the same SDF
    /// antialiasing path as rounded rectangles and circles.
    ColoredTriangles {
        vertices: Vec<(Point, Color)>,
        mask: Option<ShapeMask>,
    },

    /// 裁剪区域（后续命令在裁剪区域内绘制）
    PushClip { bounds: Rect },

    /// 弹出最近的裁剪区域
    PopClip,

    /// 平移变换
    PushTranslate { offset: Vec2 },

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
        self.commands
            .push(DrawCommand::PushClip { bounds: conservative_clip_rect(bounds) });
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
        self.commands.push(DrawCommand::Rect { bounds, color, corner_radius });
    }

    pub fn draw_soft_shadow(
        &mut self,
        bounds: Rect,
        color: Color,
        corner_radius: f32,
        blur_radius: f32,
        spread: f32,
        offset: Vec2,
    ) {
        self.commands.push(DrawCommand::SoftShadow {
            bounds,
            color,
            corner_radius,
            blur_radius,
            spread,
            offset,
        });
    }

    /// Record a GPU-interpolated gradient rectangle.
    ///
    /// Color order is top-left, top-right, bottom-left, bottom-right.
    pub fn draw_gradient_rect(&mut self, bounds: Rect, colors: [Color; 4], corner_radius: f32) {
        self.commands.push(DrawCommand::GradientRect { bounds, colors, corner_radius });
    }

    pub fn draw_text(&mut self, text: &str, style: &TextStyle, position: Point, color: Color) {
        self.commands.push(DrawCommand::Text {
            text: text.to_string(),
            style: style.clone(),
            position,
            max_width: None,
            color,
        });
    }

    /// Record a wrapped text command constrained to `max_width` pixels.
    pub fn draw_text_box(
        &mut self,
        text: &str,
        style: &TextStyle,
        position: Point,
        max_width: f32,
        color: Color,
    ) {
        self.commands.push(DrawCommand::Text {
            text: text.to_string(),
            style: style.clone(),
            position,
            max_width: Some(max_width.max(1.0)),
            color,
        });
    }

    pub fn draw_image(&mut self, bounds: Rect, uv_rect: Rect, tint: Color) {
        self.commands.push(DrawCommand::Image { bounds, uv_rect, tint });
    }

    /// Record an RGBA image that the renderer should upload to its image atlas.
    pub fn draw_raster_image(
        &mut self,
        key: &str,
        bounds: Rect,
        width: u32,
        height: u32,
        rgba: Arc<[u8]>,
        tint: Color,
    ) {
        let Some(expected_len) = raster_image_payload_len(width, height) else {
            return;
        };
        if width == 0 || height == 0 || rgba.len() != expected_len {
            return;
        }

        self.commands.push(DrawCommand::RasterImage {
            key: key.to_string(),
            bounds,
            width,
            height,
            rgba,
            tint,
        });
    }

    pub fn draw_line(&mut self, start: Point, end: Point, width: f32, color: Color) {
        self.commands.push(DrawCommand::Line { start, end, width, color });
    }

    pub fn draw_triangles(&mut self, vertices: &[Point], color: Color) {
        let triangle_vertex_count = vertices.len() - vertices.len() % 3;
        if triangle_vertex_count == 0 {
            return;
        }

        let vertices = vertices.iter().take(triangle_vertex_count).copied().collect();
        self.commands.push(DrawCommand::Triangles { vertices, color });
    }

    pub fn draw_colored_triangles(&mut self, vertices: &[(Point, Color)]) {
        let triangle_vertex_count = vertices.len() - vertices.len() % 3;
        if triangle_vertex_count == 0 {
            return;
        }

        let vertices = vertices.iter().take(triangle_vertex_count).copied().collect();
        self.commands.push(DrawCommand::ColoredTriangles { vertices, mask: None });
    }

    pub fn draw_colored_triangles_in_rect(
        &mut self,
        vertices: &[(Point, Color)],
        mask_bounds: Rect,
        corner_radius: f32,
    ) {
        let triangle_vertex_count = vertices.len() - vertices.len() % 3;
        if triangle_vertex_count == 0 {
            return;
        }

        let vertices = vertices.iter().take(triangle_vertex_count).copied().collect();
        self.commands.push(DrawCommand::ColoredTriangles {
            vertices,
            mask: Some(ShapeMask { bounds: mask_bounds, corner_radius }),
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
        assert_eq!(self.clip_depth, 0, "DrawEncoder: unbalanced clip push/pop");
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

    fn draw_soft_shadow(
        &mut self,
        bounds: Rect,
        color: Color,
        corner_radius: f32,
        blur_radius: f32,
        spread: f32,
        offset: Vec2,
    ) {
        self.draw_soft_shadow(bounds, color, corner_radius, blur_radius, spread, offset);
    }

    fn draw_gradient_rect(&mut self, bounds: Rect, colors: [Color; 4], corner_radius: f32) {
        self.draw_gradient_rect(bounds, colors, corner_radius);
    }

    fn draw_line(&mut self, start: Point, end: Point, width: f32, color: Color) {
        self.draw_line(start, end, width, color);
    }

    fn draw_triangles(&mut self, vertices: &[Point], color: Color) {
        self.draw_triangles(vertices, color);
    }

    fn draw_colored_triangles(&mut self, vertices: &[(Point, Color)]) {
        self.draw_colored_triangles(vertices);
    }

    fn draw_colored_triangles_in_rect(
        &mut self,
        vertices: &[(Point, Color)],
        mask_bounds: Rect,
        corner_radius: f32,
    ) {
        self.draw_colored_triangles_in_rect(vertices, mask_bounds, corner_radius);
    }

    fn draw_raster_image(
        &mut self,
        key: &str,
        bounds: Rect,
        width: u32,
        height: u32,
        rgba: Arc<[u8]>,
        tint: Color,
    ) {
        DrawEncoder::draw_raster_image(self, key, bounds, width, height, rgba, tint);
    }

    fn draw_text(&mut self, text: &str, font_size: f32, position: Point, color: Color) {
        use mondrian_ui_theme::typography::FontWeight;
        let style = TextStyle {
            font_size,
            line_height: font_size * 1.3,
            font_weight: FontWeight::Regular,
            letter_spacing: 0.0,
        };
        self.draw_text(text, &style, position, color);
    }

    fn draw_text_box(
        &mut self,
        text: &str,
        font_size: f32,
        position: Point,
        max_width: f32,
        color: Color,
    ) {
        use mondrian_ui_theme::typography::FontWeight;
        let style = TextStyle {
            font_size,
            line_height: font_size * 1.3,
            font_weight: FontWeight::Regular,
            letter_spacing: 0.0,
        };
        self.draw_text_box(text, &style, position, max_width, color);
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
    fn encoder_draw_gradient_rect_records_corner_colors() {
        let mut enc = DrawEncoder::new();
        enc.draw_gradient_rect(
            rect(),
            [Color::WHITE, Color::BLACK, Color::TRANSPARENT, color()],
            4.0,
        );

        let commands = enc.finish();
        assert_eq!(commands.len(), 1);
        match &commands[0] {
            DrawCommand::GradientRect { colors, corner_radius, .. } => {
                assert_eq!(*corner_radius, 4.0);
                assert_eq!(colors[0], Color::WHITE);
                assert_eq!(colors[1], Color::BLACK);
                assert_eq!(colors[2], Color::TRANSPARENT);
            }
            other => panic!("expected gradient rect command, got {other:?}"),
        }
    }

    #[test]
    fn encoder_draw_soft_shadow_adds_command() {
        let mut enc = DrawEncoder::new();
        enc.draw_soft_shadow(rect(), color(), 8.0, 24.0, 2.0, Vec2::new(0.0, 6.0));

        let commands = enc.finish();
        assert_eq!(commands.len(), 1);
        match &commands[0] {
            DrawCommand::SoftShadow {
                bounds, corner_radius, blur_radius, spread, offset, ..
            } => {
                assert_eq!(*bounds, rect());
                assert_eq!(*corner_radius, 8.0);
                assert_eq!(*blur_radius, 24.0);
                assert_eq!(*spread, 2.0);
                assert_eq!(*offset, Vec2::new(0.0, 6.0));
            }
            other => panic!("expected soft shadow command, got {other:?}"),
        }
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
    fn encoder_draw_triangles_preserves_subpixel_vertices_and_drops_incomplete_tail() {
        let mut enc = DrawEncoder::new();
        enc.draw_triangles(
            &[
                Point::new(0.2, 0.8),
                Point::new(10.1, 0.1),
                Point::new(0.4, 10.7),
                Point::new(99.0, 99.0),
            ],
            color(),
        );

        let commands = enc.finish();
        assert_eq!(commands.len(), 1);
        match &commands[0] {
            DrawCommand::Triangles { vertices, .. } => {
                assert_eq!(vertices.len(), 3);
                assert_eq!(vertices[0], Point::new(0.2, 0.8));
                assert_eq!(vertices[1], Point::new(10.1, 0.1));
                assert_eq!(vertices[2], Point::new(0.4, 10.7));
            }
            other => panic!("expected triangles command, got {other:?}"),
        }
    }

    #[test]
    fn encoder_draw_colored_triangles_preserves_subpixel_vertices_and_drops_incomplete_tail() {
        let mut enc = DrawEncoder::new();
        enc.draw_colored_triangles(&[
            (Point::new(0.2, 0.8), Color::WHITE),
            (Point::new(10.1, 0.1), Color::BLACK),
            (Point::new(0.4, 10.7), Color::TRANSPARENT),
            (Point::new(99.0, 99.0), color()),
        ]);

        let commands = enc.finish();
        assert_eq!(commands.len(), 1);
        match &commands[0] {
            DrawCommand::ColoredTriangles { vertices, mask } => {
                assert!(mask.is_none());
                assert_eq!(vertices.len(), 3);
                assert_eq!(vertices[0].0, Point::new(0.2, 0.8));
                assert_eq!(vertices[1].0, Point::new(10.1, 0.1));
                assert_eq!(vertices[1].1, Color::BLACK);
            }
            other => panic!("expected colored triangles command, got {other:?}"),
        }
    }

    #[test]
    fn encoder_draw_masked_colored_triangles_preserves_vertices_and_mask() {
        let mut enc = DrawEncoder::new();
        enc.draw_colored_triangles_in_rect(
            &[
                (Point::new(0.2, 0.8), Color::WHITE),
                (Point::new(10.1, 0.1), Color::BLACK),
                (Point::new(0.4, 10.7), Color::TRANSPARENT),
            ],
            Rect::new(1.4, 2.6, 24.2, 25.7),
            6.0,
        );

        let commands = enc.finish();
        assert_eq!(commands.len(), 1);
        match &commands[0] {
            DrawCommand::ColoredTriangles { vertices, mask } => {
                assert_eq!(vertices[0].0, Point::new(0.2, 0.8));
                assert_eq!(vertices[1].0, Point::new(10.1, 0.1));
                assert_eq!(
                    *mask,
                    Some(ShapeMask {
                        bounds: Rect::new(1.4, 2.6, 24.2, 25.7),
                        corner_radius: 6.0,
                    })
                );
            }
            other => panic!("expected colored triangles command, got {other:?}"),
        }
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
    fn encoder_draw_text_box_records_max_width() {
        let mut enc = DrawEncoder::new();
        let style = TextStyle {
            font_size: 14.0,
            line_height: 20.0,
            font_weight: mondrian_ui_theme::typography::FontWeight::Regular,
            letter_spacing: 0.0,
        };
        enc.draw_text_box("hello world", &style, Point::ZERO, 120.0, color());

        let commands = enc.finish();
        assert_eq!(commands.len(), 1);
        match &commands[0] {
            DrawCommand::Text { max_width, .. } => assert_eq!(*max_width, Some(120.0)),
            other => panic!("expected text command, got {other:?}"),
        }
    }

    #[test]
    fn encoder_draw_image() {
        let mut enc = DrawEncoder::new();
        enc.draw_image(rect(), rect(), color());
        assert_eq!(enc.command_count(), 1);
    }

    #[test]
    fn encoder_draw_raster_image_records_valid_rgba_payload() {
        let mut enc = DrawEncoder::new();
        enc.draw_raster_image(
            "icon.copy.16",
            rect(),
            2,
            1,
            Arc::from(vec![255u8; 8]),
            color(),
        );

        let commands = enc.finish();

        assert_eq!(commands.len(), 1);
        match &commands[0] {
            DrawCommand::RasterImage { key, width, height, rgba, .. } => {
                assert_eq!(key, "icon.copy.16");
                assert_eq!((*width, *height), (2, 1));
                assert_eq!(rgba.len(), 8);
            }
            other => panic!("expected raster image command, got {other:?}"),
        }
    }

    #[test]
    fn encoder_draw_raster_image_rejects_mismatched_payload_size() {
        let mut enc = DrawEncoder::new();
        enc.draw_raster_image("bad", rect(), 2, 2, Arc::from(vec![255u8; 8]), color());

        assert!(enc.is_empty());
    }

    #[test]
    fn encoder_draw_raster_image_rejects_overflowing_dimensions() {
        let mut enc = DrawEncoder::new();
        enc.draw_raster_image(
            "huge",
            rect(),
            u32::MAX,
            u32::MAX,
            Arc::from(vec![255u8; 4]),
            color(),
        );

        assert!(enc.is_empty());
    }

    #[test]
    fn encoder_preserves_subpixel_geometry_but_expands_clips_conservatively() {
        let mut enc = DrawEncoder::new();
        enc.push_clip(Rect::new(0.4, 1.6, 100.3, 49.8));
        enc.push_translate(Vec2::new(2.2, 3.8));
        enc.draw_rect(Rect::new(9.5, 10.4, 20.6, 30.2), color(), 4.0);
        enc.draw_line(Point::new(1.2, 2.8), Point::new(9.7, 10.1), 1.0, color());
        enc.draw_image(
            Rect::new(4.4, 5.5, 12.6, 16.1),
            Rect::new(0.25, 0.25, 0.5, 0.5),
            color(),
        );
        enc.pop_transform();
        enc.pop_clip();

        let cmds = enc.finish();
        assert!(matches!(
            cmds[0],
            DrawCommand::PushClip { bounds }
                if bounds == Rect::new(0.0, 1.0, 101.0, 51.0)
        ));
        assert!(matches!(
            cmds[1],
            DrawCommand::PushTranslate { offset }
                if offset == Vec2::new(2.2, 3.8)
        ));
        assert!(matches!(
            cmds[2],
            DrawCommand::Rect { bounds, .. }
                if bounds == Rect::new(9.5, 10.4, 20.6, 30.2)
        ));
        assert!(matches!(
            cmds[3],
            DrawCommand::Line { start, end, .. }
                if start == Point::new(1.2, 2.8) && end == Point::new(9.7, 10.1)
        ));
        assert!(matches!(
            cmds[4],
            DrawCommand::Image { bounds, uv_rect, .. }
                if bounds == Rect::new(4.4, 5.5, 12.6, 16.1)
                    && uv_rect == Rect::new(0.25, 0.25, 0.5, 0.5)
        ));
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
