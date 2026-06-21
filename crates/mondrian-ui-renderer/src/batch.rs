//! 批次构建器
//!
//! 将 DrawCommand 列表按相同纹理/裁剪状态分组，减少 GPU 状态切换。

use mondrian_ui_core::types::{Point, Rect};

use crate::command::DrawCommand;
use crate::shape::{
    generate_gradient_rect_vertices, generate_rect_vertices, RectVertex, RenderMode,
};

const LINE_AA_PADDING_PX: f32 = 1.25;
const MIN_LINE_WIDTH_PX: f32 = 1.0;
const MIN_LINE_DIRECTION_LEN: f32 = 0.001;
const MIN_TRIANGLE_AREA_NDC: f32 = 1.0e-12;

#[cfg(test)]
pub(crate) const LINE_SHADER_AA_MIN_PX: f32 = 0.75;
#[cfg(test)]
pub(crate) const LINE_SHADER_AA_MAX_PX: f32 = 1.5;
#[cfg(test)]
pub(crate) const LINE_SHADER_AA_EDGE_SCALE: f32 = 0.5;

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
    if screen_size.0 == 0 || screen_size.1 == 0 {
        return Vec::new();
    }

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
                let effective = if rect_is_visible(transformed) {
                    clip_stack
                        .last()
                        .copied()
                        .map(|parent| intersect_rect(parent, transformed))
                        .unwrap_or(transformed)
                } else {
                    Rect::new(0.0, 0.0, 0.0, 0.0)
                };
                clip_stack.push(effective);
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
                transform_stack.push(if offset.x.is_finite() && offset.y.is_finite() {
                    *offset
                } else {
                    glam::Vec2::ZERO
                });
            }
            DrawCommand::PopTransform => {
                transform_stack.pop();
            }
            DrawCommand::Rect { bounds, color, corner_radius } => {
                if !rect_is_visible(*bounds)
                    || !color_is_finite(*color)
                    || !corner_radius.is_finite()
                {
                    continue;
                }

                let pixel_w = bounds.width.max(1.0);
                let pixel_h = bounds.height.max(1.0);

                let rect = apply_transform(bounds, &transform_stack);
                if !rect_is_visible(rect) {
                    continue;
                }
                let screen_rect = pixel_to_ndc_rect(rect, sx, sy, tx, ty);
                if !rect_is_visible(screen_rect) {
                    continue;
                }

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
                if !rect_is_visible(*bounds)
                    || !colors.iter().all(|color| color_is_finite(*color))
                    || !corner_radius.is_finite()
                {
                    continue;
                }

                let pixel_w = bounds.width.max(1.0);
                let pixel_h = bounds.height.max(1.0);

                let rect = apply_transform(bounds, &transform_stack);
                if !rect_is_visible(rect) {
                    continue;
                }
                let screen_rect = pixel_to_ndc_rect(rect, sx, sy, tx, ty);
                if !rect_is_visible(screen_rect) {
                    continue;
                }

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
                // Text must be resolved by mondrian-ui-text before commands
                // reach this low-level batch builder.
            }
            DrawCommand::Image { bounds, uv_rect, tint } => {
                if !rect_is_visible(*bounds) || !rect_is_finite(*uv_rect) || !color_is_finite(*tint)
                {
                    continue;
                }

                ensure_texture_key(
                    &mut batches,
                    &mut current_batch,
                    clip_stack.last().copied(),
                    None,
                );
                let rect = apply_transform(bounds, &transform_stack);
                if !rect_is_visible(rect) {
                    continue;
                }
                let screen_rect = pixel_to_ndc_rect(rect, sx, sy, tx, ty);
                if !rect_is_visible(screen_rect) {
                    continue;
                }

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
                if !rect_is_visible(*bounds) || !rect_is_finite(*uv_rect) || !color_is_finite(*tint)
                {
                    continue;
                }

                ensure_texture_key(
                    &mut batches,
                    &mut current_batch,
                    clip_stack.last().copied(),
                    Some("image".to_string()),
                );
                let rect = apply_transform(bounds, &transform_stack);
                if !rect_is_visible(rect) {
                    continue;
                }
                let screen_rect = pixel_to_ndc_rect(rect, sx, sy, tx, ty);
                if !rect_is_visible(screen_rect) {
                    continue;
                }

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
                    RectVertex::new(x0, y0, u0, v1, r, g, b, a, bw, bh, 0.0, RenderMode::Image),
                    RectVertex::new(x1, y0, u1, v1, r, g, b, a, bw, bh, 0.0, RenderMode::Image),
                    RectVertex::new(x0, y1, u0, v0, r, g, b, a, bw, bh, 0.0, RenderMode::Image),
                    RectVertex::new(x0, y1, u0, v0, r, g, b, a, bw, bh, 0.0, RenderMode::Image),
                    RectVertex::new(x1, y0, u1, v1, r, g, b, a, bw, bh, 0.0, RenderMode::Image),
                    RectVertex::new(x1, y1, u1, v0, r, g, b, a, bw, bh, 0.0, RenderMode::Image),
                ];
                current_batch.vertices.extend(vertices);
            }
            DrawCommand::Line { start, end, width, color } => {
                let n_start = apply_transform_point(start, &transform_stack);
                let n_end = apply_transform_point(end, &transform_stack);
                if let Some(verts) = line_vertices(n_start, n_end, *width, color, sx, sy, tx, ty) {
                    current_batch.vertices.extend(verts);
                }
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
                    if let Some(vertices) = triangle_vertices(points, color) {
                        current_batch.vertices.extend(vertices);
                    }
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
                    if let Some(vertices) =
                        colored_triangle_vertices(points, rect_size, corner_radius)
                    {
                        current_batch.vertices.extend(vertices);
                    }
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

fn line_vertices(
    start: Point,
    end: Point,
    width: f32,
    color: &mondrian_core::Color,
    sx: f32,
    sy: f32,
    tx: f32,
    ty: f32,
) -> Option<[RectVertex; 6]> {
    if !start.x.is_finite()
        || !start.y.is_finite()
        || !end.x.is_finite()
        || !end.y.is_finite()
        || !width.is_finite()
        || !color_is_finite(*color)
    {
        return None;
    }

    let dx = end.x - start.x;
    let dy = end.y - start.y;
    let len = (dx * dx + dy * dy).sqrt();
    if !len.is_finite() {
        return None;
    }
    let (ux, uy) = if len > MIN_LINE_DIRECTION_LEN {
        (dx / len, dy / len)
    } else {
        (1.0, 0.0)
    };
    let nx = -uy;
    let ny = ux;
    let stroke_width = width.max(MIN_LINE_WIDTH_PX);
    let radius = stroke_width * 0.5;
    let half_geometry_width = radius + LINE_AA_PADDING_PX;
    let axis_padding = half_geometry_width;
    let geometry_len = len + axis_padding * 2.0;
    let geometry_width = half_geometry_width * 2.0;
    if !radius.is_finite()
        || !half_geometry_width.is_finite()
        || !geometry_len.is_finite()
        || !geometry_width.is_finite()
    {
        return None;
    }

    let start_x = start.x - ux * axis_padding;
    let start_y = start.y - uy * axis_padding;
    let end_x = end.x + ux * axis_padding;
    let end_y = end.y + uy * axis_padding;
    let pixel_points = [
        (
            Point::new(
                start_x - nx * half_geometry_width,
                start_y - ny * half_geometry_width,
            ),
            [0.0, 0.0],
        ),
        (
            Point::new(
                start_x + nx * half_geometry_width,
                start_y + ny * half_geometry_width,
            ),
            [0.0, geometry_width],
        ),
        (
            Point::new(
                end_x - nx * half_geometry_width,
                end_y - ny * half_geometry_width,
            ),
            [geometry_len, 0.0],
        ),
        (
            Point::new(
                end_x + nx * half_geometry_width,
                end_y + ny * half_geometry_width,
            ),
            [geometry_len, geometry_width],
        ),
    ];
    if !pixel_points.iter().all(|(point, tex_coord)| {
        point_is_finite(*point) && tex_coord.iter().all(|v| v.is_finite())
    }) {
        return None;
    }
    let points =
        pixel_points.map(|(point, tex_coord)| ((sx * point.x + tx, sy * point.y + ty), tex_coord));
    if !points.iter().all(|((x, y), tex_coord)| {
        x.is_finite() && y.is_finite() && tex_coord.iter().all(|v| v.is_finite())
    }) {
        return None;
    }
    let [p0, p1, p2, p3] = points;
    let area = signed_triangle_area(p0.0, p2.0, p1.0);
    if !area.is_finite() {
        return None;
    }
    let order = if area >= 0.0 {
        [p0, p2, p1, p1, p2, p3]
    } else {
        [p0, p1, p2, p2, p1, p3]
    };

    Some(order.map(|((x, y), tex_coord)| {
        RectVertex::new(
            x,
            y,
            tex_coord[0],
            tex_coord[1],
            color.r,
            color.g,
            color.b,
            color.a,
            geometry_len,
            geometry_width,
            radius,
            RenderMode::Line,
        )
    }))
}

fn triangle_vertices(points: [Point; 3], color: &mondrian_core::Color) -> Option<[RectVertex; 3]> {
    if !points.iter().all(|point| point_is_finite(*point)) || !color_is_finite(*color) {
        return None;
    }

    let mut points = [
        (points[0].x, points[0].y),
        (points[1].x, points[1].y),
        (points[2].x, points[2].y),
    ];
    let area = signed_triangle_area(points[0], points[1], points[2]);
    if !area.is_finite() || area.abs() <= MIN_TRIANGLE_AREA_NDC {
        return None;
    }
    if area < 0.0 {
        points.swap(1, 2);
    }

    Some(points.map(|(x, y)| {
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
    }))
}

fn colored_triangle_vertices(
    points: [(Point, mondrian_core::Color, [f32; 2]); 3],
    rect_size: [f32; 2],
    corner_radius: f32,
) -> Option<[RectVertex; 3]> {
    if !points.iter().all(|(point, color, tex_coord)| {
        point_is_finite(*point)
            && color_is_finite(*color)
            && tex_coord[0].is_finite()
            && tex_coord[1].is_finite()
    }) || !rect_size.iter().all(|value| value.is_finite())
        || !corner_radius.is_finite()
    {
        return None;
    }

    let mut points = [
        ((points[0].0.x, points[0].0.y), points[0].1, points[0].2),
        ((points[1].0.x, points[1].0.y), points[1].1, points[1].2),
        ((points[2].0.x, points[2].0.y), points[2].1, points[2].2),
    ];
    let area = signed_triangle_area(points[0].0, points[1].0, points[2].0);
    if !area.is_finite() || area.abs() <= MIN_TRIANGLE_AREA_NDC {
        return None;
    }
    if area < 0.0 {
        points.swap(1, 2);
    }

    Some(points.map(|((x, y), color, tex_coord)| {
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
    }))
}

fn signed_triangle_area(a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> f32 {
    (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0)
}

fn rect_is_finite(rect: Rect) -> bool {
    rect.x.is_finite() && rect.y.is_finite() && rect.width.is_finite() && rect.height.is_finite()
}

fn rect_is_visible(rect: Rect) -> bool {
    rect_is_finite(rect) && rect.width > 0.0 && rect.height > 0.0
}

fn point_is_finite(point: Point) -> bool {
    point.x.is_finite() && point.y.is_finite()
}

fn color_is_finite(color: mondrian_core::Color) -> bool {
    color.r.is_finite() && color.g.is_finite() && color.b.is_finite() && color.a.is_finite()
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

fn intersect_rect(a: Rect, b: Rect) -> Rect {
    let x0 = a.x.max(b.x);
    let y0 = a.y.max(b.y);
    let x1 = (a.x + a.width).min(b.x + b.width);
    let y1 = (a.y + a.height).min(b.y + b.height);
    Rect::new(x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0))
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
fn line_signed_distance_px(local: [f32; 2], rect_size: [f32; 2], radius: f32) -> f32 {
    let center_y = rect_size[1] * 0.5;
    let axis_padding = center_y;
    let a = [axis_padding, center_y];
    let b = [(rect_size[0] - axis_padding).max(axis_padding), center_y];
    let pa = [local[0] - a[0], local[1] - a[1]];
    let ba = [b[0] - a[0], b[1] - a[1]];
    let denom = (ba[0] * ba[0] + ba[1] * ba[1]).max(0.000001);
    let h = ((pa[0] * ba[0] + pa[1] * ba[1]) / denom).clamp(0.0, 1.0);
    let dx = pa[0] - ba[0] * h;
    let dy = pa[1] - ba[1] * h;
    (dx * dx + dy * dy).sqrt() - radius
}

#[cfg(test)]
fn line_axis_padding_px(radius: f32) -> f32 {
    radius + LINE_AA_PADDING_PX
}

#[cfg(test)]
fn line_alpha_from_signed_distance(d: f32) -> f32 {
    let aa = LINE_SHADER_AA_MIN_PX;
    smoothstep(
        aa * LINE_SHADER_AA_EDGE_SCALE,
        -aa * LINE_SHADER_AA_EDGE_SCALE,
        d,
    )
}

#[cfg(test)]
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
fn line_local_point(point: Point, start: Point, end: Point, radius: f32) -> ([f32; 2], [f32; 2]) {
    let dx = end.x - start.x;
    let dy = end.y - start.y;
    let len = dx.hypot(dy);
    let (ux, uy) = if len > MIN_LINE_DIRECTION_LEN {
        (dx / len, dy / len)
    } else {
        (1.0, 0.0)
    };
    let nx = -uy;
    let ny = ux;
    let axis_padding = line_axis_padding_px(radius);
    let origin = Point::new(
        start.x - ux * axis_padding - nx * axis_padding,
        start.y - uy * axis_padding - ny * axis_padding,
    );
    let rel_x = point.x - origin.x;
    let rel_y = point.y - origin.y;
    (
        [rel_x * ux + rel_y * uy, rel_x * nx + rel_y * ny],
        [len + axis_padding * 2.0, axis_padding * 2.0],
    )
}

#[cfg(test)]
fn ndc_from_pixel(point: Point, screen_size: (u32, u32)) -> (f32, f32) {
    (
        point.x * (2.0 / screen_size.0 as f32) - 1.0,
        point.y * (-2.0 / screen_size.1 as f32) + 1.0,
    )
}

#[cfg(test)]
fn barycentric_weights(
    p: (f32, f32),
    a: (f32, f32),
    b: (f32, f32),
    c: (f32, f32),
) -> Option<[f32; 3]> {
    let denom = (b.1 - c.1) * (a.0 - c.0) + (c.0 - b.0) * (a.1 - c.1);
    if denom.abs() <= f32::EPSILON {
        return None;
    }
    let w0 = ((b.1 - c.1) * (p.0 - c.0) + (c.0 - b.0) * (p.1 - c.1)) / denom;
    let w1 = ((c.1 - a.1) * (p.0 - c.0) + (a.0 - c.0) * (p.1 - c.1)) / denom;
    let w2 = 1.0 - w0 - w1;
    let epsilon = -0.0001;
    (w0 >= epsilon && w1 >= epsilon && w2 >= epsilon).then_some([w0, w1, w2])
}

#[cfg(test)]
fn sample_line_alpha_from_vertices(
    vertices: &[RectVertex],
    screen_size: (u32, u32),
    point: Point,
) -> f32 {
    let ndc = ndc_from_pixel(point, screen_size);
    vertices
        .chunks_exact(3)
        .filter_map(|tri| {
            let a = (tri[0].position[0], tri[0].position[1]);
            let b = (tri[1].position[0], tri[1].position[1]);
            let c = (tri[2].position[0], tri[2].position[1]);
            let weights = barycentric_weights(ndc, a, b, c)?;
            let local = [
                tri[0].tex_coord[0] * weights[0]
                    + tri[1].tex_coord[0] * weights[1]
                    + tri[2].tex_coord[0] * weights[2],
                tri[0].tex_coord[1] * weights[0]
                    + tri[1].tex_coord[1] * weights[1]
                    + tri[2].tex_coord[1] * weights[2],
            ];
            let d = line_signed_distance_px(local, tri[0].rect_size, tri[0].corner_radius_px);
            Some(line_alpha_from_signed_distance(d))
        })
        .fold(0.0, f32::max)
}

#[cfg(test)]
fn rounded_rect_signed_distance_px(local: [f32; 2], rect_size: [f32; 2], radius: f32) -> f32 {
    let radius = radius.clamp(0.0, rect_size[0].min(rect_size[1]) * 0.5);
    let half = [rect_size[0] * 0.5, rect_size[1] * 0.5];
    let q = [
        (local[0] - half[0]).abs() - half[0] + radius,
        (local[1] - half[1]).abs() - half[1] + radius,
    ];
    let outside = [q[0].max(0.0), q[1].max(0.0)];
    (outside[0] * outside[0] + outside[1] * outside[1]).sqrt() + q[0].max(q[1]).min(0.0) - radius
}

#[cfg(test)]
fn sample_shape_signed_distance_from_vertices(
    vertices: &[RectVertex],
    screen_size: (u32, u32),
    point: Point,
) -> Option<f32> {
    let ndc = ndc_from_pixel(point, screen_size);
    vertices.chunks_exact(3).find_map(|tri| {
        let a = (tri[0].position[0], tri[0].position[1]);
        let b = (tri[1].position[0], tri[1].position[1]);
        let c = (tri[2].position[0], tri[2].position[1]);
        let weights = barycentric_weights(ndc, a, b, c)?;
        let local = [
            (tri[0].tex_coord[0] * weights[0]
                + tri[1].tex_coord[0] * weights[1]
                + tri[2].tex_coord[0] * weights[2])
                * tri[0].rect_size[0],
            (tri[0].tex_coord[1] * weights[0]
                + tri[1].tex_coord[1] * weights[1]
                + tri[2].tex_coord[1] * weights[2])
                * tri[0].rect_size[1],
        ];
        Some(rounded_rect_signed_distance_px(
            local,
            tri[0].rect_size,
            tri[0].corner_radius_px,
        ))
    })
}

#[cfg(test)]
fn assert_line_coverage_connects_caps(
    vertices: &[RectVertex],
    screen_size: (u32, u32),
    start: Point,
    end: Point,
) {
    let dx = end.x - start.x;
    let dy = end.y - start.y;
    let len = dx.hypot(dy);
    let (ux, uy) = if len > MIN_LINE_DIRECTION_LEN {
        (dx / len, dy / len)
    } else {
        (1.0, 0.0)
    };
    let min_x = start.x.min(end.x).floor().max(0.0) as i32 - 3;
    let max_x = start.x.max(end.x).ceil().min(screen_size.0 as f32 - 1.0) as i32 + 3;
    let min_y = start.y.min(end.y).floor().max(0.0) as i32 - 3;
    let max_y = start.y.max(end.y).ceil().min(screen_size.1 as f32 - 1.0) as i32 + 3;
    let width = (max_x - min_x + 1).max(0) as usize;
    let height = (max_y - min_y + 1).max(0) as usize;
    let mut visible = vec![false; width * height];
    let mut start_seed = None;
    let mut end_pixels = vec![false; width * height];

    for gy in 0..height {
        for gx in 0..width {
            let x = min_x + gx as i32;
            let y = min_y + gy as i32;
            let pixel_center = Point::new(x as f32 + 0.5, y as f32 + 0.5);
            let alpha = sample_line_alpha_from_vertices(vertices, screen_size, pixel_center);
            if alpha < 0.1 {
                continue;
            }

            let idx = gy * width + gx;
            visible[idx] = true;
            let rel_x = pixel_center.x - start.x;
            let rel_y = pixel_center.y - start.y;
            let projected = rel_x * ux + rel_y * uy;
            if projected <= 1.5 {
                start_seed.get_or_insert(idx);
            }
            if projected >= len - 1.5 {
                end_pixels[idx] = true;
            }
        }
    }

    let seed = start_seed.expect("line coverage should include the start cap");
    assert!(
        end_pixels.iter().any(|is_end| *is_end),
        "line coverage should include the end cap"
    );

    let mut visited = vec![false; width * height];
    let mut queue = std::collections::VecDeque::from([seed]);
    visited[seed] = true;

    while let Some(idx) = queue.pop_front() {
        if end_pixels[idx] {
            return;
        }

        let gx = idx % width;
        let gy = idx / width;
        for oy in -1_i32..=1 {
            for ox in -1_i32..=1 {
                if ox == 0 && oy == 0 {
                    continue;
                }
                let nx = gx as i32 + ox;
                let ny = gy as i32 + oy;
                if nx < 0 || ny < 0 || nx >= width as i32 || ny >= height as i32 {
                    continue;
                }
                let next = ny as usize * width + nx as usize;
                if visible[next] && !visited[next] {
                    visited[next] = true;
                    queue.push_back(next);
                }
            }
        }
    }

    panic!("line coverage should form an 8-connected visible path from start cap to end cap");
}

#[cfg(test)]
fn strongest_line_alpha_around(
    vertices: &[RectVertex],
    screen_size: (u32, u32),
    center: Point,
    normal: (f32, f32),
) -> f32 {
    let mut strongest_alpha = 0.0_f32;

    for normal_offset in [-0.5_f32, -0.25, 0.0, 0.25, 0.5] {
        let sample = Point::new(
            center.x + normal.0 * normal_offset,
            center.y + normal.1 * normal_offset,
        );
        let base_x = sample.x.floor() as i32;
        let base_y = sample.y.floor() as i32;

        for y in (base_y - 1)..=(base_y + 1) {
            for x in (base_x - 1)..=(base_x + 1) {
                let pixel_center = Point::new(x as f32 + 0.5, y as f32 + 0.5);
                strongest_alpha = strongest_alpha.max(sample_line_alpha_from_vertices(
                    vertices,
                    screen_size,
                    pixel_center,
                ));
            }
        }
    }

    strongest_alpha
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
    fn build_batches_zero_sized_surface_returns_empty() {
        let cmds = [DrawCommand::Rect {
            bounds: Rect::new(0.0, 0.0, 100.0, 50.0),
            color: Color::WHITE,
            corner_radius: 0.0,
        }];

        assert!(build_batches(&cmds, (0, 100)).is_empty());
        assert!(build_batches(&cmds, (100, 0)).is_empty());
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
    fn build_batches_rects_skip_invalid_geometry_and_colors() {
        let cmds = [
            DrawCommand::Rect {
                bounds: Rect::new(0.0, 0.0, 20.0, 20.0),
                color: Color::WHITE,
                corner_radius: 0.0,
            },
            DrawCommand::Rect {
                bounds: Rect::new(f32::NAN, 0.0, 20.0, 20.0),
                color: Color::WHITE,
                corner_radius: 0.0,
            },
            DrawCommand::Rect {
                bounds: Rect::new(0.0, 0.0, 0.0, 20.0),
                color: Color::WHITE,
                corner_radius: 0.0,
            },
            DrawCommand::Rect {
                bounds: Rect::new(0.0, 0.0, 20.0, 20.0),
                color: Color { r: 1.0, g: 1.0, b: f32::INFINITY, a: 1.0 },
                corner_radius: 0.0,
            },
        ];

        let batches = build_batches(&cmds, (100, 100));

        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].vertices.len(), 6);
        for vertex in &batches[0].vertices {
            assert!(vertex.position.iter().all(|value| value.is_finite()));
            assert!(vertex.color.iter().all(|value| value.is_finite()));
        }
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
    fn build_batches_gradient_rects_skip_invalid_inputs() {
        let invalid_colors = [
            Color::WHITE,
            Color::BLACK,
            Color { r: f32::NAN, g: 0.0, b: 0.0, a: 1.0 },
            Color::TRANSPARENT,
        ];
        let cmds = [
            DrawCommand::GradientRect {
                bounds: Rect::new(0.0, 0.0, 20.0, 20.0),
                colors: [Color::WHITE, Color::BLACK, Color::TRANSPARENT, Color::WHITE],
                corner_radius: 0.0,
            },
            DrawCommand::GradientRect {
                bounds: Rect::new(0.0, 0.0, -1.0, 20.0),
                colors: [Color::WHITE, Color::BLACK, Color::TRANSPARENT, Color::WHITE],
                corner_radius: 0.0,
            },
            DrawCommand::GradientRect {
                bounds: Rect::new(0.0, 0.0, 20.0, 20.0),
                colors: invalid_colors,
                corner_radius: 0.0,
            },
        ];

        let batches = build_batches(&cmds, (100, 100));

        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].vertices.len(), 6);
        assert!(batches[0]
            .vertices
            .iter()
            .all(|vertex| vertex.color.iter().all(|value| value.is_finite())));
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
    fn build_batches_diagonal_hairline_uses_analytic_line_sdf_data() {
        let cmds = [DrawCommand::Line {
            start: Point::new(8.0, 8.0),
            end: Point::new(56.0, 56.0),
            width: 1.0,
            color: Color::WHITE,
        }];

        let batches = build_batches(&cmds, (64, 64));

        assert_eq!(batches.len(), 1);
        let vertices = &batches[0].vertices;
        assert_eq!(vertices.len(), 6);
        for vertex in vertices {
            assert_eq!(vertex.render_mode, RenderMode::Line as u32);
            assert!(
                vertex.rect_size[1] > 1.0,
                "line quad must include AA padding beyond the requested stroke width"
            );
            assert!(
                vertex.corner_radius_px >= 0.5,
                "line shader must receive the stroke radius for round caps"
            );
        }
        assert!(
            vertices.iter().any(|vertex| vertex.tex_coord[0] > vertex.rect_size[0] - 0.01),
            "line vertices must carry local-pixel coordinates for the fragment SDF"
        );
    }

    #[test]
    fn analytic_line_sdf_keeps_45_degree_centerline_continuous() {
        let radius = 0.5;
        let axis_padding = line_axis_padding_px(radius);
        let rect_size = [48.0_f32.hypot(48.0) + axis_padding * 2.0, 3.0];
        for step in 0..=48 {
            let local_x = axis_padding + step as f32 * 2.0_f32.sqrt();
            let d = line_signed_distance_px([local_x, rect_size[1] * 0.5], rect_size, radius);
            assert!(d <= -0.49, "centerline step {step} has weak coverage d={d}");
        }
    }

    #[test]
    fn analytic_line_sdf_keeps_45_degree_pixel_centers_visible() {
        let radius = 0.5;
        let start = Point::new(8.0, 8.0);
        let end = Point::new(56.0, 56.0);
        let direction = 1.0 / 2.0_f32.sqrt();
        let normal = (-direction, direction);

        for normal_offset in [-0.5_f32, -0.25, 0.0, 0.25, 0.5] {
            for step in 0..=48 {
                let center = Point::new(
                    start.x + step as f32 + normal.0 * normal_offset,
                    start.y + step as f32 + normal.1 * normal_offset,
                );
                let base_x = center.x.floor() as i32;
                let base_y = center.y.floor() as i32;
                let mut strongest_alpha = 0.0_f32;

                for y in (base_y - 1)..=(base_y + 1) {
                    for x in (base_x - 1)..=(base_x + 1) {
                        let pixel_center = Point::new(x as f32 + 0.5, y as f32 + 0.5);
                        let (local, rect_size) = line_local_point(pixel_center, start, end, radius);
                        let d = line_signed_distance_px(local, rect_size, radius);
                        strongest_alpha = strongest_alpha.max(line_alpha_from_signed_distance(d));
                    }
                }

                assert!(
                    strongest_alpha >= 0.5,
                    "45 degree line has weak pixel coverage at offset {normal_offset}, step {step}: alpha {strongest_alpha}"
                );
            }
        }
    }

    #[test]
    fn batch_line_vertices_keep_primary_angle_subpixel_pixel_coverage() {
        let screen_size = (120, 120);
        let length = 40.0_f32;

        for angle in [
            0.0_f32, 15.0, 30.0, 45.0, 60.0, 75.0, 90.0, 135.0, 180.0, 225.0, 270.0, 315.0,
        ] {
            let radians = angle.to_radians();
            let unit = (radians.cos(), radians.sin());
            let normal = (-unit.1, unit.0);

            for phase in [0.0_f32, 0.25, 0.5, 0.75] {
                let start = Point::new(60.0 + phase, 60.0 + phase);
                let end = Point::new(start.x + unit.0 * length, start.y + unit.1 * length);
                let cmds = [DrawCommand::Line { start, end, width: 1.0, color: Color::WHITE }];
                let batches = build_batches(&cmds, screen_size);

                assert!(
                    batches.len() == 1,
                    "angle {angle} phase {phase} should produce one line batch"
                );
                let vertices = &batches[0].vertices;

                for step in 0..=length as i32 {
                    let center = Point::new(
                        start.x + unit.0 * step as f32,
                        start.y + unit.1 * step as f32,
                    );
                    let mut strongest_alpha = 0.0_f32;

                    for normal_offset in [-0.5_f32, -0.25, 0.0, 0.25, 0.5] {
                        let sample = Point::new(
                            center.x + normal.0 * normal_offset,
                            center.y + normal.1 * normal_offset,
                        );
                        let base_x = sample.x.floor() as i32;
                        let base_y = sample.y.floor() as i32;

                        for y in (base_y - 1)..=(base_y + 1) {
                            for x in (base_x - 1)..=(base_x + 1) {
                                let pixel_center = Point::new(x as f32 + 0.5, y as f32 + 0.5);
                                strongest_alpha =
                                    strongest_alpha.max(sample_line_alpha_from_vertices(
                                        vertices,
                                        screen_size,
                                        pixel_center,
                                    ));
                            }
                        }
                    }

                    assert!(
                        strongest_alpha >= 0.5,
                        "angle {angle} line has weak batch-generated pixel coverage at phase {phase}, step {step}: alpha {strongest_alpha}"
                    );
                }
            }
        }
    }

    #[test]
    fn batch_line_vertices_keep_45_degree_hairline_alpha_profile_stable() {
        let screen_size = (128, 128);
        let cases = [
            (Point::new(12.0, 12.0), Point::new(84.0, 84.0)),
            (Point::new(12.25, 12.75), Point::new(84.25, 84.75)),
            (Point::new(12.5, 12.0), Point::new(84.5, 84.0)),
            (Point::new(12.0, 84.0), Point::new(84.0, 12.0)),
            (Point::new(12.75, 84.25), Point::new(84.75, 12.25)),
        ];

        for (start, end) in cases {
            let cmds = [DrawCommand::Line { start, end, width: 1.0, color: Color::WHITE }];
            let batches = build_batches(&cmds, screen_size);
            assert_eq!(batches.len(), 1);

            let dx = end.x - start.x;
            let dy = end.y - start.y;
            let len = dx.hypot(dy);
            let unit = (dx / len, dy / len);
            let normal = (-unit.1, unit.0);
            let vertices = &batches[0].vertices;
            let mut previous_alpha = None;

            for step in 0..=len.round() as i32 {
                let center = Point::new(
                    start.x + unit.0 * step as f32,
                    start.y + unit.1 * step as f32,
                );
                let alpha = strongest_line_alpha_around(vertices, screen_size, center, normal);
                assert!(
                    alpha >= 0.55,
                    "45 degree hairline has a weak alpha sample for {start:?}->{end:?} at step {step}: {alpha}"
                );

                if let Some(previous) = previous_alpha {
                    let delta = f32::abs(previous - alpha);
                    assert!(
                        delta <= 0.55,
                        "45 degree hairline alpha jumps too abruptly for {start:?}->{end:?} at step {step}: previous={previous}, current={alpha}"
                    );
                }
                previous_alpha = Some(alpha);
            }
        }
    }

    #[test]
    fn batch_line_vertices_keep_45_degree_hairline_coverage_connected() {
        let screen_size = (96, 96);
        let cases = [
            (Point::new(12.0, 12.0), Point::new(60.0, 60.0)),
            (Point::new(12.25, 12.0), Point::new(60.25, 60.0)),
            (Point::new(12.0, 12.25), Point::new(60.0, 60.25)),
            (Point::new(12.5, 12.5), Point::new(60.5, 60.5)),
            (Point::new(12.25, 12.75), Point::new(60.25, 60.75)),
            (Point::new(12.0, 60.0), Point::new(60.0, 12.0)),
            (Point::new(12.5, 60.25), Point::new(60.5, 12.25)),
        ];

        for (start, end) in cases {
            let cmds = [DrawCommand::Line { start, end, width: 1.0, color: Color::WHITE }];
            let batches = build_batches(&cmds, screen_size);
            assert_eq!(batches.len(), 1);
            assert_line_coverage_connects_caps(&batches[0].vertices, screen_size, start, end);
        }
    }

    #[test]
    fn analytic_line_sdf_keeps_centerline_continuous_for_primary_angles() {
        let radius = 0.5;
        let angles = [
            0.0_f32, 15.0, 30.0, 45.0, 60.0, 75.0, 90.0, 135.0, 180.0, 225.0, 270.0, 315.0,
        ];

        for angle in angles {
            let radians = angle.to_radians();
            let dx = radians.cos() * 56.0;
            let dy = radians.sin() * 56.0;
            let len = dx.hypot(dy);
            let axis_padding = line_axis_padding_px(radius);
            let rect_size = [len + axis_padding * 2.0, 3.0];

            for step in 0..=56 {
                let local_x = axis_padding + step as f32;
                let d = line_signed_distance_px([local_x, rect_size[1] * 0.5], rect_size, radius);
                assert!(
                    d <= -0.49,
                    "angle {angle} centerline step {step} has weak coverage d={d}"
                );
            }
        }
    }

    #[test]
    fn build_batches_hairline_angles_keep_front_faces_and_local_line_space() {
        let angles = [
            0.0_f32, 15.0, 30.0, 45.0, 60.0, 75.0, 90.0, 135.0, 180.0, 225.0, 270.0, 315.0,
        ];

        for angle in angles {
            let radians = angle.to_radians();
            let start = Point::new(80.0, 80.0);
            let end = Point::new(
                start.x + radians.cos() * 48.0,
                start.y + radians.sin() * 48.0,
            );
            let cmds = [DrawCommand::Line { start, end, width: 1.0, color: Color::WHITE }];

            let batches = build_batches(&cmds, (200, 200));

            assert_eq!(batches.len(), 1, "angle {angle} should produce one batch");
            let vertices = &batches[0].vertices;
            assert_eq!(
                vertices.len(),
                6,
                "angle {angle} should expand to two triangles"
            );
            for tri in vertices.chunks_exact(3) {
                let a = (tri[0].position[0], tri[0].position[1]);
                let b = (tri[1].position[0], tri[1].position[1]);
                let c = (tri[2].position[0], tri[2].position[1]);
                assert!(
                    signed_triangle_area(a, b, c) > 0.0,
                    "angle {angle} generated a back-facing line triangle"
                );
            }
            for vertex in vertices {
                assert_eq!(vertex.render_mode, RenderMode::Line as u32);
                assert!(
                    vertex.position.iter().all(|value| value.is_finite()),
                    "angle {angle} generated non-finite position"
                );
                assert!(
                    vertex.tex_coord[0] >= 0.0
                        && vertex.tex_coord[0] <= vertex.rect_size[0]
                        && vertex.tex_coord[1] >= 0.0
                        && vertex.tex_coord[1] <= vertex.rect_size[1],
                    "angle {angle} generated local coordinates outside line bounds"
                );
                assert!(
                    (vertex.rect_size[1] - (1.0 + LINE_AA_PADDING_PX * 2.0)).abs() < 0.001,
                    "angle {angle} should keep 1px stroke plus conservative AA padding per side"
                );
                assert!((vertex.corner_radius_px - 0.5).abs() < 0.001);
            }
        }
    }

    #[test]
    fn analytic_line_sdf_preserves_round_caps_for_zero_length_lines() {
        let radius = 2.0;
        let axis_padding = line_axis_padding_px(radius);
        let rect_size = [axis_padding * 2.0, radius * 2.0 + LINE_AA_PADDING_PX * 2.0];
        let center = [axis_padding, rect_size[1] * 0.5];
        assert!((line_signed_distance_px(center, rect_size, radius) + radius).abs() < 0.001);
        let cap_edge = [axis_padding + radius, rect_size[1] * 0.5];
        assert!(line_signed_distance_px(cap_edge, rect_size, radius).abs() < 0.001);
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
    fn build_batches_masked_colored_triangles_normalize_winding_with_vertex_data() {
        let cmds = [DrawCommand::ColoredTriangles {
            vertices: vec![
                (Point::new(10.0, 10.0), Color::from_rgba8(255, 0, 0, 255)),
                (Point::new(90.0, 10.0), Color::from_rgba8(0, 255, 0, 255)),
                (Point::new(10.0, 90.0), Color::from_rgba8(0, 0, 255, 255)),
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
        let tri = &vertices[..3];
        let a = (tri[0].position[0], tri[0].position[1]);
        let b = (tri[1].position[0], tri[1].position[1]);
        let c = (tri[2].position[0], tri[2].position[1]);
        assert!(signed_triangle_area(a, b, c) > 0.0);
        assert_eq!(tri[0].color, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(tri[0].tex_coord, [0.0, 0.0]);
        assert_eq!(tri[1].color, [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(tri[1].tex_coord, [0.0, 1.0]);
        assert_eq!(tri[2].color, [0.0, 1.0, 0.0, 1.0]);
        assert_eq!(tri[2].tex_coord, [1.0, 0.0]);
    }

    #[test]
    fn build_batches_masked_colored_triangles_reconstruct_mask_sdf_from_vertices() {
        let mask = crate::command::ShapeMask {
            bounds: Rect::new(20.0, 20.0, 60.0, 60.0),
            corner_radius: 30.0,
        };
        let cmds = [DrawCommand::ColoredTriangles {
            vertices: vec![
                (Point::new(20.0, 20.0), Color::from_rgba8(255, 0, 0, 255)),
                (Point::new(80.0, 20.0), Color::from_rgba8(0, 255, 0, 255)),
                (Point::new(20.0, 80.0), Color::from_rgba8(0, 0, 255, 255)),
                (Point::new(80.0, 20.0), Color::from_rgba8(0, 255, 0, 255)),
                (
                    Point::new(80.0, 80.0),
                    Color::from_rgba8(255, 255, 255, 255),
                ),
                (Point::new(20.0, 80.0), Color::from_rgba8(0, 0, 255, 255)),
            ],
            mask: Some(mask),
        }];
        let batches = build_batches(&cmds, (100, 100));

        assert_eq!(batches.len(), 1);
        let vertices = &batches[0].vertices;
        assert_eq!(vertices.len(), 6);
        for tri in vertices.chunks_exact(3) {
            let a = (tri[0].position[0], tri[0].position[1]);
            let b = (tri[1].position[0], tri[1].position[1]);
            let c = (tri[2].position[0], tri[2].position[1]);
            assert!(signed_triangle_area(a, b, c) > 0.0);
        }

        let center = mask.bounds.center();
        let center_d = sample_shape_signed_distance_from_vertices(vertices, (100, 100), center)
            .expect("masked colored triangle center should be covered by final triangles");
        assert!(
            (center_d + mask.corner_radius).abs() < 0.001,
            "masked triangle center signed distance should match circle radius, got {center_d}"
        );

        for angle in [0.0_f32, 45.0, 90.0, 135.0, 180.0, 225.0, 270.0, 315.0] {
            let radians = angle.to_radians();
            let boundary = Point::new(
                center.x + mask.corner_radius * radians.cos(),
                center.y + mask.corner_radius * radians.sin(),
            );
            let d = sample_shape_signed_distance_from_vertices(vertices, (100, 100), boundary)
                .unwrap_or_else(|| {
                    panic!("masked colored triangle boundary at {angle} degrees missed triangles")
                });
            assert!(
                d.abs() < 0.01,
                "masked colored triangle boundary at {angle} degrees produced signed distance {d}"
            );
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

    #[test]
    fn build_batches_nested_clips_intersect_parent_and_child() {
        let cmds = [
            DrawCommand::PushClip { bounds: Rect::new(20.0, 30.0, 100.0, 80.0) },
            DrawCommand::PushClip { bounds: Rect::new(0.0, 0.0, 200.0, 48.0) },
            DrawCommand::Image {
                bounds: Rect::new(0.0, 0.0, 200.0, 48.0),
                uv_rect: Rect::new(0.0, 0.0, 1.0, 1.0),
                tint: Color::WHITE,
            },
            DrawCommand::PopClip,
            DrawCommand::PopClip,
        ];

        let batches = build_batches(&cmds, (300, 200));

        assert_eq!(batches.len(), 1);
        assert_eq!(
            batches[0].clip_rect,
            Some(Rect::new(20.0, 30.0, 100.0, 18.0))
        );
    }

    #[test]
    fn build_batches_disjoint_nested_clip_keeps_empty_effective_clip() {
        let cmds = [
            DrawCommand::PushClip { bounds: Rect::new(20.0, 30.0, 100.0, 80.0) },
            DrawCommand::PushClip { bounds: Rect::new(0.0, 0.0, 10.0, 10.0) },
            DrawCommand::Rect {
                bounds: Rect::new(0.0, 0.0, 10.0, 10.0),
                color: Color::WHITE,
                corner_radius: 0.0,
            },
            DrawCommand::PopClip,
            DrawCommand::PopClip,
        ];

        let batches = build_batches(&cmds, (300, 200));

        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].clip_rect, Some(Rect::new(20.0, 30.0, 0.0, 0.0)));
    }

    #[test]
    fn build_batches_invalid_clip_becomes_empty_clip() {
        let cmds = [
            DrawCommand::PushClip { bounds: Rect::new(f32::NAN, 0.0, 100.0, 80.0) },
            DrawCommand::Rect {
                bounds: Rect::new(0.0, 0.0, 10.0, 10.0),
                color: Color::WHITE,
                corner_radius: 0.0,
            },
            DrawCommand::PopClip,
        ];

        let batches = build_batches(&cmds, (300, 200));

        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].clip_rect, Some(Rect::new(0.0, 0.0, 0.0, 0.0)));
        assert!(batches[0].vertices.iter().all(|vertex| {
            vertex.position.iter().all(|value| value.is_finite())
                && vertex.color.iter().all(|value| value.is_finite())
        }));
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

    #[test]
    fn build_batches_invalid_transform_preserves_stack_balance_without_moving_geometry() {
        let cmds = [
            DrawCommand::PushTranslate { offset: glam::Vec2::new(50.0, 0.0) },
            DrawCommand::PushTranslate { offset: glam::Vec2::new(f32::NAN, 10.0) },
            DrawCommand::Rect {
                bounds: Rect::new(0.0, 0.0, 100.0, 100.0),
                color: Color::WHITE,
                corner_radius: 0.0,
            },
            DrawCommand::PopTransform,
            DrawCommand::Rect {
                bounds: Rect::new(0.0, 0.0, 100.0, 100.0),
                color: Color::BLACK,
                corner_radius: 0.0,
            },
            DrawCommand::PopTransform,
        ];

        let batches = build_batches(&cmds, (1000, 1000));

        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].vertices.len(), 12);
        let first_rect_x = batches[0].vertices[0].position[0];
        let second_rect_x = batches[0].vertices[6].position[0];
        assert!((first_rect_x - second_rect_x).abs() < 0.0001);
    }

    #[test]
    fn build_batches_translated_clip_uses_screen_space_effective_bounds() {
        let cmds = [
            DrawCommand::Rect {
                bounds: Rect::new(0.0, 0.0, 10.0, 10.0),
                color: Color::WHITE,
                corner_radius: 0.0,
            },
            DrawCommand::PushTranslate { offset: glam::Vec2::new(40.0, 30.0) },
            DrawCommand::PushClip { bounds: Rect::new(5.0, 7.0, 20.0, 11.0) },
            DrawCommand::Rect {
                bounds: Rect::new(0.0, 0.0, 100.0, 100.0),
                color: Color::BLACK,
                corner_radius: 0.0,
            },
            DrawCommand::PopClip,
            DrawCommand::PopTransform,
            DrawCommand::Rect {
                bounds: Rect::new(70.0, 80.0, 10.0, 10.0),
                color: Color::WHITE,
                corner_radius: 0.0,
            },
        ];

        let batches = build_batches(&cmds, (200, 200));

        assert_eq!(batches.len(), 3);
        assert_eq!(batches[0].clip_rect, None);
        assert_eq!(
            batches[1].clip_rect,
            Some(Rect::new(45.0, 37.0, 20.0, 11.0))
        );
        assert_eq!(batches[2].clip_rect, None);
        assert_eq!(batches[0].vertices.len(), 6);
        assert_eq!(batches[1].vertices.len(), 6);
        assert_eq!(batches[2].vertices.len(), 6);
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
    fn build_batches_line_uses_stable_round_caps_with_aa_padding() {
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

        let half_geometry_width = 2.0 + LINE_AA_PADDING_PX;
        let expected_min_x = (10.0 - half_geometry_width) * 0.02 - 1.0;
        let expected_max_x = (90.0 + half_geometry_width) * 0.02 - 1.0;
        assert!((min_x - expected_min_x).abs() < 0.001);
        assert!((max_x - expected_max_x).abs() < 0.001);
    }

    #[test]
    fn build_batches_zero_length_wide_line_bounds_include_round_cap() {
        let cmds = [DrawCommand::Line {
            start: Point::new(50.0, 50.0),
            end: Point::new(50.0, 50.0),
            width: 4.0,
            color: Color::WHITE,
        }];
        let batches = build_batches(&cmds, (100, 100));

        assert_eq!(batches.len(), 1);
        let vertices = &batches[0].vertices;
        let min_x = vertices.iter().map(|v| v.position[0]).fold(f32::INFINITY, f32::min);
        let max_x = vertices.iter().map(|v| v.position[0]).fold(f32::NEG_INFINITY, f32::max);
        let min_y = vertices.iter().map(|v| v.position[1]).fold(f32::INFINITY, f32::min);
        let max_y = vertices.iter().map(|v| v.position[1]).fold(f32::NEG_INFINITY, f32::max);

        let half_geometry_width = 2.0 + LINE_AA_PADDING_PX;
        let expected_min = (50.0 - half_geometry_width) * 0.02 - 1.0;
        let expected_max = (50.0 + half_geometry_width) * 0.02 - 1.0;
        assert!((min_x - expected_min).abs() < 0.001);
        assert!((max_x - expected_max).abs() < 0.001);
        assert!((min_y - (-expected_max)).abs() < 0.001);
        assert!((max_y - (-expected_min)).abs() < 0.001);
        for vertex in vertices {
            assert!((vertex.rect_size[0] - (4.0 + LINE_AA_PADDING_PX * 2.0)).abs() < 0.001);
            assert!((vertex.rect_size[1] - (4.0 + LINE_AA_PADDING_PX * 2.0)).abs() < 0.001);
            assert!((vertex.corner_radius_px - 2.0).abs() < 0.001);
        }
    }

    #[test]
    fn build_batches_lines_skip_invalid_color() {
        let cmds = [DrawCommand::Line {
            start: Point::new(10.0, 50.0),
            end: Point::new(90.0, 50.0),
            width: 2.0,
            color: Color { r: 1.0, g: f32::NAN, b: 1.0, a: 1.0 },
        }];

        assert!(build_batches(&cmds, (100, 100)).is_empty());
    }

    #[test]
    fn build_batches_lines_skip_overflowing_length() {
        let cmds = [DrawCommand::Line {
            start: Point::new(-f32::MAX, 50.0),
            end: Point::new(f32::MAX, 50.0),
            width: 1.0,
            color: Color::WHITE,
        }];

        assert!(build_batches(&cmds, (100, 100)).is_empty());
    }

    #[test]
    fn build_batches_lines_skip_overflowing_geometry() {
        let cmds = [DrawCommand::Line {
            start: Point::new(10.0, 10.0),
            end: Point::new(11.0, 10.0),
            width: f32::MAX,
            color: Color::WHITE,
        }];

        assert!(build_batches(&cmds, (100, 100)).is_empty());
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

    #[test]
    fn build_batches_triangles_preserve_subpixel_vertices_after_winding_normalization() {
        let cmds = [DrawCommand::Triangles {
            vertices: vec![
                Point::new(10.25, 12.5),
                Point::new(22.75, 74.25),
                Point::new(90.5, 20.125),
            ],
            color: Color::from_rgba8(51, 102, 153, 204),
        }];

        let batches = build_batches(&cmds, (100, 100));

        assert_eq!(batches.len(), 1);
        let vertices = &batches[0].vertices;
        assert_eq!(vertices.len(), 3);
        let tri = &vertices[..3];
        let a = (tri[0].position[0], tri[0].position[1]);
        let b = (tri[1].position[0], tri[1].position[1]);
        let c = (tri[2].position[0], tri[2].position[1]);
        assert!(signed_triangle_area(a, b, c) > 0.0);
        for vertex in tri {
            assert_eq!(vertex.render_mode, RenderMode::Shape as u32);
            assert_eq!(vertex.rect_size, [1.0, 1.0]);
            assert_eq!(vertex.corner_radius_px, 0.0);
            assert_eq!(vertex.tex_coord, [0.0, 0.0]);
            assert_eq!(vertex.color, [0.2, 0.4, 0.6, 0.8]);
        }

        let actual = tri
            .iter()
            .map(|vertex| {
                Point::new(
                    (vertex.position[0] + 1.0) * 50.0,
                    (1.0 - vertex.position[1]) * 50.0,
                )
            })
            .collect::<Vec<_>>();
        let expected = [
            Point::new(10.25, 12.5),
            Point::new(22.75, 74.25),
            Point::new(90.5, 20.125),
        ];
        for expected_point in expected {
            assert!(
                actual.iter().any(|point| {
                    (point.x - expected_point.x).abs() < 0.001
                        && (point.y - expected_point.y).abs() < 0.001
                }),
                "subpixel triangle vertex {expected_point:?} was not preserved in {actual:?}"
            );
        }
    }

    #[test]
    fn build_batches_triangles_skip_invalid_and_degenerate_geometry() {
        let cmds = [DrawCommand::Triangles {
            vertices: vec![
                Point::new(10.0, 10.0),
                Point::new(90.0, 10.0),
                Point::new(10.0, 90.0),
                Point::new(f32::NAN, 0.0),
                Point::new(20.0, 0.0),
                Point::new(0.0, 20.0),
                Point::new(40.0, 40.0),
                Point::new(40.0, 40.0),
                Point::new(40.0, 40.0),
            ],
            color: Color::WHITE,
        }];

        let batches = build_batches(&cmds, (100, 100));

        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].vertices.len(), 3);
        for vertex in &batches[0].vertices {
            assert!(vertex.position[0].is_finite());
            assert!(vertex.position[1].is_finite());
        }
    }

    #[test]
    fn build_batches_colored_triangles_skip_invalid_geometry_and_colors() {
        let invalid_color = Color { r: 1.0, g: f32::INFINITY, b: 0.0, a: 1.0 };
        let cmds = [DrawCommand::ColoredTriangles {
            vertices: vec![
                (Point::new(10.0, 10.0), Color::from_rgba8(255, 0, 0, 255)),
                (Point::new(90.0, 10.0), Color::from_rgba8(0, 255, 0, 255)),
                (Point::new(10.0, 90.0), Color::from_rgba8(0, 0, 255, 255)),
                (Point::new(0.0, f32::NEG_INFINITY), Color::WHITE),
                (Point::new(20.0, 0.0), Color::WHITE),
                (Point::new(0.0, 20.0), Color::WHITE),
                (Point::new(40.0, 40.0), Color::WHITE),
                (Point::new(50.0, 40.0), invalid_color),
                (Point::new(40.0, 50.0), Color::WHITE),
            ],
            mask: None,
        }];

        let batches = build_batches(&cmds, (100, 100));

        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].vertices.len(), 3);
        for vertex in &batches[0].vertices {
            assert!(vertex.position[0].is_finite());
            assert!(vertex.position[1].is_finite());
            assert!(vertex.color.iter().all(|channel| channel.is_finite()));
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
    fn build_batches_images_skip_invalid_bounds_uvs_and_tints() {
        let cmds = [
            DrawCommand::Image {
                bounds: Rect::new(0.0, 0.0, 16.0, 16.0),
                uv_rect: Rect::new(0.0, 0.0, 1.0, 1.0),
                tint: Color::WHITE,
            },
            DrawCommand::Image {
                bounds: Rect::new(0.0, 0.0, 0.0, 16.0),
                uv_rect: Rect::new(0.0, 0.0, 1.0, 1.0),
                tint: Color::WHITE,
            },
            DrawCommand::Image {
                bounds: Rect::new(0.0, 0.0, 16.0, 16.0),
                uv_rect: Rect::new(0.0, f32::NAN, 1.0, 1.0),
                tint: Color::WHITE,
            },
            DrawCommand::RasterAtlasImage {
                bounds: Rect::new(0.0, 0.0, 16.0, 16.0),
                uv_rect: Rect::new(0.0, 0.0, 1.0, 1.0),
                tint: Color { r: 1.0, g: 1.0, b: 1.0, a: f32::INFINITY },
            },
        ];

        let batches = build_batches(&cmds, (100, 100));

        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].vertices.len(), 6);
        assert!(batches[0].vertices.iter().all(|vertex| {
            vertex.position.iter().all(|value| value.is_finite())
                && vertex.tex_coord.iter().all(|value| value.is_finite())
                && vertex.color.iter().all(|value| value.is_finite())
        }));
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
        assert!(
            batches[1]
                .vertices
                .iter()
                .all(|vertex| vertex.render_mode == RenderMode::Image as u32),
            "raster atlas images must preserve full RGBA color instead of glyph alpha tinting"
        );
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Unresolved text command
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn build_batches_leaves_unresolved_text_to_the_text_resolver() {
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

        // Glyph-atlas image commands still use alpha-mask tinting.
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
    fn circle_command_batch_sdf_keeps_angle_boundaries_round() {
        let screen_size = (128, 128);
        let cases = [
            Rect::new(24.0, 24.0, 64.0, 64.0),
            Rect::new(24.25, 24.75, 64.0, 64.0),
            Rect::new(48.5, 48.25, 9.0, 9.0),
        ];
        let angles = [
            0.0_f32, 15.0, 30.0, 45.0, 60.0, 75.0, 90.0, 120.0, 135.0, 150.0, 180.0, 225.0, 270.0,
            315.0,
        ];

        for rect in cases {
            let radius = rect.width * 0.5;
            let center = rect.center();
            let cmds = [DrawCommand::Rect {
                bounds: rect,
                color: Color::WHITE,
                corner_radius: radius,
            }];
            let batches = build_batches(&cmds, screen_size);

            assert_eq!(batches.len(), 1);
            let vertices = &batches[0].vertices;
            assert_eq!(vertices.len(), 6);
            for vertex in vertices {
                assert_eq!(vertex.render_mode, RenderMode::Shape as u32);
                assert_eq!(vertex.rect_size, [rect.width, rect.height]);
                assert!((vertex.corner_radius_px - radius).abs() < 0.001);
            }

            let center_distance =
                sample_shape_signed_distance_from_vertices(vertices, screen_size, center)
                    .expect("circle center should be inside generated batch geometry");
            assert!(
                (center_distance + radius).abs() < 0.001,
                "circle center for {rect:?} produced signed distance {center_distance}"
            );

            for angle in angles {
                let radians = angle.to_radians();
                let boundary = Point::new(
                    center.x + radius * radians.cos(),
                    center.y + radius * radians.sin(),
                );
                let d = sample_shape_signed_distance_from_vertices(vertices, screen_size, boundary)
                    .unwrap_or_else(|| {
                        panic!("circle boundary at {angle} degrees missed batch triangles")
                    });
                assert!(
                    d.abs() < 0.01,
                    "circle boundary for {rect:?} at {angle} degrees produced signed distance {d}"
                );

                let inward = Point::new(
                    center.x + (radius - 1.0).max(0.0) * radians.cos(),
                    center.y + (radius - 1.0).max(0.0) * radians.sin(),
                );
                let outward = Point::new(
                    center.x + (radius + 1.0) * radians.cos(),
                    center.y + (radius + 1.0) * radians.sin(),
                );
                let inside =
                    sample_shape_signed_distance_from_vertices(vertices, screen_size, inward)
                        .unwrap_or_else(|| {
                            panic!("inner circle sample at {angle} degrees missed batch triangles")
                        });
                assert!(
                    inside <= -0.75,
                    "inner circle sample for {rect:?} at {angle} degrees should stay filled, d={inside}"
                );
                if rect.contains(outward) {
                    let outside =
                        sample_shape_signed_distance_from_vertices(vertices, screen_size, outward)
                            .unwrap_or_else(|| {
                                panic!(
                                    "outer circle sample at {angle} degrees missed batch triangles"
                                )
                            });
                    assert!(
                        outside >= 0.75,
                        "outer circle sample for {rect:?} at {angle} degrees should stay outside, d={outside}"
                    );
                }
            }
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
