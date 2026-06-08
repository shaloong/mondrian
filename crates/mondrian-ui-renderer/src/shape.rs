//! 2D 顶点类型与形状生成
//!
//! 所有 UI 形状最终都三角化为 [`RectVertex`] 流，送入 GPU 管线。

use bytemuck::{Pod, Zeroable};
use mondrian_ui_core::types::Rect;

/// Render mode sentinel for the fragment shader.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderMode {
    Shape = 0,
    Glyph = 1,
}

/// UI 渲染的顶点格式 (48 bytes, packed).
///
/// The shader clamps `corner_radius_px` automatically — callers do not need
/// to pre-clamp to half-size.
///
/// Shader locations:
///   0: position
///   1: tex_coord
///   2: color
///   3: rect_size (pixels, for pixel-space rounded-rect SDF)
///   4: corner_radius_px (0 = sharp rect)
///   5: render_mode (0 = shape, 1 = glyph)
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct RectVertex {
    pub position: [f32; 2],    // 8 bytes,  offset 0
    pub tex_coord: [f32; 2],   // 8 bytes,  offset 8
    pub color: [f32; 4],       // 16 bytes, offset 16
    pub rect_size: [f32; 2],   // 8 bytes,  offset 32
    pub corner_radius_px: f32, // 4 bytes,  offset 40
    pub render_mode: u32,      // 4 bytes,  offset 44
}

impl RectVertex {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        x: f32,
        y: f32,
        u: f32,
        v: f32,
        r: f32,
        g: f32,
        b: f32,
        a: f32,
        rect_w: f32,
        rect_h: f32,
        corner_radius_px: f32,
        render_mode: RenderMode,
    ) -> Self {
        Self {
            position: [x, y],
            tex_coord: [u, v],
            color: [r, g, b, a],
            rect_size: [rect_w, rect_h],
            corner_radius_px,
            render_mode: render_mode as u32,
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
                    format: wgpu::VertexFormat::Float32x2,
                    offset: 32,
                    shader_location: 3,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32,
                    offset: 40,
                    shader_location: 4,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Uint32,
                    offset: 44,
                    shader_location: 5,
                },
            ],
        }
    }
}

/// 生成普通矩形（6 个顶点，2 个三角形）。所有顶点共享相同的 per-rect 属性。
pub fn generate_rect_vertices(
    rect: Rect,
    r: f32,
    g: f32,
    b: f32,
    a: f32,
    pixel_w: f32,
    pixel_h: f32,
    corner_radius_px: f32,
    render_mode: RenderMode,
) -> [RectVertex; 6] {
    let x0 = rect.x;
    let y0 = rect.y;
    let x1 = rect.x + rect.width;
    let y1 = rect.y + rect.height;

    let v = |x: f32, y: f32, u: f32, v: f32| {
        RectVertex::new(
            x,
            y,
            u,
            v,
            r,
            g,
            b,
            a,
            pixel_w,
            pixel_h,
            corner_radius_px,
            render_mode,
        )
    };

    [
        v(x0, y0, 0.0, 0.0),
        v(x1, y0, 1.0, 0.0),
        v(x0, y1, 0.0, 1.0),
        v(x0, y1, 0.0, 1.0),
        v(x1, y0, 1.0, 0.0),
        v(x1, y1, 1.0, 1.0),
    ]
}

/// Rust 端等价于 WGSL 的 sd_rounded_box_px，用于 CPU 侧 SDF 验证。
///
/// 对应 WGSL 片段着色器 let d = sd_rounded_box_px(p, size, r)：
/// - p — 像素坐标 (0..size)，从矩形左上角开始
/// - size — 矩形尺寸（像素）
/// - r -- 圆角半径（像素，已 clamp 至 min(size) * 0.5）

///
/// 返回值为负数表示在形状内部，0 表示在边界上，正数表示在外部。
fn sd_rounded_box_px_rust(p: [f32; 2], size: [f32; 2], r: f32) -> f32 {
    let half_x = size[0] * 0.5;
    let half_y = size[1] * 0.5;
    let qx = (p[0] - half_x).abs() - half_x + r;
    let qy = (p[1] - half_y).abs() - half_y + r;
    let q_clamped = [qx.max(0.0), qy.max(0.0)];
    let q_magnitude = (q_clamped[0] * q_clamped[0] + q_clamped[1] * q_clamped[1]).sqrt();
    q_magnitude + qx.max(qy).min(0.0) - r
}

#[cfg(test)]
mod tests {
    use super::*;

    // ═══════════════════════════════════════════════════════════════════════
    // RectVertex
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn vertex_size_is_48_bytes() {
        assert_eq!(std::mem::size_of::<RectVertex>(), 48);
    }

    #[test]
    fn vertex_is_pod_zeroable() {
        fn assert_pod<T: Pod + Zeroable>() {}
        assert_pod::<RectVertex>();
    }

    #[test]
    fn vertex_layout_stride_matches_size() {
        let layout = RectVertex::layout();
        assert_eq!(
            layout.array_stride as usize,
            std::mem::size_of::<RectVertex>()
        );
    }

    #[test]
    fn vertex_layout_has_6_attributes() {
        let layout = RectVertex::layout();
        assert_eq!(layout.attributes.len(), 6);
    }

    #[test]
    fn vertex_new_stores_all_fields() {
        let v = RectVertex::new(
            1.0,
            2.0,
            0.5,
            0.5,
            0.1,
            0.2,
            0.3,
            0.8,
            100.0,
            50.0,
            8.0,
            RenderMode::Shape,
        );
        assert_eq!(v.position, [1.0, 2.0]);
        assert_eq!(v.tex_coord, [0.5, 0.5]);
        assert_eq!(v.color, [0.1, 0.2, 0.3, 0.8]);
        assert_eq!(v.rect_size, [100.0, 50.0]);
        assert_eq!(v.corner_radius_px, 8.0);
        assert_eq!(v.render_mode, RenderMode::Shape as u32);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // generate_rect_vertices
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn rect_vertices_has_6_elements() {
        let verts = generate_rect_vertices(
            Rect::new(0.0, 0.0, 100.0, 50.0),
            1.0,
            0.0,
            0.0,
            1.0,
            100.0,
            50.0,
            0.0,
            RenderMode::Shape,
        );
        assert_eq!(verts.len(), 6);
    }

    #[test]
    fn rect_vertices_in_bounds() {
        let rect = Rect::new(10.0, 20.0, 100.0, 50.0);
        let verts = generate_rect_vertices(
            rect,
            1.0,
            0.0,
            0.0,
            1.0,
            100.0,
            50.0,
            0.0,
            RenderMode::Shape,
        );
        for v in &verts {
            assert!(v.position[0] >= rect.x - 0.01);
            assert!(v.position[0] <= rect.x + rect.width + 0.01);
            assert!(v.position[1] >= rect.y - 0.01);
            assert!(v.position[1] <= rect.y + rect.height + 0.01);
        }
    }

    #[test]
    fn rect_vertices_pass_color_correctly() {
        let verts = generate_rect_vertices(
            Rect::ZERO,
            0.0,
            1.0,
            0.0,
            0.5,
            100.0,
            50.0,
            0.0,
            RenderMode::Shape,
        );
        for v in &verts {
            assert_eq!(v.color, [0.0, 1.0, 0.0, 0.5]);
        }
    }

    #[test]
    fn rect_vertices_pass_rect_size() {
        let verts = generate_rect_vertices(
            Rect::new(0.0, 0.0, 200.0, 80.0),
            1.0,
            1.0,
            1.0,
            1.0,
            200.0,
            80.0,
            0.0,
            RenderMode::Shape,
        );
        for v in &verts {
            assert_eq!(v.rect_size, [200.0, 80.0]);
        }
    }

    #[test]
    fn rect_with_corner_radius_passes_to_vertices() {
        let verts = generate_rect_vertices(
            Rect::new(0.0, 0.0, 100.0, 50.0),
            1.0,
            1.0,
            1.0,
            1.0,
            100.0,
            50.0,
            8.0,
            RenderMode::Shape,
        );
        for v in &verts {
            assert!((v.corner_radius_px - 8.0).abs() < 0.001);
        }
    }

    #[test]
    fn rect_glyph_mode_sets_render_mode() {
        let verts = generate_rect_vertices(
            Rect::new(0.0, 0.0, 32.0, 32.0),
            1.0,
            1.0,
            1.0,
            1.0,
            32.0,
            32.0,
            -1.0,
            RenderMode::Glyph,
        );
        for v in &verts {
            assert_eq!(v.render_mode, RenderMode::Glyph as u32);
        }
    }

    #[test]
    fn rect_large_radius_does_not_panic() {
        // Radius exceeding half-size is fine — shader clamps automatically.
        let verts = generate_rect_vertices(
            Rect::new(0.0, 0.0, 100.0, 50.0),
            1.0,
            1.0,
            1.0,
            1.0,
            100.0,
            50.0,
            5000.0,
            RenderMode::Shape,
        );
        assert_eq!(verts.len(), 6);
        for v in &verts {
            assert_eq!(v.corner_radius_px, 5000.0);
        }
    }
    // ═══════════════════════════════════════════════════════════════════════
    // SDF: 圆形内接验证（对应 WGSL sd_rounded_box_px）
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn circle_100x100_inscribed_sdf_center() {
        // 中心点：距边界 50px
        let d = sd_rounded_box_px_rust([50.0, 50.0], [100.0, 100.0], 50.0);
        assert!((d - (-50.0)).abs() < 0.001, "center sd={} should be -50", d);
    }

    #[test]
    fn circle_100x100_inscribed_sdf_right_edge() {
        // 右边缘中点：正好在圆上
        let d = sd_rounded_box_px_rust([100.0, 50.0], [100.0, 100.0], 50.0);
        assert!((d - 0.0).abs() < 0.001, "right edge sd={} should be 0", d);
    }

    #[test]
    fn circle_100x100_inscribed_sdf_left_edge() {
        let d = sd_rounded_box_px_rust([0.0, 50.0], [100.0, 100.0], 50.0);
        assert!((d - 0.0).abs() < 0.001, "left edge sd={} should be 0", d);
    }

    #[test]
    fn circle_100x100_inscribed_sdf_top_edge() {
        let d = sd_rounded_box_px_rust([50.0, 0.0], [100.0, 100.0], 50.0);
        assert!((d - 0.0).abs() < 0.001, "top edge sd={} should be 0", d);
    }

    #[test]
    fn circle_100x100_inscribed_sdf_bottom_edge() {
        let d = sd_rounded_box_px_rust([50.0, 100.0], [100.0, 100.0], 50.0);
        assert!((d - 0.0).abs() < 0.001, "bottom edge sd={} should be 0", d);
    }

    #[test]
    fn circle_100x100_inscribed_sdf_golden_angle() {
        // 45° 对角线方向：距圆心 50px 正好在圆上
        // cos(45°) = sin(45°) = 0.7071..., 50 * 0.7071... ≈ 35.355
        // 圆心在 (50,50)，圆上点为 (50±35.355, 50±35.355)
        let dist = 50.0_f64 * std::f64::consts::FRAC_1_SQRT_2;
        let d = sd_rounded_box_px_rust(
            [50.0 + dist as f32, 50.0 + dist as f32],
            [100.0, 100.0],
            50.0,
        );
        assert!(d.abs() < 0.001, "45° point sd={} should be 0", d);
    }

    #[test]
    fn circle_100x100_inscribed_sdf_inside() {
        // 右边缘向内 10px: 距圆心 40px → 内部 10px
        let d = sd_rounded_box_px_rust([90.0, 50.0], [100.0, 100.0], 50.0);
        assert!((d - (-10.0)).abs() < 0.001, "inside sd={} should be -10", d);
    }

    #[test]
    fn circle_100x100_inscribed_sdf_outside_corner() {
        // 矩形角点 (100,100)：距圆心 sqrt(50²+50²)=70.71 → 外部 20.71
        let d = sd_rounded_box_px_rust([100.0, 100.0], [100.0, 100.0], 50.0);
        let expected = (2.0_f64 * (50.0_f64 * 50.0_f64)).sqrt() as f32 - 50.0;
        assert!(
            (d - expected).abs() < 0.001,
            "corner sd={} should be {}",
            d,
            expected
        );
    }

    // ═══════════════════════════════════════════════════════════════════════
    // SDF: 非正方形矩形 + 圆形半径
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn circle_200x100_inscribed_short_axis() {
        // 200x100 矩形，r=50（clamp 到短轴半长 50）→ 短轴方向为圆
        let d = sd_rounded_box_px_rust([50.0, 0.0], [200.0, 100.0], 50.0);
        assert!((d - 0.0).abs() < 0.001, "short-axis top edge sd={}", d);
    }

    #[test]
    fn circle_200x100_long_axis_outside() {
        // 右边缘中点 (200,50) 在胶囊形短轴上，正好在边界上
        let d = sd_rounded_box_px_rust([200.0, 50.0], [200.0, 100.0], 50.0);
        assert!((d - 0.0).abs() < 0.001, "right edge on pill rect sd={}", d);
    }

    // SDF: 数值稳定性
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn sdf_radius_zero_is_sharp_rect() {
        let d = sd_rounded_box_px_rust([100.0, 100.0], [100.0, 100.0], 0.0);
        assert!((d - 0.0).abs() < 0.001, "sharp corner sd={} should be 0", d);
    }

    #[test]
    fn sdf_radius_exceeds_half_still_valid() {
        // r 超过 min/2 会被 WGSL clamp，但裸函数自身不会 clamp
        // 在此检验函数不会 panic/NAN，且给出有限值
        let d = sd_rounded_box_px_rust([100.0, 50.0], [100.0, 100.0], 1000.0);
        assert!(d.is_finite(), "over-large r should be finite sd={}", d);
        // 注意：裸露函数对过大 r 不 clamp，SDF 为正值（点在扩展边界外）
    }

    #[test]
    fn sdf_tiny_rect_does_not_nan() {
        let d = sd_rounded_box_px_rust([0.5, 0.5], [1.0, 1.0], 0.5);
        assert!(d.is_finite(), "tiny rect sd should be finite, got {}", d);
        assert!((d - (-0.5)).abs() < 0.001, "tiny circle center sd={}", d);
    }

    #[test]
    fn sdf_large_rect_no_overflow() {
        let d = sd_rounded_box_px_rust([2048.0, 1080.0], [4096.0, 2160.0], 100.0);
        assert!(d.is_finite(), "large rect sd should be finite, got {}", d);
    }
    // ═══════════════════════════════════════════════════════════════════════
    // Vertex: local_pos (tex_coord) 数据完整性
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn rect_tex_coord_spans_zero_to_one() {
        let verts = generate_rect_vertices(
            Rect::new(-0.5, 0.5, 1.0, 1.0),
            1.0,
            1.0,
            1.0,
            1.0,
            100.0,
            100.0,
            50.0,
            RenderMode::Shape,
        );
        let min_u = verts.iter().map(|v| v.tex_coord[0]).reduce(f32::min).unwrap();
        let max_u = verts.iter().map(|v| v.tex_coord[0]).reduce(f32::max).unwrap();
        let min_v = verts.iter().map(|v| v.tex_coord[1]).reduce(f32::min).unwrap();
        let max_v = verts.iter().map(|v| v.tex_coord[1]).reduce(f32::max).unwrap();
        assert!((min_u - 0.0).abs() < 0.001, "min_u={}", min_u);
        assert!((max_u - 1.0).abs() < 0.001, "max_u={}", max_u);
        assert!((min_v - 0.0).abs() < 0.001, "min_v={}", min_v);
        assert!((max_v - 1.0).abs() < 0.001, "max_v={}", max_v);
    }

    #[test]
    fn circle_vertex_passes_rect_size_as_pixel_dimensions() {
        let verts = generate_rect_vertices(
            Rect::new(-0.5, -0.5, 0.5, 0.5),
            1.0,
            1.0,
            1.0,
            1.0,
            100.0,
            100.0,
            50.0,
            RenderMode::Shape,
        );
        for v in &verts {
            assert_eq!(
                v.rect_size,
                [100.0, 100.0],
                "rect_size should be pixel dims"
            );
            assert!(
                (v.corner_radius_px - 50.0).abs() < 0.001,
                "corner_radius_px={} should be 50",
                v.corner_radius_px
            );
        }
    }

    #[test]
    fn rect_sdf_reconstruction_from_vertex_data() {
        // local_pos * rect_size → 像素坐标（对应 WGSL 片段着色器 let p = in.local_pos * in.rect_size）
        let verts = generate_rect_vertices(
            Rect::new(-0.5, -0.5, 1.0, 1.0),
            1.0,
            1.0,
            1.0,
            1.0,
            100.0,
            100.0,
            0.0,
            RenderMode::Shape,
        );
        for v in &verts {
            let p_local = [
                v.tex_coord[0] * v.rect_size[0],
                v.tex_coord[1] * v.rect_size[1],
            ];
            assert!(
                p_local[0] >= 0.0 && p_local[0] <= 100.0,
                "p_local.x={}",
                p_local[0]
            );
            assert!(
                p_local[1] >= 0.0 && p_local[1] <= 100.0,
                "p_local.y={}",
                p_local[1]
            );
        }
    }
}
