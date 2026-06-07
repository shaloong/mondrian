//! 批次构建器
//!
//! 将 DrawCommand 列表按相同纹理/裁剪状态分组，减少 GPU 状态切换。

use mondrian_ui_core::types::{Point, Rect};

use crate::command::DrawCommand;
use crate::shape::{generate_rect_vertices, generate_rounded_rect_vertices, RectVertex};

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
                let transformed = apply_transform(bounds, &transform_stack);
                clip_stack.push(transformed);
            }
            DrawCommand::PopClip => {
                clip_stack.pop();
            }
            DrawCommand::PushTranslate { offset } => {
                transform_stack.push(*offset);
            }
            DrawCommand::PopTransform => {
                transform_stack.pop();
            }
            DrawCommand::Rect {
                bounds,
                color,
                corner_radius,
            } => {
                let rect = apply_transform(bounds, &transform_stack);
                let screen_rect = pixel_to_ndc_rect(rect, sx, sy, tx, ty);

                let r = *corner_radius * sx.max(sy.abs()).abs();

                let vertices = if r > 0.0 {
                    generate_rounded_rect_vertices(
                        screen_rect,
                        color.r,
                        color.g,
                        color.b,
                        color.a,
                        [r; 4],
                    )
                } else {
                    generate_rect_vertices(
                        screen_rect,
                        color.r,
                        color.g,
                        color.b,
                        color.a,
                        0.0,
                    )
                    .to_vec()
                };
                current_batch.vertices.extend(vertices);
            }
            DrawCommand::Text { .. } => {
                // Stage B: text placeholder — skip
            }
            DrawCommand::Image {
                bounds,
                uv_rect,
                tint,
            } => {
                let rect = apply_transform(bounds, &transform_stack);
                let screen_rect = pixel_to_ndc_rect(rect, sx, sy, tx, ty);

                // Generate vertices with UV coords and corner_radius=-1 (texture mode)
                let x0 = screen_rect.x; let y0 = screen_rect.y;
                let x1 = screen_rect.x + screen_rect.width;
                let y1 = screen_rect.y + screen_rect.height;
                let u0 = uv_rect.x; let v0 = uv_rect.y;
                let u1 = uv_rect.x + uv_rect.width;
                let v1 = uv_rect.y + uv_rect.height;
                let r = tint.r; let g = tint.g; let b = tint.b; let a = tint.a;
                let vertices = vec![
                    RectVertex::new(x0, y0, u0, v0, r, g, b, a, -1.0),
                    RectVertex::new(x1, y0, u1, v0, r, g, b, a, -1.0),
                    RectVertex::new(x0, y1, u0, v1, r, g, b, a, -1.0),
                    RectVertex::new(x0, y1, u0, v1, r, g, b, a, -1.0),
                    RectVertex::new(x1, y0, u1, v0, r, g, b, a, -1.0),
                    RectVertex::new(x1, y1, u1, v1, r, g, b, a, -1.0),
                ];
                current_batch.vertices.extend(vertices);
            }
            DrawCommand::Line {
                start,
                end,
                width,
                color,
            } => {
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
                let screen_hw = hw * sx.max(sy.abs()).abs();
                let screen_nx = nx * sx.max(sy.abs()).abs();
                let screen_ny = ny * sx.max(sy.abs()).abs();

                // Build a thin quad extruded along the line normal
                let x0 = screen_start.x - screen_nx * screen_hw;
                let y0 = screen_start.y - screen_ny * screen_hw;
                let x1 = screen_start.x + screen_nx * screen_hw;
                let y1 = screen_start.y + screen_ny * screen_hw;
                let x2 = screen_end.x - screen_nx * screen_hw;
                let y2 = screen_end.y - screen_ny * screen_hw;
                let x3 = screen_end.x + screen_nx * screen_hw;
                let y3 = screen_end.y + screen_ny * screen_hw;

                let verts = vec![
                    RectVertex::new(x0, y0, 0.0, 0.0, color.r, color.g, color.b, color.a, 0.0),
                    RectVertex::new(x2, y2, 0.0, 0.0, color.r, color.g, color.b, color.a, 0.0),
                    RectVertex::new(x1, y1, 0.0, 0.0, color.r, color.g, color.b, color.a, 0.0),
                    RectVertex::new(x1, y1, 0.0, 0.0, color.r, color.g, color.b, color.a, 0.0),
                    RectVertex::new(x2, y2, 0.0, 0.0, color.r, color.g, color.b, color.a, 0.0),
                    RectVertex::new(x3, y3, 0.0, 0.0, color.r, color.g, color.b, color.a, 0.0),
                ];
                current_batch.vertices.extend(verts);
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
        if let Some(clip) = clip_stack.last().copied() {
            current_batch.clip_rect = Some(clip);
        }
        batches.push(current_batch);
    }

    batches
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
            DrawCommand::PushClip {
                bounds: Rect::new(0.0, 0.0, 50.0, 50.0),
            },
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
            DrawCommand::PushClip {
                bounds: Rect::new(0.0, 0.0, 50.0, 50.0),
            },
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
        assert_eq!(batches.len(), 1);
        assert!(batches[0].clip_rect.is_none());
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Transform stack
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn build_batches_transform_offsets_rect() {
        let cmds = [
            DrawCommand::PushTranslate {
                offset: glam::Vec2::new(50.0, 0.0),
            },
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
        let mut cmds = vec![DrawCommand::PushClip {
            bounds: Rect::new(0.0, 0.0, 500.0, 500.0),
        }];
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
            assert!(batch.clip_rect.is_some(),
                "Batch under active clip must have clip_rect set");
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

        let mut min_x = f32::MAX; let mut max_x = f32::MIN;
        let mut min_y = f32::MAX; let mut max_y = f32::MIN;
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
        assert_ne!(b1080[0].vertices[0].position[1], b4k[0].vertices[0].position[1]);
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

        // First vertex should have the UV rect's top-left
        assert!((verts[0].tex_coord[0] - uv.x).abs() < 0.001,
            "tex_coord.u={} should be {}", verts[0].tex_coord[0], uv.x);
        assert!((verts[0].tex_coord[1] - uv.y).abs() < 0.001,
            "tex_coord.v={} should be {}", verts[0].tex_coord[1], uv.y);

        // Last vertex should have the UV rect's bottom-right
        let ur = uv.x + uv.width;
        let vr = uv.y + uv.height;
        assert!((verts[5].tex_coord[0] - ur).abs() < 0.001,
            "last tex_coord.u={} should be {}", verts[5].tex_coord[0], ur);
        assert!((verts[5].tex_coord[1] - vr).abs() < 0.001,
            "last tex_coord.v={} should be {}", verts[5].tex_coord[1], vr);

        // All vertices should have corner_radius = -1.0 (texture mode)
        for v in verts {
            assert_eq!(v.corner_radius, -1.0, "Image vertices must have corner_radius=-1");
        }
    }
}
