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

fn push_rect(verts: &mut Vec<RectVertex>, rx: f32, ry: f32, rw: f32, rh: f32, r: f32, g: f32, b: f32, a: f32) {
    let x0 = rx; let y0 = ry;
    let x1 = rx + rw; let y1 = ry + rh;
    verts.push(RectVertex::new(x0, y0, 0.0, 0.0, r, g, b, a, 0.0));
    verts.push(RectVertex::new(x1, y0, 1.0, 0.0, r, g, b, a, 0.0));
    verts.push(RectVertex::new(x0, y1, 0.0, 1.0, r, g, b, a, 0.0));
    verts.push(RectVertex::new(x0, y1, 0.0, 1.0, r, g, b, a, 0.0));
    verts.push(RectVertex::new(x1, y0, 1.0, 0.0, r, g, b, a, 0.0));
    verts.push(RectVertex::new(x1, y1, 1.0, 1.0, r, g, b, a, 0.0));
}

/// 生成圆角矩形（所有角使用相同的半径）
///
/// `ndc_r` gives the per-axis NDC radii (rx, ry) so arc points stay circular
/// on non-square screens.
///
/// Tessellation: center rect (inner area) + 4 edge strips (top/bottom/left/right)
/// + 4 corner fans (arcs). The gaps between arcs and outer corners are uncovered
/// → show whatever was drawn behind this rect → visible rounded corners.
pub fn generate_rounded_rect_vertices(
    rect: Rect,
    r: f32,
    g: f32,
    b: f32,
    a: f32,
    _radii: [f32; 4],
    ndc_r: (f32, f32),
) -> Vec<RectVertex> {
    let (rx_initial, ry_initial) = ndc_r;
    if rx_initial <= 0.0 && ry_initial <= 0.0 {
        return generate_rect_vertices(rect, r, g, b, a, 0.0).to_vec();
    }
    let x0 = rect.x;
    let y0 = rect.y;
    let x1 = rect.x + rect.width;
    let y1 = rect.y + rect.height;
    let segments = 16;
    // 9 rects (center + 4 edges + 4 corners as fans) × 6 + 4*segments*3
    let mut verts = Vec::with_capacity(54 + (segments * 4 + 2) * 3);

    let rx = rx_initial.min(rect.width * 0.5).max(0.0);
    let ry = ry_initial.min(rect.height * 0.5).max(0.0);
    if rx <= 0.0 && ry <= 0.0 {
        return generate_rect_vertices(rect, r, g, b, a, 0.0).to_vec();
    }

    // NDC y increases upward: y0 = smaller (screen bottom), y1 = larger (screen top)
    let top_y = y1;
    let bot_y = y0;
    let inner_x0 = x0 + rx;
    let inner_x1 = x1 - rx;
    let inner_y0 = bot_y + ry;
    let inner_y1 = top_y - ry;

    // If inner area collapsed, fall back to plain rect
    if inner_x1 <= inner_x0 || inner_y1 <= inner_y0 {
        return generate_rect_vertices(rect, r, g, b, a, 0.0).to_vec();
    }

    // ── Center rectangle (inner area) ─────────────────────────────────
    {
        // always true since we returned above, but keep the block for clarity
        push_rect(&mut verts, inner_x0, inner_y0, inner_x1 - inner_x0, inner_y1 - inner_y0, r, g, b, a);
    }

    // ── Edge strips ───────────────────────────────────────────────────
    // (original if blocks removed — inner area is guaranteed valid now)
    push_rect(&mut verts, inner_x0, inner_y1, inner_x1 - inner_x0, top_y - inner_y1, r, g, b, a);
    push_rect(&mut verts, inner_x0, bot_y, inner_x1 - inner_x0, inner_y0 - bot_y, r, g, b, a);
    push_rect(&mut verts, x0, inner_y0, inner_x0 - x0, inner_y1 - inner_y0, r, g, b, a);
    push_rect(&mut verts, inner_x1, inner_y0, x1 - inner_x1, inner_y1 - inner_y0, r, g, b, a);

    // ── Corner fans ─────────────────────────────────────────────────
    let corner_centers = [
        Vec2::new(x0 + rx, top_y - ry),   // TL
        Vec2::new(x1 - rx, top_y - ry),   // TR
        Vec2::new(x1 - rx, bot_y + ry),   // BR
        Vec2::new(x0 + rx, bot_y + ry),   // BL
    ];
    let start_angles = [
        std::f32::consts::FRAC_PI_2,       // TL: π/2 → π  (top→left)
        0.0,                                // TR: 0 → π/2   (right→top)
        std::f32::consts::PI * 1.5,        // BR: 3π/2→2π  (bottom→right)
        std::f32::consts::PI,              // BL: π→3π/2   (left→bottom)
    ];
    let inner_corners = [
        Vec2::new(inner_x0, inner_y1), // TL
        Vec2::new(inner_x1, inner_y1), // TR
        Vec2::new(inner_x1, inner_y0), // BR
        Vec2::new(inner_x0, inner_y0), // BL
    ];

    for ci in 0..4 {
        let center = corner_centers[ci];
        let inner = inner_corners[ci];
        let start = start_angles[ci];
        for i in 0..segments {
            let a0 = start + (i as f32) / (segments as f32) * std::f32::consts::FRAC_PI_2;
            let a1 = start + ((i + 1) as f32) / (segments as f32) * std::f32::consts::FRAC_PI_2;
            let p0 = center + Vec2::new(a0.cos() * rx, a0.sin() * ry);
            let p1 = center + Vec2::new(a1.cos() * rx, a1.sin() * ry);
            // radius=0 = geometric tessellation, no SDF clipping
            verts.push(RectVertex::new(inner.x, inner.y, 0.0, 0.0, r, g, b, a, 0.0));
            verts.push(RectVertex::new(p0.x, p0.y, 0.0, 0.0, r, g, b, a, 0.0));
            verts.push(RectVertex::new(p1.x, p1.y, 0.0, 0.0, r, g, b, a, 0.0));
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
            (0.0, 0.0),
        );
        assert_eq!(verts.len(), 6);
    }

    #[test]
    fn rounded_rect_with_radii_produces_many_vertices() {
        let verts = generate_rounded_rect_vertices(
            Rect::new(0.0, 0.0, 100.0, 50.0),
            1.0, 1.0, 1.0, 1.0,
            [8.0; 4],
            (8.0, 8.0),
        );
        // center(6) + 4 edges(6×4=24) + 4 corners × 16 segments × 3 = 30 + 192 = 222
        assert!(verts.len() > 6);
        assert_eq!(verts.len(), 222);
    }

    #[test]
    fn rounded_rect_vertices_are_divisible_by_3() {
        let verts = generate_rounded_rect_vertices(
            Rect::new(0.0, 0.0, 200.0, 100.0),
            1.0, 1.0, 1.0, 1.0,
            [16.0, 0.0, 16.0, 0.0],
            (16.0, 16.0),
        );
        assert_eq!(verts.len() % 3, 0);
    }

    #[test]
    fn rounded_rect_all_vertices_in_bounds() {
        let rect = Rect::new(0.0, 0.0, 200.0, 100.0);
        let verts = generate_rounded_rect_vertices(rect, 1.0, 1.0, 1.0, 1.0, [16.0; 4], (16.0, 16.0));
        for v in &verts {
            assert!(v.position[0] >= rect.x - 0.1, "vertex x={} below min_x={}", v.position[0], rect.x);
            assert!(v.position[0] <= rect.x + rect.width + 0.1, "vertex x={} above max_x={}", v.position[0], rect.x + rect.width);
            assert!(v.position[1] >= rect.y - 0.1);
            assert!(v.position[1] <= rect.y + rect.height + 0.1);
        }
    }

    #[test]
    fn rounded_rect_small_rect_falls_back() {
        let verts = generate_rounded_rect_vertices(
            Rect::new(0.0, 0.0, 8.0, 8.0),
            1.0, 1.0, 1.0, 1.0,
            [10.0; 4],
            (10.0, 10.0),
        );
        assert_eq!(verts.len(), 6, "Small rect should fall back to plain rect");
    }

    #[test]
    fn rounded_rect_different_corner_radii() {
        let verts = generate_rounded_rect_vertices(
            Rect::new(0.0, 0.0, 100.0, 100.0),
            1.0, 0.0, 0.0, 1.0,
            [0.0, 8.0, 16.0, 4.0],
            (16.0, 16.0),
        );
        assert!(verts.len() > 6, "Should generate corner fans for non-zero radii");
        assert_eq!(verts.len() % 3, 0);
    }
}
