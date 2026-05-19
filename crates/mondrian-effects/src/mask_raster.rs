//! Mask shape rasterization — converts MaskShape into alpha buffers.
//!
//! Uses Signed Distance Field (SDF) evaluation for smooth anti-aliased
//! edges and natural feather/expansion support.

use super::mask::{BezierPoint, MaskShape};
use glam::Vec2;

/// Rasterize a MaskShape into an alpha buffer at the given resolution.
///
/// Returns `Vec<u8>` of length `width * height`, where each byte is an
/// alpha value (0 = transparent, 255 = opaque).
pub fn rasterize_mask_shape(
    shape: &MaskShape,
    width: u32,
    height: u32,
    feather: f32,
    expansion: f32,
    opacity: f32,
) -> Vec<u8> {
    let w = width.max(1) as usize;
    let h = height.max(1) as usize;
    let total = w * h;
    let mut alpha = vec![0u8; total];

    let opacity = opacity.clamp(0.0, 1.0);
    if opacity <= 1.0 / 255.0 {
        return alpha;
    }

    let inv_w = 1.0 / w as f32;
    let inv_h = 1.0 / h as f32;

    for y in 0..h {
        for x in 0..w {
            // Normalized coordinates [0, 1]
            let px = (x as f32 + 0.5) * inv_w;
            let py = (y as f32 + 0.5) * inv_h;

            let dist = shape_sdf(shape, px, py);
            // Apply expansion: positive expands the shape (pushes edge outward).
            let expanded = dist - expansion * inv_w.max(inv_h);

            // Apply feather: transition zone around the edge.
            let feather_px = (feather.max(0.0) * 0.5) * inv_w.max(inv_h).max(1e-6);
            let a = if feather_px <= 1e-8 {
                if expanded <= 0.0 { 1.0 } else { 0.0 }
            } else {
                1.0 - smoothstep(-feather_px, feather_px, expanded)
            };

            let final_a = (a * opacity).clamp(0.0, 1.0);
            alpha[y * w + x] = (final_a * 255.0).round() as u8;
        }
    }

    alpha
}

/// Signed Distance Field for a MaskShape at normalized coordinates (px, py) in [0, 1].
/// Negative = inside, positive = outside.
fn shape_sdf(shape: &MaskShape, px: f32, py: f32) -> f32 {
    match shape {
        MaskShape::Rectangle { x, y, width, height, corner_radius } => {
            rect_sdf(px, py, *x, *y, *width, *height, *corner_radius)
        }
        MaskShape::Ellipse { center, radii } => {
            ellipse_sdf(px, py, center.x, center.y, radii.x, radii.y)
        }
        MaskShape::Path { points, closed } => {
            path_sdf(px, py, points, *closed)
        }
    }
}

/// SDF for a rounded rectangle.
fn rect_sdf(px: f32, py: f32, rx: f32, ry: f32, rw: f32, rh: f32, cr: f32) -> f32 {
    let cx = rx + rw * 0.5;
    let cy = ry + rh * 0.5;
    let hw = rw * 0.5;
    let hh = rh * 0.5;

    let dx = (px - cx).abs() - hw + cr;
    let dy = (py - cy).abs() - hh + cr;

    let outside = Vec2::new(dx.max(0.0), dy.max(0.0)).length();
    let inside = dx.max(dy).min(0.0);

    outside + inside - cr
}

/// SDF for an ellipse with given center and radii.
fn ellipse_sdf(px: f32, py: f32, cx: f32, cy: f32, rx: f32, ry: f32) -> f32 {
    let rx = rx.max(1e-8);
    let ry = ry.max(1e-8);
    let dx = (px - cx) / rx;
    let dy = (py - cy) / ry;
    let len = (dx * dx + dy * dy).sqrt();
    if len <= 1e-10 {
        return -1.0;
    }
    (len - 1.0) * rx.min(ry)
}

/// SDF for a Bezier path — subdivide to polyline then compute distance.
fn path_sdf(px: f32, py: f32, points: &[BezierPoint], closed: bool) -> f32 {
    if points.is_empty() {
        return 1.0;
    }
    if points.len() == 1 {
        let dx = px - points[0].position.x;
        let dy = py - points[0].position.y;
        return (dx * dx + dy * dy).sqrt();
    }

    let segments = subdivide_path(points, closed);
    let mut min_dist = f32::MAX;

    let target = Vec2::new(px, py);

    for seg in &segments {
        let d = point_to_segment_dist(target, seg.0, seg.1);
        min_dist = min_dist.min(d);
    }

    // Determine inside/outside via even-odd winding (only if closed).
    if closed {
        let inside = winding_number(target, &segments) % 2 != 0;
        if inside { -min_dist } else { min_dist }
    } else {
        min_dist
    }
}

/// Subdivide Bezier curves into line segments.
fn subdivide_path(points: &[BezierPoint], closed: bool) -> Vec<(Vec2, Vec2)> {
    let mut segments = Vec::new();
    let n = points.len();
    for i in 0..n {
        let next = if i + 1 < n { i + 1 } else if closed { 0 } else { break };
        subdivide_bezier(points[i], points[next], &mut segments);
    }
    segments
}

const BEZIER_SUBDIVISIONS: usize = 8;

/// Subdivide a cubic Bezier segment into line segments.
fn subdivide_bezier(a: BezierPoint, b: BezierPoint, out: &mut Vec<(Vec2, Vec2)>) {
    let steps = BEZIER_SUBDIVISIONS;
    let inv = 1.0 / steps as f32;
    let mut prev = a.position;
    for s in 1..=steps {
        let t = s as f32 * inv;
        let pt = cubic_bezier(a, b, t);
        out.push((prev, pt));
        prev = pt;
    }
}

/// Evaluate a cubic Bezier at parameter t in [0, 1].
fn cubic_bezier(a: BezierPoint, b: BezierPoint, t: f32) -> Vec2 {
    let t2 = t * t;
    let t3 = t2 * t;
    let u = 1.0 - t;
    let u2 = u * u;
    let u3 = u2 * u;
    a.position * u3
        + (a.position + a.control_out) * (3.0 * u2 * t)
        + (b.position + b.control_in) * (3.0 * u * t2)
        + b.position * t3
}

/// Minimum distance from point to line segment.
fn point_to_segment_dist(p: Vec2, a: Vec2, b: Vec2) -> f32 {
    let ab = b - a;
    let ap = p - a;
    let len2 = ab.length_squared();
    if len2 < 1e-10 {
        return ap.length();
    }
    let t = (ap.dot(ab) / len2).clamp(0.0, 1.0);
    let closest = a + ab * t;
    (p - closest).length()
}

/// Compute the winding number of a point relative to a polygon.
fn winding_number(p: Vec2, segments: &[(Vec2, Vec2)]) -> i32 {
    let mut wn = 0i32;
    for &(a, b) in segments {
        if a.y <= p.y {
            if b.y > p.y && cross2d(b - a, p - a) > 0.0 {
                wn += 1;
            }
        } else if b.y <= p.y && cross2d(b - a, p - a) < 0.0 {
            wn -= 1;
        }
    }
    wn
}

fn cross2d(a: Vec2, b: Vec2) -> f32 {
    a.x * b.y - a.y * b.x
}

/// Smooth Hermite interpolation between edge0 and edge1.
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_rectangle_produces_full_alpha_inside() {
        // A rectangle covering the entire [0, 1] area.
        let shape = MaskShape::Rectangle {
            x: 0.0, y: 0.0, width: 1.0, height: 1.0, corner_radius: 0.0,
        };
        let alpha = rasterize_mask_shape(&shape, 64, 64, 0.0, 0.0, 1.0);
        // All pixels should be fully opaque (inside the shape).
        assert!(alpha.iter().all(|&a| a > 200));
    }

    #[test]
    fn small_rectangle_has_transparent_border() {
        let shape = MaskShape::Rectangle {
            x: 0.25, y: 0.25, width: 0.5, height: 0.5, corner_radius: 0.0,
        };
        let alpha = rasterize_mask_shape(&shape, 64, 64, 0.0, 0.0, 1.0);
        // Center should be opaque.
        let center = alpha[32 * 64 + 32];
        assert!(center > 200);
        // Corner should be transparent.
        let corner = alpha[0];
        assert!(corner < 50);
    }

    #[test]
    fn feather_softens_edge() {
        let shape = MaskShape::Rectangle {
            x: 0.25, y: 0.25, width: 0.5, height: 0.5, corner_radius: 0.0,
        };
        let hard = rasterize_mask_shape(&shape, 64, 64, 0.0, 0.0, 1.0);
        let soft = rasterize_mask_shape(&shape, 64, 64, 10.0, 0.0, 1.0);
        // Hard edge: all pixels are either near opaque or near transparent.
        let _hard_binary: Vec<_> = hard.iter().map(|&a| a > 128).collect();
        let _soft_binary: Vec<_> = soft.iter().map(|&a| a > 128).collect();
        // Feather should smooth the transition — fewer purely binary pixels.
        let hard_mid = hard.iter().filter(|&&a| a > 30 && a < 220).count();
        let soft_mid = soft.iter().filter(|&&a| a > 30 && a < 220).count();
        assert!(soft_mid > hard_mid, "feather should create more transitional pixels");
    }

    #[test]
    fn expansion_expands_shape() {
        let shape = MaskShape::Rectangle {
            x: 0.25, y: 0.25, width: 0.5, height: 0.5, corner_radius: 0.0,
        };
        let normal = rasterize_mask_shape(&shape, 64, 64, 0.0, 0.0, 1.0);
        // Positive expansion = larger mask.
        let expanded = rasterize_mask_shape(&shape, 64, 64, 0.0, 10.0, 1.0);
        let normal_count = normal.iter().filter(|&&a| a > 128).count();
        let expanded_count = expanded.iter().filter(|&&a| a > 128).count();
        assert!(expanded_count > normal_count, "expansion should increase visible area");
    }

    #[test]
    fn opacity_scales_alpha() {
        let shape = MaskShape::Rectangle {
            x: 0.0, y: 0.0, width: 1.0, height: 1.0, corner_radius: 0.0,
        };
        let full = rasterize_mask_shape(&shape, 32, 32, 0.0, 0.0, 1.0);
        let half = rasterize_mask_shape(&shape, 32, 32, 0.0, 0.0, 0.5);
        let full_avg = full.iter().map(|&a| a as f32).sum::<f32>() / full.len() as f32;
        let half_avg = half.iter().map(|&a| a as f32).sum::<f32>() / half.len() as f32;
        assert!((half_avg * 2.0 - full_avg).abs() < 10.0);
    }

    #[test]
    fn ellipse_is_round() {
        let shape = MaskShape::Ellipse {
            center: Vec2::new(0.5, 0.5),
            radii: Vec2::new(0.3, 0.3),
        };
        let alpha = rasterize_mask_shape(&shape, 64, 64, 0.0, 0.0, 1.0);
        // Center is inside.
        assert!(alpha[32 * 64 + 32] > 200);
        // Far corner is outside.
        assert!(alpha[0] < 50);
        // Edges at same radius should have similar alpha.
        let right = alpha[32 * 64 + 50]; // (0.78, 0.5) — radius ~0.28
        let bottom = alpha[50 * 64 + 32]; // (0.5, 0.78) — radius ~0.28
        assert!((right as i32 - bottom as i32).abs() < 20);
    }

    #[test]
    fn zero_opacity_returns_all_zeros() {
        let shape = MaskShape::Rectangle {
            x: 0.0, y: 0.0, width: 1.0, height: 1.0, corner_radius: 0.0,
        };
        let alpha = rasterize_mask_shape(&shape, 32, 32, 0.0, 0.0, 0.0);
        assert!(alpha.iter().all(|&a| a == 0));
    }

    #[test]
    fn degenerate_zero_size_handled() {
        let shape = MaskShape::Rectangle {
            x: 0.5, y: 0.5, width: 0.0, height: 0.0, corner_radius: 0.0,
        };
        // Should not panic.
        let _alpha = rasterize_mask_shape(&shape, 1, 1, 0.0, 0.0, 1.0);
    }
}
