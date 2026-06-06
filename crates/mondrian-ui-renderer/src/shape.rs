//! 2D 顶点类型与形状生成
//!
//! 所有 UI 形状最终都三角化为 [`RectVertex`] 流，送入 GPU 管线。

use bytemuck::{Pod, Zeroable};
use glam::Vec2;
use mondrian_ui_core::types::Rect;

/// UI 渲染的顶点格式
///
/// 匹配 shader 中的 `@location(0)` / `@location(1)` 布局。
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct RectVertex {
    pub position: [f32; 2],
    pub tex_coord: [f32; 2],
    pub color: [f32; 4],
    pub corner_radius: f32,
}

impl RectVertex {
    pub fn new(x: f32, y: f32, u: f32, v: f32, r: f32, g: f32, b: f32, a: f32, radius: f32) -> Self {
        Self {
            position: [x, y],
            tex_coord: [u, v],
            color: [r, g, b, a],
            corner_radius: radius,
        }
    }

    pub fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Self>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x2,
                    offset: 0,
                    shader_location: 0,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x2,
                    offset: 8,
                    shader_location: 1,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x4,
                    offset: 16,
                    shader_location: 2,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32,
                    offset: 32,
                    shader_location: 3,
                },
            ],
        }
    }
}

/// 生成普通矩形（6 个顶点，2 个三角形）
pub fn generate_rect_vertices(rect: Rect, r: f32, g: f32, b: f32, a: f32, radius: f32) -> [RectVertex; 6] {
    let x0 = rect.x;
    let y0 = rect.y;
    let x1 = rect.x + rect.width;
    let y1 = rect.y + rect.height;

    [
        RectVertex::new(x0, y0, 0.0, 0.0, r, g, b, a, radius),
        RectVertex::new(x1, y0, 1.0, 0.0, r, g, b, a, radius),
        RectVertex::new(x0, y1, 0.0, 1.0, r, g, b, a, radius),
        RectVertex::new(x0, y1, 0.0, 1.0, r, g, b, a, radius),
        RectVertex::new(x1, y0, 1.0, 0.0, r, g, b, a, radius),
        RectVertex::new(x1, y1, 1.0, 1.0, r, g, b, a, radius),
    ]
}

/// 生成圆角矩形（4 个角各有独立半径）
pub fn generate_rounded_rect_vertices(
    rect: Rect,
    r: f32,
    g: f32,
    b: f32,
    a: f32,
    radii: [f32; 4],
) -> Vec<RectVertex> {
    if radii.iter().all(|&r| r <= 0.0) {
        return generate_rect_vertices(rect, r, g, b, a, 0.0).to_vec();
    }

    let x0 = rect.x;
    let y0 = rect.y;
    let x1 = rect.x + rect.width;
    let y1 = rect.y + rect.height;
    let segments = 8; // 每个角的三角形分段数
    let mut verts = Vec::with_capacity((segments * 4 + 2) * 3);

    // 使用简化的三角剖分：中心 + 外环顶点
    let radii_clamped = [
        radii[0].min(rect.width * 0.5).min(rect.height * 0.5).max(0.0), // top-left
        radii[1].min(rect.width * 0.5).min(rect.height * 0.5).max(0.0), // top-right
        radii[2].min(rect.width * 0.5).min(rect.height * 0.5).max(0.0), // bottom-right
        radii[3].min(rect.width * 0.5).min(rect.height * 0.5).max(0.0), // bottom-left
    ];

    let corner_centers = [
        Vec2::new(x0 + radii_clamped[0], y0 + radii_clamped[0]),
        Vec2::new(x1 - radii_clamped[1], y0 + radii_clamped[1]),
        Vec2::new(x1 - radii_clamped[2], y1 - radii_clamped[2]),
        Vec2::new(x0 + radii_clamped[3], y1 - radii_clamped[3]),
    ];

    let corner_start_angles = [
        std::f32::consts::PI,           // top-left: π → 3π/2
        std::f32::consts::PI * 1.5,     // top-right: 3π/2 → 2π
        0.0,                             // bottom-right: 0 → π/2
        std::f32::consts::PI * 0.5,     // bottom-left: π/2 → π
    ];

    // 中心矩形（核心区域）的两个三角形
    let inner_x0 = x0 + radii_clamped[0].max(radii_clamped[3]);
    let inner_y0 = y0 + radii_clamped[0].max(radii_clamped[1]);
    let inner_x1 = x1 - radii_clamped[1].max(radii_clamped[2]);
    let inner_y1 = y1 - radii_clamped[2].max(radii_clamped[3]);

    if inner_x1 > inner_x0 && inner_y1 > inner_y0 {
        verts.push(RectVertex::new(inner_x0, inner_y0, 0.5, 0.5, r, g, b, a, 0.0));
        verts.push(RectVertex::new(inner_x1, inner_y0, 0.5, 0.5, r, g, b, a, 0.0));
        verts.push(RectVertex::new(inner_x0, inner_y1, 0.5, 0.5, r, g, b, a, 0.0));

        verts.push(RectVertex::new(inner_x0, inner_y1, 0.5, 0.5, r, g, b, a, 0.0));
        verts.push(RectVertex::new(inner_x1, inner_y0, 0.5, 0.5, r, g, b, a, 0.0));
        verts.push(RectVertex::new(inner_x1, inner_y1, 0.5, 0.5, r, g, b, a, 0.0));
    } else {
        // 太小了，退化为普通矩形
        return generate_rect_vertices(rect, r, g, b, a, radii[0]).to_vec();
    }

    // 每个角从核心矩形向外的扇形三角形带
    let inner_corners = [
        Vec2::new(inner_x0, inner_y0),
        Vec2::new(inner_x1, inner_y0),
        Vec2::new(inner_x1, inner_y1),
        Vec2::new(inner_x0, inner_y1),
    ];

    for corner_idx in 0..4 {
        let center = corner_centers[corner_idx];
        let r_c = radii_clamped[corner_idx];
        if r_c <= 0.0 {
            continue;
        }
        let inner = inner_corners[corner_idx];
        let start_angle = corner_start_angles[corner_idx];

        for i in 0..segments {
            let a0 = start_angle + (i as f32) / (segments as f32) * std::f32::consts::FRAC_PI_2;
            let a1 = start_angle + ((i + 1) as f32) / (segments as f32) * std::f32::consts::FRAC_PI_2;

            let p0 = center + Vec2::new(a0.cos() * r_c, a0.sin() * r_c);
            let p1 = center + Vec2::new(a1.cos() * r_c, a1.sin() * r_c);

            // 三角形: inner → p0 → p1
            verts.push(RectVertex::new(inner.x, inner.y, 0.5, 0.5, r, g, b, a, 0.0));
            verts.push(RectVertex::new(p0.x, p0.y, 0.0, 0.0, r, g, b, a, r_c));
            verts.push(RectVertex::new(p1.x, p1.y, 0.0, 0.0, r, g, b, a, r_c));
        }
    }

    verts
}

#[cfg(test)]
mod tests {
    use super::*;

    // ═══════════════════════════════════════════════════════════════════════
    // RectVertex
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn vertex_size_is_36_bytes() {
        assert_eq!(std::mem::size_of::<RectVertex>(), 36);
    }

    #[test]
    fn vertex_is_pod_zeroable() {
        // Compile-time check that RectVertex implements Pod + Zeroable
        fn assert_pod<T: Pod + Zeroable>() {}
        assert_pod::<RectVertex>();
    }

    #[test]
    fn vertex_layout_stride_matches_size() {
        let layout = RectVertex::layout();
        assert_eq!(layout.array_stride as usize, std::mem::size_of::<RectVertex>());
    }

    #[test]
    fn vertex_layout_has_4_attributes() {
        let layout = RectVertex::layout();
        assert_eq!(layout.attributes.len(), 4);
    }

    #[test]
    fn vertex_new_stores_all_fields() {
        let v = RectVertex::new(1.0, 2.0, 0.5, 0.5, 0.1, 0.2, 0.3, 0.8, 10.0);
        assert_eq!(v.position, [1.0, 2.0]);
        assert_eq!(v.tex_coord, [0.5, 0.5]);
        assert_eq!(v.color, [0.1, 0.2, 0.3, 0.8]);
        assert_eq!(v.corner_radius, 10.0);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // generate_rect_vertices
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn rect_vertices_has_6_elements() {
        let verts = generate_rect_vertices(Rect::new(0.0, 0.0, 100.0, 50.0), 1.0, 0.0, 0.0, 1.0, 0.0);
        assert_eq!(verts.len(), 6);
    }

    #[test]
    fn rect_vertices_in_bounds() {
        let rect = Rect::new(10.0, 20.0, 100.0, 50.0);
        let verts = generate_rect_vertices(rect, 1.0, 0.0, 0.0, 1.0, 0.0);
        for v in &verts {
            assert!(v.position[0] >= rect.x - 0.01);
            assert!(v.position[0] <= rect.x + rect.width + 0.01);
            assert!(v.position[1] >= rect.y - 0.01);
            assert!(v.position[1] <= rect.y + rect.height + 0.01);
        }
    }

    #[test]
    fn rect_vertices_pass_color_correctly() {
        let verts = generate_rect_vertices(Rect::ZERO, 0.0, 1.0, 0.0, 0.5, 0.0);
        for v in &verts {
            assert_eq!(v.color, [0.0, 1.0, 0.0, 0.5]);
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // generate_rounded_rect_vertices
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn rounded_rect_zero_radii_falls_back_to_plain() {
        let verts = generate_rounded_rect_vertices(
            Rect::new(0.0, 0.0, 100.0, 50.0),
            1.0, 1.0, 1.0, 1.0,
            [0.0; 4],
        );
        assert_eq!(verts.len(), 6);
    }

    #[test]
    fn rounded_rect_with_radii_produces_more_vertices() {
        let verts = generate_rounded_rect_vertices(
            Rect::new(0.0, 0.0, 100.0, 50.0),
            1.0, 1.0, 1.0, 1.0,
            [8.0; 4],
        );
        // 4 corners * 8 segments * 3 verts + 6 center verts = 102
        assert!(verts.len() > 6);
        assert_eq!(verts.len(), 102);
    }

    #[test]
    fn rounded_rect_vertices_are_divisible_by_3() {
        let verts = generate_rounded_rect_vertices(
            Rect::new(0.0, 0.0, 200.0, 100.0),
            1.0, 1.0, 1.0, 1.0,
            [16.0, 0.0, 16.0, 0.0],
        );
        assert_eq!(verts.len() % 3, 0, "Vertex count should be divisible by 3 (triangles)");
    }

    #[test]
    fn rounded_rect_all_vertices_in_bounds() {
        let rect = Rect::new(0.0, 0.0, 200.0, 100.0);
        let verts = generate_rounded_rect_vertices(rect, 1.0, 1.0, 1.0, 1.0, [16.0; 4]);
        for v in &verts {
            assert!(v.position[0] >= rect.x - 0.1, "vertex x={} below min_x={}", v.position[0], rect.x);
            assert!(v.position[0] <= rect.x + rect.width + 0.1, "vertex x={} above max_x={}", v.position[0], rect.x + rect.width);
            assert!(v.position[1] >= rect.y - 0.1);
            assert!(v.position[1] <= rect.y + rect.height + 0.1);
        }
    }

    #[test]
    fn rounded_rect_small_rect_falls_back() {
        // A rect so small that the inner rectangle collapses
        let verts = generate_rounded_rect_vertices(
            Rect::new(0.0, 0.0, 8.0, 8.0),
            1.0, 1.0, 1.0, 1.0,
            [10.0; 4], // radius > rect size
        );
        assert_eq!(verts.len(), 6, "Small rect should fall back to plain rect");
    }

    #[test]
    fn rounded_rect_different_corner_radii() {
        // Each corner with a different radius
        let verts = generate_rounded_rect_vertices(
            Rect::new(0.0, 0.0, 100.0, 100.0),
            1.0, 0.0, 0.0, 1.0,
            [0.0, 8.0, 16.0, 4.0],
        );
        assert!(verts.len() > 6, "Should generate corner fans for non-zero radii");
        assert_eq!(verts.len() % 3, 0);
    }
}
