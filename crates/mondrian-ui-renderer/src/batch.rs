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

/// 将 DrawCommands 转换为 DrawBatch 列表
pub fn build_batches(commands: &[DrawCommand], screen_size: (u32, u32)) -> Vec<DrawBatch> {
    let mut batches: Vec<DrawBatch> = Vec::new();
    let mut current_batch = DrawBatch {
        vertices: Vec::new(),
        clip_rect: None,
        texture_key: None,
    };

    let mut clip_stack: Vec<Rect> = Vec::new();
    let mut transform_stack: Vec<glam::Vec2> = Vec::new();

    // 视口变换矩阵，将像素坐标 → NDC
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
                let screen_rect = Rect::new(
                    rect.x * sx + tx,
                    rect.y * sy + ty,
                    rect.width * sx,
                    rect.height * sy.abs(),
                );

                let r = *corner_radius * sx.max(sy.abs()).abs();

                let vertices = if r > 0.0 {
                    generate_rounded_rect_vertices(
                        screen_rect,
                        color.r, color.g, color.b, color.a,
                        [r; 4],
                    )
                } else {
                    generate_rect_vertices(screen_rect, color.r, color.g, color.b, color.a, 0.0)
                        .to_vec()
                };
                current_batch.vertices.extend(vertices);
            }
            DrawCommand::Text { .. } => {
                // Stage B: text placeholder — skip
            }
            DrawCommand::Image { bounds, uv_rect, tint } => {
                let rect = apply_transform(bounds, &transform_stack);
                let screen_rect = Rect::new(
                    rect.x * sx + tx,
                    rect.y * sy + ty,
                    rect.width * sx,
                    rect.height * sy.abs(),
                );

                let vertices = generate_rect_vertices(
                    screen_rect,
                    tint.r, tint.g, tint.b, tint.a, 0.0,
                );
                current_batch.vertices.extend(vertices);
            }
            DrawCommand::Line { start, end, width, color } => {
                // 简化为细矩形
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

                let lr = Rect::from_min_max(
                    screen_start.x - screen_hw,
                    screen_start.y - screen_hw,
                    screen_start.x + screen_hw,
                    screen_start.y + screen_hw,
                );
                let vertices = generate_rect_vertices(lr, color.r, color.g, color.b, color.a, 0.0);
                current_batch.vertices.extend(vertices);
            }
        }

        // 当批次过大时分割
        if current_batch.vertices.len() > 16384 {
            let clip = clip_stack.last().copied();
            let finished = std::mem::replace(
                &mut current_batch,
                DrawBatch {
                    vertices: Vec::new(),
                    clip_rect: clip,
                    texture_key: None,
                },
            );
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
