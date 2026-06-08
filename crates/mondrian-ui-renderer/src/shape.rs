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
    pub position: [f32; 2],       // 8 bytes,  offset 0
    pub tex_coord: [f32; 2],      // 8 bytes,  offset 8
    pub color: [f32; 4],          // 16 bytes, offset 16
    pub rect_size: [f32; 2],      // 8 bytes,  offset 32
    pub corner_radius_px: f32,    // 4 bytes,  offset 40
    pub render_mode: u32,         // 4 bytes,  offset 44
}

impl RectVertex {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        x: f32, y: f32,
        u: f32, v: f32,
        r: f32, g: f32, b: f32, a: f32,
        rect_w: f32, rect_h: f32,
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
    r: f32, g: f32, b: f32, a: f32,
    pixel_w: f32, pixel_h: f32,
    corner_radius_px: f32,
    render_mode: RenderMode,
) -> [RectVertex; 6] {
    let x0 = rect.x;
    let y0 = rect.y;
    let x1 = rect.x + rect.width;
    let y1 = rect.y + rect.height;

    let v = |x: f32, y: f32, u: f32, v: f32| {
        RectVertex::new(x, y, u, v, r, g, b, a, pixel_w, pixel_h, corner_radius_px, render_mode)
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
        assert_eq!(layout.array_stride as usize, std::mem::size_of::<RectVertex>());
    }

    #[test]
    fn vertex_layout_has_6_attributes() {
        let layout = RectVertex::layout();
        assert_eq!(layout.attributes.len(), 6);
    }

    #[test]
    fn vertex_new_stores_all_fields() {
        let v = RectVertex::new(1.0, 2.0, 0.5, 0.5, 0.1, 0.2, 0.3, 0.8, 100.0, 50.0, 8.0, RenderMode::Shape);
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
            1.0, 0.0, 0.0, 1.0, 100.0, 50.0, 0.0, RenderMode::Shape,
        );
        assert_eq!(verts.len(), 6);
    }

    #[test]
    fn rect_vertices_in_bounds() {
        let rect = Rect::new(10.0, 20.0, 100.0, 50.0);
        let verts = generate_rect_vertices(rect, 1.0, 0.0, 0.0, 1.0, 100.0, 50.0, 0.0, RenderMode::Shape);
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
            Rect::ZERO, 0.0, 1.0, 0.0, 0.5, 100.0, 50.0, 0.0, RenderMode::Shape,
        );
        for v in &verts {
            assert_eq!(v.color, [0.0, 1.0, 0.0, 0.5]);
        }
    }

    #[test]
    fn rect_vertices_pass_rect_size() {
        let verts = generate_rect_vertices(
            Rect::new(0.0, 0.0, 200.0, 80.0),
            1.0, 1.0, 1.0, 1.0, 200.0, 80.0, 0.0, RenderMode::Shape,
        );
        for v in &verts {
            assert_eq!(v.rect_size, [200.0, 80.0]);
        }
    }

    #[test]
    fn rect_with_corner_radius_passes_to_vertices() {
        let verts = generate_rect_vertices(
            Rect::new(0.0, 0.0, 100.0, 50.0),
            1.0, 1.0, 1.0, 1.0, 100.0, 50.0, 8.0, RenderMode::Shape,
        );
        for v in &verts {
            assert!((v.corner_radius_px - 8.0).abs() < 0.001);
        }
    }

    #[test]
    fn rect_glyph_mode_sets_render_mode() {
        let verts = generate_rect_vertices(
            Rect::new(0.0, 0.0, 32.0, 32.0),
            1.0, 1.0, 1.0, 1.0, 32.0, 32.0, -1.0, RenderMode::Glyph,
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
            1.0, 1.0, 1.0, 1.0, 100.0, 50.0, 5000.0, RenderMode::Shape,
        );
        assert_eq!(verts.len(), 6);
        for v in &verts {
            assert_eq!(v.corner_radius_px, 5000.0);
        }
    }
}
