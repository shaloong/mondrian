//! 批次构建器
//!
//! 将 DrawCommand 列表按相同纹理/裁剪状态分组，减少 GPU 状态切换。

use mondrian_ui_core::types::{Point, Rect};

use crate::command::DrawCommand;
use crate::shape::{
    generate_gradient_rect_vertices, generate_rect_vertices, RectVertex, RenderMode,
};

/// 一个绘制批次 —— 一组顶点 + 可选的裁剪矩形
#[derive(Debug, Clone)]
pub struct DrawBatch {
    pub vertices: Vec<RectVertex>,
    pub clip_rect: Option<Rect>,
    pub texture_key: Option<String>,
}

/// 将 DrawCommand 序列转换为 Y 轴翻转后的 NDC 坐标批次
///
/// 输入像素坐标的原点为左上角。输出 NDC 坐标 y=1 为顶部，y=-1 为底部。
/// 每个批次最多容纳 16384 个顶点；溢出时自动分割批次并保留裁剪状态。
pub fn build_batches(commands: &[DrawCommand], screen_size: (u32, u32)) -> Vec<DrawBatch> {
    let mut batches: Vec<DrawBatch> = Vec::new();
    let mut current_batch = DrawBatch {
        vertices: Vec::new(),
        clip_rect: None,
        texture_key: None,
    };

    let mut clip_stack: Vec<Rect> = Vec::new();
    let mut transform_stack: Vec<glam::Vec2> = Vec::new();

    let sx = 2.0 / screen_size.0 as f32;
    let sy = -2.0 / screen_size.1 as f32;
    let tx = -1.0;
    let ty = 1.0;

    for cmd in commands {
        match cmd {
            DrawCommand::PushClip { bounds } => {
                finish_batch_if_needed(
                    &mut batches,
                    &mut current_batch,
                    clip_stack.last().copied(),
                );
                let transformed = apply_transform(bounds, &transform_stack);
                clip_stack.push(transformed);
            }
            DrawCommand::PopClip => {
                finish_batch_if_needed(
                    &mut batches,
                    &mut current_batch,
                    clip_stack.last().copied(),
                );
                clip_stack.pop();
            }
            DrawCommand::PushTranslate { offset } => {
                transform_stack.push(*offset);
            }
            DrawCommand::PopTransform => {
                transform_stack.pop();
            }
            DrawCommand::Rect { bounds, color, corner_radius } => {
                let pixel_w = bounds.width.max(1.0);
                let pixel_h = bounds.height.max(1.0);

                let rect = apply_transform(bounds, &transform_stack);
                let screen_rect = pixel_to_ndc_rect(rect, sx, sy, tx, ty);

                let vertices = generate_rect_vertices(
                    screen_rect,
                    color.r,
                    color.g,
                    color.b,
                    color.a,
                    pixel_w,
                    pixel_h,
                    *corner_radius,
                    RenderMode::Shape,
                );
                current_batch.vertices.extend(vertices);
            }
            DrawCommand::GradientRect { bounds, colors, corner_radius } => {
                let pixel_w = bounds.width.max(1.0);
                let pixel_h = bounds.height.max(1.0);

                let rect = apply_transform(bounds, &transform_stack);
                let screen_rect = pixel_to_ndc_rect(rect, sx, sy, tx, ty);

                let vertices = generate_gradient_rect_vertices(
                    screen_rect,
                    colors,
                    pixel_w,
                    pixel_h,
                    *corner_radius,
                    RenderMode::Shape,
                );
                current_batch.vertices.extend(vertices);
            }
            DrawCommand::Text { .. } => {
                // Stage B: text placeholder — skip
            }
            DrawCommand::Image { bounds, uv_rect, tint } => {
                ensure_texture_key(
                    &mut batches,
                    &mut current_batch,
                    clip_stack.last().copied(),
                    None,
                );
                let rect = apply_transform(bounds, &transform_stack);
                let screen_rect = pixel_to_ndc_rect(rect, sx, sy, tx, ty);

                // Generate vertices with UV coords and render_mode=Glyph
                let x0 = screen_rect.x;
                let y0 = screen_rect.y;
                let x1 = screen_rect.x + screen_rect.width;
                let y1 = screen_rect.y + screen_rect.height;
                let u0 = uv_rect.x;
                let v0 = uv_rect.y;
                let u1 = uv_rect.x + uv_rect.width;
                let v1 = uv_rect.y + uv_rect.height;
                let r = tint.r;
                let g = tint.g;
                let b = tint.b;
                let a = tint.a;
                // NDC Y is flipped (y0=bottom, y1=top), so swap V coords:
                // bottom vertices → v1 (bottom of glyph), top vertices → v0 (top of glyph)
                let bw = bounds.width.max(1.0);
                let bh = bounds.height.max(1.0);
                let vertices = vec![
                    RectVertex::new(x0, y0, u0, v1, r, g, b, a, bw, bh, 0.0, RenderMode::Glyph),
                    RectVertex::new(x1, y0, u1, v1, r, g, b, a, bw, bh, 0.0, RenderMode::Glyph),
                    RectVertex::new(x0, y1, u0, v0, r, g, b, a, bw, bh, 0.0, RenderMode::Glyph),
                    RectVertex::new(x0, y1, u0, v0, r, g, b, a, bw, bh, 0.0, RenderMode::Glyph),
                    RectVertex::new(x1, y0, u1, v1, r, g, b, a, bw, bh, 0.0, RenderMode::Glyph),
                    RectVertex::new(x1, y1, u1, v0, r, g, b, a, bw, bh, 0.0, RenderMode::Glyph),
                ];
                current_batch.vertices.extend(vertices);
            }
            DrawCommand::RasterImage { .. } => {
                // The renderer resolves these into RasterAtlasImage before batching.
            }
            DrawCommand::RasterAtlasImage { bounds, uv_rect, tint } => {
                ensure_texture_key(
                    &mut batches,
                    &mut current_batch,
                    clip_stack.last().copied(),
                    Some("image".to_string()),
                );
                let rect = apply_transform(bounds, &transform_stack);
                let screen_rect = pixel_to_ndc_rect(rect, sx, sy, tx, ty);

                let x0 = screen_rect.x;
                let y0 = screen_rect.y;
                let x1 = screen_rect.x + screen_rect.width;
                let y1 = screen_rect.y + screen_rect.height;
                let u0 = uv_rect.x;
                let v0 = uv_rect.y;
                let u1 = uv_rect.x + uv_rect.width;
                let v1 = uv_rect.y + uv_rect.height;
                let r = tint.r;
                let g = tint.g;
                let b = tint.b;
                let a = tint.a;
                let bw = bounds.width.max(1.0);
                let bh = bounds.height.max(1.0);
                let vertices = vec![
                    RectVertex::new(x0, y0, u0, v1, r, g, b, a, bw, bh, 0.0, RenderMode::Glyph),
                    RectVertex::new(x1, y0, u1, v1, r, g, b, a, bw, bh, 0.0, RenderMode::Glyph),
                    RectVertex::new(x0, y1, u0, v0, r, g, b, a, bw, bh, 0.0, RenderMode::Glyph),
                    RectVertex::new(x0, y1, u0, v0, r, g, b, a, bw, bh, 0.0, RenderMode::Glyph),
                    RectVertex::new(x1, y0, u1, v1, r, g, b, a, bw, bh, 0.0, RenderMode::Glyph),
                    RectVertex::new(x1, y1, u1, v0, r, g, b, a, bw, bh, 0.0, RenderMode::Glyph),
                ];
                current_batch.vertices.extend(vertices);
            }
            DrawCommand::Line { start, end, width, color } => {
                let dx = end.x - start.x;
                let dy = end.y - start.y;
                let len = (dx * dx + dy * dy).sqrt().max(0.001);
                let nx = -dy / len;
                let ny = dx / len;
                let hw = (*width).max(1.0) * 0.5;

                let n_start = apply_transform_point(start, &transform_stack);
                let n_end = apply_transform_point(end, &transform_stack);

                let screen_start = Point::new(sx * n_start.x + tx, sy * n_start.y + ty);
                let screen_end = Point::new(sx * n_end.x + tx, sy * n_end.y + ty);
                // Scale pixel half-width to NDC separately per axis so the line
                // appears uniformly wide regardless of orientation.
                let off_x = nx * hw * sx;
                let off_y = ny * hw * sy.abs();

                // Build a thin quad extruded along the line normal
                let x0 = screen_start.x - off_x;
                let y0 = screen_start.y - off_y;
                let x1 = screen_start.x + off_x;
                let y1 = screen_start.y + off_y;
                let x2 = screen_end.x - off_x;
                let y2 = screen_end.y - off_y;
                let x3 = screen_end.x + off_x;
                let y3 = screen_end.y + off_y;

                let verts = line_vertices([(x0, y0), (x1, y1), (x2, y2), (x3, y3)], color);
                current_batch.vertices.extend(verts);
            }
            DrawCommand::Triangles { vertices, color } => {
                for triangle in vertices.chunks_exact(3) {
                    let points = [
                        point_to_ndc(
                            apply_transform_point(&triangle[0], &transform_stack),
                            sx,
                            sy,
                            tx,
                            ty,
                        ),
                        point_to_ndc(
                            apply_transform_point(&triangle[1], &transform_stack),
                            sx,
                            sy,
                            tx,
                            ty,
                        ),
                        point_to_ndc(
                            apply_transform_point(&triangle[2], &transform_stack),
                            sx,
                            sy,
                            tx,
                            ty,
                        ),
                    ];
                    current_batch.vertices.extend(triangle_vertices(points, color));
                }
            }
            DrawCommand::ColoredTriangles { vertices, mask } => {
                let mask_rect = mask.map(|mask| apply_transform(&mask.bounds, &transform_stack));
                for triangle in vertices.chunks_exact(3) {
                    let point = |index: usize| {
                        let (point, color) = triangle[index];
                        let transformed = apply_transform_point(&point, &transform_stack);
                        let tex_coord = mask_rect
                            .map(|rect| {
                                [
                                    ((transformed.x - rect.x) / rect.width.max(1.0))
                                        .clamp(0.0, 1.0),
                                    ((transformed.y - rect.y) / rect.height.max(1.0))
                                        .clamp(0.0, 1.0),
                                ]
                            })
                            .unwrap_or([0.0, 0.0]);
                        (point_to_ndc(transformed, sx, sy, tx, ty), color, tex_coord)
                    };
                    let points = [point(0), point(1), point(2)];
                    let rect_size = mask
                        .map(|mask| [mask.bounds.width.max(1.0), mask.bounds.height.max(1.0)])
                        .unwrap_or([1.0, 1.0]);
                    let corner_radius = mask.map(|mask| mask.corner_radius).unwrap_or(0.0);
                    current_batch.vertices.extend(colored_triangle_vertices(
                        points,
                        rect_size,
                        corner_radius,
                    ));
                }
            }
        }

        // Split batch when vertex count exceeds limit; preserve clip state
        if current_batch.vertices.len() > 16384 {
            let current_clip = clip_stack.last().copied();
            let mut finished = std::mem::replace(
                &mut current_batch,
                DrawBatch {
                    vertices: Vec::new(),
                    clip_rect: current_clip,
                    texture_key: None,
                },
            );
            finished.clip_rect = current_clip;
            batches.push(finished);
        }
    }

    if !current_batch.vertices.is_empty() {
        finish_batch_if_needed(&mut batches, &mut current_batch, clip_stack.last().copied());
    }

    batches
}

fn line_vertices(points: [(f32, f32); 4], color: &mondrian_core::Color) -> [RectVertex; 6] {
    let [p0, p1, p2, p3] = points;
    let order = if signed_triangle_area(p0, p2, p1) >= 0.0 {
        [p0, p2, p1, p1, p2, p3]
    } else {
        [p0, p1, p2, p2, p1, p3]
    };

    order.map(|(x, y)| {
        RectVertex::new(
            x,
            y,
            0.0,
            0.0,
            color.r,
            color.g,
            color.b,
            color.a,
            1.0,
            1.0,
            0.0,
            RenderMode::Shape,
        )
    })
}

fn triangle_vertices(points: [Point; 3], color: &mondrian_core::Color) -> [RectVertex; 3] {
    let mut points = [
        (points[0].x, points[0].y),
        (points[1].x, points[1].y),
        (points[2].x, points[2].y),
    ];
    if signed_triangle_area(points[0], points[1], points[2]) < 0.0 {
        points.swap(1, 2);
    }

    points.map(|(x, y)| {
        RectVertex::new(
            x,
            y,
            0.0,
            0.0,
            color.r,
            color.g,
            color.b,
            color.a,
            1.0,
            1.0,
            0.0,
            RenderMode::Shape,
        )
    })
}

fn colored_triangle_vertices(
    points: [(Point, mondrian_core::Color, [f32; 2]); 3],
    rect_size: [f32; 2],
    corner_radius: f32,
) -> [RectVertex; 3] {
    let mut points = [
        ((points[0].0.x, points[0].0.y), points[0].1, points[0].2),
        ((points[1].0.x, points[1].0.y), points[1].1, points[1].2),
        ((points[2].0.x, points[2].0.y), points[2].1, points[2].2),
    ];
    if signed_triangle_area(points[0].0, points[1].0, points[2].0) < 0.0 {
        points.swap(1, 2);
    }

    points.map(|((x, y), color, tex_coord)| {
        RectVertex::new(
            x,
            y,
            tex_coord[0],
            tex_coord[1],
            color.r,
            color.g,
            color.b,
            color.a,
            rect_size[0],
            rect_size[1],
            corner_radius,
            RenderMode::Shape,
        )
    })
}

fn signed_triangle_area(a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> f32 {
    (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0)
}

fn finish_batch_if_needed(
    batches: &mut Vec<DrawBatch>,
    current_batch: &mut DrawBatch,
    clip_rect: Option<Rect>,
) {
    if current_batch.vertices.is_empty() {
        return;
    }
    current_batch.clip_rect = clip_rect;
    batches.push(std::mem::replace(
        current_batch,
        DrawBatch { vertices: Vec::new(), clip_rect, texture_key: None },
    ));
}

fn ensure_texture_key(
    batches: &mut Vec<DrawBatch>,
    current_batch: &mut DrawBatch,
    clip_rect: Option<Rect>,
    texture_key: Option<String>,
) {
    if current_batch.texture_key == texture_key {
        return;
    }
    finish_batch_if_needed(batches, current_batch, clip_rect);
    current_batch.texture_key = texture_key;
}

/// Convert pixel-space rect to NDC coordinates with Y-flip.
///
/// Pixel origin (top-left) maps to NDC y=1 (top).
/// NDC y direction is preserved correct: top→bottom in pixel space maps to y=1→y=-1.
fn pixel_to_ndc_rect(rect: Rect, sx: f32, sy: f32, tx: f32, ty: f32) -> Rect {
    let ndc_x = rect.x * sx + tx;
    let ndc_w = rect.width * sx;

    // Y-flip: pixel y=0 → NDC y=1 (top), pixel y=height → NDC y=-1 (bottom)
    let y_top = rect.y * sy + ty;
    let y_bottom = (rect.y + rect.height) * sy + ty;
    let ndc_y = y_top.min(y_bottom); // always the smaller value
    let ndc_h = (y_bottom - y_top).abs();

    Rect::new(ndc_x, ndc_y, ndc_w, ndc_h)
}

fn apply_transform(rect: &Rect, stack: &[glam::Vec2]) -> Rect {
    let mut offset = glam::Vec2::ZERO;
    for t in stack {
        offset += *t;
    }
    Rect::new(
        rect.x + offset.x,
        rect.y + offset.y,
        rect.width,
        rect.height,
    )
}

fn apply_transform_point(p: &Point, stack: &[glam::Vec2]) -> Point {
    let mut offset = glam::Vec2::ZERO;
    for t in stack {
        offset += *t;
    }
    Point::new(p.x + offset.x, p.y + offset.y)
}

fn point_to_ndc(point: Point, sx: f32, sy: f32, tx: f32, ty: f32) -> Point {
    Point::new(sx * point.x + tx, sy * point.y + ty)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::Color;

    // ═══════════════════════════════════════════════════════════════════════
    // Empty / basic
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn build_batches_empty_returns_empty() {
        let batches = build_batches(&[], (1920, 1080));
        assert!(batches.is_empty());
    }

    #[test]
    fn build_batches_single_rect_returns_one_batch() {
        let cmds = [DrawCommand::Rect {
            bounds: Rect::new(0.0, 0.0, 100.0, 50.0),
            color: Color::WHITE,
            corner_radius: 0.0,
        }];
        let batches = build_batches(&cmds, (1920, 1080));
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].vertices.len(), 6);
    }

    #[test]
    fn build_batches_gradient_rect_uses_six_vertices_with_corner_colors() {
        let colors = [
            Color::from_rgba8(255, 0, 0, 255),
            Color::from_rgba8(0, 255, 0, 255),
            Color::from_rgba8(0, 0, 255, 255),
            Color::from_rgba8(255, 255, 255, 255),
        ];
        let cmds = [DrawCommand::GradientRect {
            bounds: Rect::new(0.0, 0.0, 100.0, 50.0),
            colors,
            corner_radius: 0.0,
        }];

        let batches = build_batches(&cmds, (100, 100));

        assert_eq!(batches.len(), 1);
        let vertices = &batches[0].vertices;
        assert_eq!(vertices.len(), 6);
        assert_eq!(vertices[0].color, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(vertices[1].color, [0.0, 1.0, 0.0, 1.0]);
        assert_eq!(vertices[2].color, [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(vertices[5].color, [1.0, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn build_batches_colored_triangles_preserves_vertex_colors() {
        let cmds = [DrawCommand::ColoredTriangles {
            vertices: vec![
                (Point::new(0.0, 0.0), Color::from_rgba8(255, 0, 0, 255)),
                (Point::new(10.0, 0.0), Color::from_rgba8(0, 255, 0, 255)),
                (Point::new(0.0, 10.0), Color::from_rgba8(0, 0, 255, 255)),
            ],
            mask: None,
        }];

        let batches = build_batches(&cmds, (100, 100));

        assert_eq!(batches.len(), 1);
        let vertices = &batches[0].vertices;
        assert_eq!(vertices.len(), 3);
        let colors = vertices.iter().map(|vertex| vertex.color).collect::<Vec<_>>();
        assert!(colors.contains(&[1.0, 0.0, 0.0, 1.0]));
        assert!(colors.contains(&[0.0, 1.0, 0.0, 1.0]));
        assert!(colors.contains(&[0.0, 0.0, 1.0, 1.0]));
    }

    #[test]
    fn build_batches_masked_colored_triangles_passes_sdf_mask_data() {
        let cmds = [DrawCommand::ColoredTriangles {
            vertices: vec![
                (Point::new(10.0, 10.0), Color::from_rgba8(255, 0, 0, 255)),
                (Point::new(90.0, 10.0), Color::from_rgba8(0, 255, 0, 255)),
                (Point::new(50.0, 90.0), Color::from_rgba8(0, 0, 255, 255)),
            ],
            mask: Some(crate::command::ShapeMask {
                bounds: Rect::new(10.0, 10.0, 80.0, 80.0),
                corner_radius: 40.0,
            }),
        }];

        let batches = build_batches(&cmds, (100, 100));

        assert_eq!(batches.len(), 1);
        let vertices = &batches[0].vertices;
        assert_eq!(vertices.len(), 3);
        for vertex in vertices {
            assert_eq!(vertex.rect_size, [80.0, 80.0]);
            assert_eq!(vertex.corner_radius_px, 40.0);
            assert!(vertex.tex_coord[0] >= 0.0 && vertex.tex_coord[0] <= 1.0);
            assert!(vertex.tex_coord[1] >= 0.0 && vertex.tex_coord[1] <= 1.0);
        }
    }

    #[test]
    fn build_batches_multiple_rects_same_batch() {
        let cmds = [
            DrawCommand::Rect {
                bounds: Rect::new(0.0, 0.0, 100.0, 50.0),
                color: Color::WHITE,
                corner_radius: 0.0,
            },
            DrawCommand::Rect {
                bounds: Rect::new(10.0, 10.0, 200.0, 30.0),
                color: Color::BLACK,
                corner_radius: 0.0,
            },
        ];
        let batches = build_batches(&cmds, (1920, 1080));
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].vertices.len(), 12);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Clip stack
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn build_batches_clip_applied_on_finalization() {
        let cmds = [
            DrawCommand::PushClip { bounds: Rect::new(0.0, 0.0, 50.0, 50.0) },
            DrawCommand::Rect {
                bounds: Rect::new(0.0, 0.0, 100.0, 100.0),
                color: Color::WHITE,
                corner_radius: 0.0,
            },
            // No PopClip — clip stays active when batch is finalized
        ];
        let batches = build_batches(&cmds, (1920, 1080));
        assert_eq!(batches.len(), 1);
        assert!(batches[0].clip_rect.is_some());
    }

    #[test]
    fn build_batches_no_clip_after_pop() {
        let cmds = [
            DrawCommand::PushClip { bounds: Rect::new(0.0, 0.0, 50.0, 50.0) },
            DrawCommand::Rect {
                bounds: Rect::new(0.0, 0.0, 100.0, 100.0),
                color: Color::WHITE,
                corner_radius: 0.0,
            },
            DrawCommand::PopClip,
            DrawCommand::Rect {
                bounds: Rect::new(200.0, 200.0, 100.0, 100.0),
                color: Color::BLACK,
                corner_radius: 0.0,
            },
        ];
        let batches = build_batches(&cmds, (1920, 1080));
        assert_eq!(batches.len(), 2);
        assert!(batches[0].clip_rect.is_some());
        assert!(batches[1].clip_rect.is_none());
    }

    #[test]
    fn build_batches_flushes_before_clip_state_changes() {
        let cmds = [
            DrawCommand::Rect {
                bounds: Rect::new(0.0, 0.0, 10.0, 10.0),
                color: Color::WHITE,
                corner_radius: 0.0,
            },
            DrawCommand::PushClip { bounds: Rect::new(0.0, 0.0, 50.0, 50.0) },
            DrawCommand::Rect {
                bounds: Rect::new(0.0, 0.0, 100.0, 100.0),
                color: Color::BLACK,
                corner_radius: 0.0,
            },
            DrawCommand::PopClip,
        ];

        let batches = build_batches(&cmds, (1920, 1080));
        assert_eq!(batches.len(), 2);
        assert!(batches[0].clip_rect.is_none());
        assert!(batches[1].clip_rect.is_some());
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Transform stack
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn build_batches_transform_offsets_rect() {
        let cmds = [
            DrawCommand::PushTranslate { offset: glam::Vec2::new(50.0, 0.0) },
            DrawCommand::Rect {
                bounds: Rect::new(0.0, 0.0, 100.0, 100.0),
                color: Color::WHITE,
                corner_radius: 0.0,
            },
            DrawCommand::PopTransform,
        ];
        let batches = build_batches(&cmds, (1920, 1080));
        assert_eq!(batches.len(), 1);
        let v0 = &batches[0].vertices[0];
        assert!(v0.position[0] > -1.0 && v0.position[0] < 1.0);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Batch splitting (large commands)
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn build_batches_splits_on_large_vertex_count() {
        let mut cmds = Vec::new();
        for i in 0..2800i32 {
            cmds.push(DrawCommand::Rect {
                bounds: Rect::new(i as f32, 0.0, 1.0, 1.0),
                color: Color::WHITE,
                corner_radius: 0.0,
            });
        }
        let batches = build_batches(&cmds, (1920, 1080));
        assert!(
            batches.len() >= 2,
            "Should split into at least 2 batches, got {}",
            batches.len()
        );
    }

    #[test]
    fn build_batches_clip_preserved_across_batch_split() {
        // Push a clip, then generate enough rects to overflow a batch.
        // The clip should be attached to BOTH batches.
        let mut cmds = vec![DrawCommand::PushClip { bounds: Rect::new(0.0, 0.0, 500.0, 500.0) }];
        for i in 0..2800i32 {
            cmds.push(DrawCommand::Rect {
                bounds: Rect::new(i as f32, 0.0, 1.0, 1.0),
                color: Color::WHITE,
                corner_radius: 0.0,
            });
        }
        let batches = build_batches(&cmds, (1920, 1080));
        assert!(batches.len() >= 2);
        // All batches (except possibly the last if PopClip happened) must
        // carry the clip rect since the PushClip was never popped.
        for batch in &batches {
            assert!(
                batch.clip_rect.is_some(),
                "Batch under active clip must have clip_rect set"
            );
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Line command
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn build_batches_line_produces_vertices() {
        let cmds = [DrawCommand::Line {
            start: Point::new(0.0, 0.0),
            end: Point::new(100.0, 0.0),
            width: 2.0,
            color: Color::WHITE,
        }];
        let batches = build_batches(&cmds, (1920, 1080));
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].vertices.len(), 6);
    }

    #[test]
    fn build_batches_line_diagonal_does_not_panic() {
        let cmds = [DrawCommand::Line {
            start: Point::new(0.0, 0.0),
            end: Point::new(100.0, 100.0),
            width: 3.0,
            color: Color::WHITE,
        }];
        let batches = build_batches(&cmds, (1920, 1080));
        assert_eq!(batches.len(), 1);
    }

    #[test]
    fn build_batches_vertical_line_triangles_are_front_facing() {
        let cmds = [DrawCommand::Line {
            start: Point::new(50.0, 10.0),
            end: Point::new(50.0, 90.0),
            width: 4.0,
            color: Color::WHITE,
        }];
        let batches = build_batches(&cmds, (100, 100));
        let vertices = &batches[0].vertices;

        for tri in vertices.chunks_exact(3) {
            let a = (tri[0].position[0], tri[0].position[1]);
            let b = (tri[1].position[0], tri[1].position[1]);
            let c = (tri[2].position[0], tri[2].position[1]);
            assert!(signed_triangle_area(a, b, c) > 0.0);
        }
    }

    #[test]
    fn build_batches_line_uses_stable_butt_caps() {
        let cmds = [DrawCommand::Line {
            start: Point::new(10.0, 50.0),
            end: Point::new(90.0, 50.0),
            width: 4.0,
            color: Color::WHITE,
        }];
        let batches = build_batches(&cmds, (100, 100));
        let min_x = batches[0].vertices.iter().map(|v| v.position[0]).fold(f32::INFINITY, f32::min);
        let max_x = batches[0]
            .vertices
            .iter()
            .map(|v| v.position[0])
            .fold(f32::NEG_INFINITY, f32::max);

        assert!((min_x - (-0.8)).abs() < 0.001);
        assert!((max_x - 0.8).abs() < 0.001);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Triangle command
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn build_batches_triangles_are_front_facing_after_y_flip() {
        let cmds = [DrawCommand::Triangles {
            vertices: vec![
                Point::new(10.0, 10.0),
                Point::new(90.0, 10.0),
                Point::new(10.0, 90.0),
                Point::new(90.0, 10.0),
                Point::new(90.0, 90.0),
                Point::new(10.0, 90.0),
            ],
            color: Color::WHITE,
        }];
        let batches = build_batches(&cmds, (100, 100));

        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].vertices.len(), 6);
        for tri in batches[0].vertices.chunks_exact(3) {
            let a = (tri[0].position[0], tri[0].position[1]);
            let b = (tri[1].position[0], tri[1].position[1]);
            let c = (tri[2].position[0], tri[2].position[1]);
            assert!(signed_triangle_area(a, b, c) > 0.0);
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Image command
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn build_batches_image_produces_vertices() {
        let cmds = [DrawCommand::Image {
            bounds: Rect::new(0.0, 0.0, 100.0, 100.0),
            uv_rect: Rect::new(0.0, 0.0, 1.0, 1.0),
            tint: Color::WHITE,
        }];
        let batches = build_batches(&cmds, (1920, 1080));
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].vertices.len(), 6);
    }

    #[test]
    fn build_batches_raster_atlas_images_use_image_texture_key() {
        let cmds = [
            DrawCommand::Image {
                bounds: Rect::new(0.0, 0.0, 16.0, 16.0),
                uv_rect: Rect::new(0.0, 0.0, 0.01, 0.01),
                tint: Color::WHITE,
            },
            DrawCommand::RasterAtlasImage {
                bounds: Rect::new(20.0, 0.0, 16.0, 16.0),
                uv_rect: Rect::new(0.1, 0.0, 0.01, 0.01),
                tint: Color::WHITE,
            },
        ];

        let batches = build_batches(&cmds, (100, 100));

        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].texture_key, None);
        assert_eq!(batches[1].texture_key.as_deref(), Some("image"));
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Text command (skipped in Stage B)
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn build_batches_text_is_skipped() {
        use mondrian_ui_theme::typography::{FontWeight, TextStyle};
        let style = TextStyle {
            font_size: 14.0,
            line_height: 20.0,
            font_weight: FontWeight::Regular,
            letter_spacing: 0.0,
        };
        let cmds = [DrawCommand::Text {
            text: "hello".into(),
            style,
            position: Point::ZERO,
            max_width: None,
            color: Color::WHITE,
        }];
        let batches = build_batches(&cmds, (1920, 1080));
        assert!(batches.is_empty() || batches[0].vertices.is_empty());
    }

    // ═══════════════════════════════════════════════════════════════════════
    // NDC coordinate transform
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn build_batches_fullscreen_rect_in_ndc() {
        let cmds = [DrawCommand::Rect {
            bounds: Rect::new(0.0, 0.0, 1920.0, 1080.0),
            color: Color::WHITE,
            corner_radius: 0.0,
        }];
        let batches = build_batches(&cmds, (1920, 1080));
        let verts = &batches[0].vertices;

        let mut min_x = f32::MAX;
        let mut max_x = f32::MIN;
        let mut min_y = f32::MAX;
        let mut max_y = f32::MIN;
        for v in verts {
            min_x = min_x.min(v.position[0]);
            max_x = max_x.max(v.position[0]);
            min_y = min_y.min(v.position[1]);
            max_y = max_y.max(v.position[1]);
        }

        assert!((min_x + 1.0).abs() < 0.01, "min_x={} should be -1", min_x);
        assert!((max_x - 1.0).abs() < 0.01, "max_x={} should be 1", max_x);
        assert!((min_y + 1.0).abs() < 0.02, "min_y={} should be -1", min_y);
        assert!((max_y - 1.0).abs() < 0.02, "max_y={} should be 1", max_y);
    }

    #[test]
    fn build_batches_rect_fully_in_ndc_bounds() {
        // A small rect at center should be well within [-1, 1]
        let cmds = [DrawCommand::Rect {
            bounds: Rect::new(100.0, 100.0, 200.0, 100.0),
            color: Color::WHITE,
            corner_radius: 0.0,
        }];
        let batches = build_batches(&cmds, (1920, 1080));
        for v in &batches[0].vertices {
            assert!(v.position[0] >= -1.0 && v.position[0] <= 1.0);
            assert!(v.position[1] >= -1.0 && v.position[1] <= 1.0);
        }
    }

    #[test]
    fn build_batches_different_screen_sizes() {
        let cmds = [DrawCommand::Rect {
            bounds: Rect::new(960.0, 540.0, 100.0, 100.0), // centered
            color: Color::WHITE,
            corner_radius: 0.0,
        }];
        let b1080 = build_batches(&cmds, (1920, 1080));
        let b4k = build_batches(&cmds, (3840, 2160));
        // Same logical center at different resolutions → different NDC
        assert_ne!(
            b1080[0].vertices[0].position[1],
            b4k[0].vertices[0].position[1]
        );
    }

    #[test]
    fn image_vertices_have_correct_uvs() {
        // Create an Image command with known UV
        let uv = Rect::new(0.1, 0.2, 0.05, 0.06);
        let cmds = [DrawCommand::Image {
            bounds: Rect::new(100.0, 200.0, 50.0, 30.0),
            uv_rect: uv,
            tint: Color::WHITE,
        }];
        let batches = build_batches(&cmds, (1920, 1080));
        assert_eq!(batches.len(), 1);

        let verts = &batches[0].vertices;
        assert_eq!(verts.len(), 6);

        // After Y-flip correction: bottom vertices get v1 (bottom UV), top get v0
        let v0 = uv.y;
        let v1 = uv.y + uv.height;
        // verts[0] is bottom-left: should have v=v1 (bottom of glyph)
        assert!(
            (verts[0].tex_coord[1] - v1).abs() < 0.001,
            "bottom-left v={} should be v1={}",
            verts[0].tex_coord[1],
            v1
        );
        // verts[2] is top-left: should have v=v0 (top of glyph)
        assert!(
            (verts[2].tex_coord[1] - v0).abs() < 0.001,
            "top-left v={} should be v0={}",
            verts[2].tex_coord[1],
            v0
        );
        // verts[5] is top-right: u=right, v=top
        assert!((verts[5].tex_coord[0] - (uv.x + uv.width)).abs() < 0.001);
        assert!((verts[5].tex_coord[1] - v0).abs() < 0.001);

        // All vertices should have render_mode = Glyph
        for v in verts {
            assert_eq!(
                v.render_mode,
                RenderMode::Glyph as u32,
                "Image vertices must have render_mode=Glyph"
            );
        }
    }
    // ═══════════════════════════════════════════════════════════════════════
    // SDF / 圆形数据完整性：DrawCommand → RectVertex 管线
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn circle_command_passes_correct_rect_size() {
        // 100x100 矩形 + corner_radius=50 → 圆形
        let cmds = [DrawCommand::Rect {
            bounds: Rect::new(100.0, 100.0, 100.0, 100.0),
            color: Color::WHITE,
            corner_radius: 50.0,
        }];
        let batches = build_batches(&cmds, (1920, 1080));
        assert_eq!(batches.len(), 1);
        let verts = &batches[0].vertices;
        assert_eq!(verts.len(), 6);
        for v in verts {
            assert_eq!(
                v.rect_size,
                [100.0, 100.0],
                "rect_size should be pixel dims of original rect"
            );
            assert!(
                (v.corner_radius_px - 50.0).abs() < 0.001,
                "corner_radius_px={} should be 50",
                v.corner_radius_px
            );
            assert!(
                v.tex_coord[0] >= 0.0 && v.tex_coord[0] <= 1.0,
                "tex_coord.x={} should be in [0,1]",
                v.tex_coord[0]
            );
            assert!(
                v.tex_coord[1] >= 0.0 && v.tex_coord[1] <= 1.0,
                "tex_coord.y={} should be in [0,1]",
                v.tex_coord[1]
            );
        }
    }

    #[test]
    fn circle_command_sdf_reconstruction_from_batch() {
        let cmds = [DrawCommand::Rect {
            bounds: Rect::new(0.0, 0.0, 100.0, 100.0),
            color: Color::WHITE,
            corner_radius: 50.0,
        }];
        let batches = build_batches(&cmds, (1920, 1080));
        for v in &batches[0].vertices {
            let p_local = [
                v.tex_coord[0] * v.rect_size[0],
                v.tex_coord[1] * v.rect_size[1],
            ];
            assert!(
                p_local[0] >= 0.0 && p_local[0] <= 100.0,
                "p_local.x={} out of bounds",
                p_local[0]
            );
            assert!(
                p_local[1] >= 0.0 && p_local[1] <= 100.0,
                "p_local.y={} out of bounds",
                p_local[1]
            );
        }
    }

    #[test]
    fn circle_command_rect_size_matches_bounds_not_ndc() {
        // rect_size 始终是像素尺寸，即使矩形进行了平移变换
        let cmds = [
            DrawCommand::PushTranslate { offset: glam::Vec2::new(100.0, 200.0) },
            DrawCommand::Rect {
                bounds: Rect::new(0.0, 0.0, 50.0, 80.0),
                color: Color::WHITE,
                corner_radius: 25.0,
            },
            DrawCommand::PopTransform,
        ];
        let batches = build_batches(&cmds, (1920, 1080));
        for v in &batches[0].vertices {
            assert_eq!(
                v.rect_size,
                [50.0, 80.0],
                "rect_size must remain original pixel dims"
            );
            assert!((v.corner_radius_px - 25.0).abs() < 0.001);
        }
    }
}
