//! 文本渲染器
//!
//! 将文字排版并生成 DrawCommand::Image 列表（每个字形指向 GlyphAtlas UV）。

use mondrian_core::Color;
use mondrian_ui_core::types::{Point, Rect};
use mondrian_ui_renderer::command::DrawCommand;

use crate::atlas::GlyphAtlas;
use crate::font::FontManager;
use crate::layout::TextLayout;

pub struct TextRenderer {
    pub font_manager: FontManager,
    pub atlas: GlyphAtlas,
}

impl TextRenderer {
    pub fn new() -> Self {
        Self {
            font_manager: FontManager::new(),
            atlas: GlyphAtlas::new(2048),
        }
    }

    pub fn layout_and_render(
        &mut self,
        text: &str,
        font_size: f32,
        position: Point,
        color: Color,
        max_width: Option<f32>,
    ) -> Vec<DrawCommand> {
        let attrs = cosmic_text::Attrs::new()
            .family(cosmic_text::Family::SansSerif)
            .weight(cosmic_text::Weight::NORMAL);

        let font_system = &mut self.font_manager.font_system;
        let layout = if let Some(w) = max_width {
            TextLayout::new_multiline(font_system, text, attrs, font_size, w)
        } else {
            TextLayout::new_single_line(font_system, text, attrs, font_size)
        };

        // Real font ascent: distance from line_top to baseline (cosmic-text measured)
        let ascent = layout
            .buffer()
            .layout_runs()
            .next()
            .map(|run| run.line_y - run.line_top)
            .unwrap_or(font_size * 0.75);
        let baseline_y = position.y + ascent;

        let mut commands = Vec::new();
        for (_line_y, glyph) in layout.positioned_glyphs() {
            if let Some((uv_rect, bmp_w, bmp_h, top, left)) =
                self.atlas.get_or_rasterize(font_system, glyph, 0.0)
            {
                let bitmap_x = position.x + glyph.x + left as f32;
                let bitmap_top = baseline_y - top as f32;
                commands.push(DrawCommand::Image {
                    bounds: Rect::new(bitmap_x, bitmap_top, bmp_w as f32, bmp_h as f32),
                    uv_rect,
                    tint: color,
                });
            }
        }
        commands
    }

    pub fn measure_text(&mut self, text: &str, font_size: f32) -> (f32, f32) {
        let attrs = cosmic_text::Attrs::new()
            .family(cosmic_text::Family::SansSerif)
            .weight(cosmic_text::Weight::NORMAL);
        let layout =
            TextLayout::new_single_line(&mut self.font_manager.font_system, text, attrs, font_size);
        layout.size()
    }

    /// Take pending uploads for GPU texture upload
    pub fn take_pending_uploads(&mut self) -> Vec<crate::atlas::GlyphUpload> {
        std::mem::take(&mut self.atlas.pending_uploads)
    }
}

/// 后处理：将 DrawCommand::Text 替换为字形 DrawCommand::Image
///
/// 在每个渲染帧调用 `TreeWalker::paint()` 之后、`UiRenderer::render()` 之前使用。
/// 首次出现的字形本帧不显示（下帧 atlas 上传 GPU 后可见）。
pub fn resolve_text_commands(
    commands: Vec<DrawCommand>,
    text_renderer: &mut TextRenderer,
) -> Vec<DrawCommand> {
    let mut resolved = Vec::with_capacity(commands.len());
    for cmd in commands {
        match cmd {
            DrawCommand::Text { text, style, position, color } => {
                let glyph_cmds =
                    text_renderer.layout_and_render(&text, style.font_size, position, color, None);
                resolved.extend(glyph_cmds);
            }
            _ => resolved.push(cmd),
        }
    }
    resolved
}

impl Default for TextRenderer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Helpers ─────────────────────────────────────────────────────────

    /// Extract [Rect] bounds from Image commands.
    fn image_bounds(cmds: &[DrawCommand]) -> Vec<Rect> {
        cmds.iter()
            .filter_map(|c| {
                if let DrawCommand::Image { bounds, .. } = c {
                    Some(*bounds)
                } else {
                    None
                }
            })
            .collect()
    }

    /// Get the UV rect from the first Image command.
    fn first_uv(cmds: &[DrawCommand]) -> Option<Rect> {
        cmds.iter().find_map(|c| {
            if let DrawCommand::Image { uv_rect, .. } = c {
                Some(*uv_rect)
            } else {
                None
            }
        })
    }

    // ── Basic Tests ─────────────────────────────────────────────────────

    #[test]
    fn text_renderer_creates() {
        let _r = TextRenderer::new();
    }

    #[test]
    fn text_renderer_default_creates() {
        let _r = TextRenderer::default();
    }

    #[test]
    fn measure_text_returns_positive() {
        let mut r = TextRenderer::new();
        let (w, h) = r.measure_text("Hello", 16.0);
        assert!(w > 0.0);
        assert!(h > 0.0);
    }

    #[test]
    fn measure_empty_text() {
        let mut r = TextRenderer::new();
        let (w, _h) = r.measure_text("", 16.0);
        assert_eq!(w, 0.0);
    }

    #[test]
    fn layout_and_render_empty_text() {
        let mut r = TextRenderer::new();
        let cmds = r.layout_and_render("", 16.0, Point::ZERO, Color::WHITE, None);
        assert!(cmds.is_empty());
    }

    #[test]
    fn different_chars_have_different_uvs() {
        let mut r = TextRenderer::new();
        let _ = r.layout_and_render("AB", 24.0, Point::ZERO, Color::WHITE, None);
        assert!(!r.atlas.pending_uploads.is_empty());
        let cmds = r.layout_and_render("AB", 24.0, Point::ZERO, Color::WHITE, None);
        let uvs: Vec<Rect> = cmds
            .iter()
            .filter_map(|c| {
                if let DrawCommand::Image { uv_rect, .. } = c {
                    Some(*uv_rect)
                } else {
                    None
                }
            })
            .collect();
        assert!(
            uvs.len() >= 2,
            "Expected 2+ glyphs for 'AB', got {}",
            uvs.len()
        );
        if uvs.len() >= 2 {
            let (r1, r2) = (uvs[0], uvs[1]);
            let same = (r1.x - r2.x).abs() < 0.0001
                && (r1.y - r2.y).abs() < 0.0001
                && (r1.width - r2.width).abs() < 0.0001
                && (r1.height - r2.height).abs() < 0.0001;
            assert!(!same,
                "Characters 'A' and 'B' mapped to the SAME UV rect {:?} — glyph differentiation broken", r1);
        }
    }

    #[test]
    fn upload_position_matches_uv_rect() {
        let mut r = TextRenderer::new();
        let _ = r.layout_and_render("AB", 24.0, Point::ZERO, Color::WHITE, None);
        let uploads = r.take_pending_uploads();
        assert!(!uploads.is_empty());
        let cmds = r.layout_and_render("AB", 24.0, Point::ZERO, Color::WHITE, None);
        let uvs: Vec<Rect> = cmds
            .iter()
            .filter_map(|c| {
                if let DrawCommand::Image { uv_rect, .. } = c {
                    Some(*uv_rect)
                } else {
                    None
                }
            })
            .collect();
        assert!(
            !uvs.is_empty(),
            "Second frame should produce Image commands"
        );
        for upload in &uploads {
            let expected_u = upload.x as f32 / 2048.0;
            let expected_v = upload.y as f32 / 2048.0;
            let found = uvs
                .iter()
                .any(|uv| (uv.x - expected_u).abs() < 0.001 && (uv.y - expected_v).abs() < 0.001);
            assert!(
                found,
                "Upload at ({},{}) has no matching UV rect among {:?}",
                upload.x, upload.y, uvs
            );
        }
    }

    #[test]
    fn consecutive_chars_have_consecutive_uvs() {
        let mut r = TextRenderer::new();
        let _ = r.layout_and_render("ABC", 24.0, Point::ZERO, Color::WHITE, None);
        let _ = r.take_pending_uploads();
        let cmds = r.layout_and_render("ABC", 24.0, Point::ZERO, Color::WHITE, None);
        let uvs: Vec<Rect> = cmds
            .iter()
            .filter_map(|c| {
                if let DrawCommand::Image { uv_rect, .. } = c {
                    Some(*uv_rect)
                } else {
                    None
                }
            })
            .collect();
        assert!(uvs.len() >= 3, "Expected 3 glyphs, got {}", uvs.len());
        assert_ne!(uvs[0], uvs[1], "A and B should have different UVs");
        assert_ne!(uvs[1], uvs[2], "B and C should have different UVs");
        assert_ne!(uvs[0], uvs[2], "A and C should have different UVs");
    }

    #[test]
    fn text_commands_produce_non_empty_bitmaps() {
        let mut r = TextRenderer::new();
        let _ = r.layout_and_render("Test", 16.0, Point::ZERO, Color::WHITE, None);
        let uploads = r.take_pending_uploads();
        assert!(
            !uploads.is_empty(),
            "Should have pending uploads after rasterization"
        );
        for u in &uploads {
            let non_zero = u.data.iter().filter(|&&b| b > 0).count();
            assert!(
                non_zero > 0,
                "Glyph {}x{} bitmap is all zeros!",
                u.width,
                u.height
            );
        }
    }

    // ── Baseline Consistency Tests ──────────────────────────────────────

    /// Glyphs from the same render call share the same `baseline_y`.
    /// Since `bounds.y = baseline_y - top`, glyphs with the same `top`
    /// must have equal `bounds.y`. Glyphs with different `top` values
    /// will have different `bounds.y`, proportional to their top delta.
    #[test]
    fn all_glyphs_on_line_y_positions_are_consistent() {
        let mut r = TextRenderer::new();
        let text = "Hello World";
        for &fs in &[12.0, 13.0, 14.0, 16.0, 20.0, 24.0, 36.0] {
            let _ = r.layout_and_render(text, fs, Point::ZERO, Color::WHITE, None);
            let _ = r.take_pending_uploads();
            let cmds = r.layout_and_render(text, fs, Point::ZERO, Color::WHITE, None);
            let bounds = image_bounds(&cmds);
            assert!(!bounds.is_empty(), "No glyphs for '{}' at {}px", text, fs);
            // Glyph y positions should all be near each other
            // (they share the same baseline, differing only by their top bearing)
            let y_positions: Vec<f32> = bounds.iter().map(|b| b.y).collect();
            let y_min = y_positions.iter().cloned().fold(f32::INFINITY, f32::min);
            let y_max = y_positions.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
            assert!(
                y_max - y_min <= fs * 1.5,
                "At {}px, glyph y positions vary by {:.1}px (max={:.1}, min={:.1}). Range too large.",
                fs, y_max - y_min, y_max, y_min
            );
        }
    }

    /// All glyph positions should be near the requested position.
    #[test]
    fn glyphs_positioned_near_requested_point() {
        let mut r = TextRenderer::new();
        for &fs in &[14.0, 16.0, 24.0] {
            for &(text, px, py) in &[
                ("i", 0.0, 0.0),
                ("e", 0.0, 0.0),
                ("l", 0.0, 0.0),
                ("M", 100.0, 50.0),
                ("g", 100.0, 50.0),
            ] {
                let _ = r.layout_and_render(text, fs, Point::new(px, py), Color::WHITE, None);
                let _ = r.take_pending_uploads();
                let cmds = r.layout_and_render(text, fs, Point::new(px, py), Color::WHITE, None);
                let bounds = image_bounds(&cmds);
                if bounds.is_empty() {
                    continue;
                }
                let b = bounds[0];
                // x should be near the requested position
                assert!(
                    b.x >= px - fs && b.x <= px + fs * 2.0,
                    "'{}' at {}px pos=({},{:.0}): bitmap_x={:.1} far from expected range [{:.1}, {:.1}]",
                    text, fs, px, py, b.x, px - fs, px + fs * 2.0
                );
                // y should be reasonable (baseline is px.y + ascent, then bitmap top = baseline - top)
                assert!(
                    b.y >= py - fs * 2.0 && b.y <= py + fs * 2.0,
                    "'{}' at {}px pos=({},{:.0}): bitmap_y={:.1} out of range [{:.1}, {:.1}]",
                    text,
                    fs,
                    px,
                    py,
                    b.y,
                    py - fs * 2.0,
                    py + fs * 2.0
                );
            }
        }
    }

    // ── Glyph Advance Consistency Tests ──────────────────────────────────

    /// Consecutive glyph centers must be monotonically increasing (LTR text).
    #[test]
    fn consecutive_glyph_centers_monotonic() {
        let mut r = TextRenderer::new();
        let text = "ABCDEFG";
        for &fs in &[12.0, 13.0, 14.0, 16.0, 20.0, 24.0] {
            let _ = r.layout_and_render(text, fs, Point::ZERO, Color::WHITE, None);
            let _ = r.take_pending_uploads();
            let cmds = r.layout_and_render(text, fs, Point::ZERO, Color::WHITE, None);
            let bounds = image_bounds(&cmds);
            if bounds.len() < 2 {
                continue;
            }
            let centers: Vec<f32> = bounds.iter().map(|b| b.x + b.width / 2.0).collect();
            for w in centers.windows(2) {
                assert!(
                    w[0] < w[1],
                    "At {}px, glyph centers not monotonically increasing: {:?}. Bounds: {:?}",
                    fs,
                    centers,
                    bounds
                );
            }
        }
    }

    /// Consecutive glyphs should not overlap by more than 3px.
    #[test]
    fn consecutive_glyphs_minimal_overlap() {
        let mut r = TextRenderer::new();
        let text = "ABCDEFG";
        for &fs in &[12.0, 13.0, 14.0, 16.0, 20.0, 24.0] {
            let _ = r.layout_and_render(text, fs, Point::ZERO, Color::WHITE, None);
            let _ = r.take_pending_uploads();
            let cmds = r.layout_and_render(text, fs, Point::ZERO, Color::WHITE, None);
            let bounds = image_bounds(&cmds);
            if bounds.len() < 2 {
                continue;
            }
            for w in bounds.windows(2) {
                let gap = w[1].x - (w[0].x + w[0].width);
                assert!(
                    gap > -3.0,
                    "At {}px, consecutive glyphs overlap by {:.1}px. Bounds: {:?}",
                    fs,
                    -gap,
                    bounds
                );
            }
        }
    }

    /// Known kerning pairs should not be drastically wider than non-kerning pairs.
    #[test]
    fn kerning_not_severely_wider_than_unrelated_pairs() {
        let mut r = TextRenderer::new();
        for &fs in &[16.0, 24.0, 36.0] {
            let _ = r.layout_and_render("AV", fs, Point::ZERO, Color::WHITE, None);
            let _ = r.take_pending_uploads();
            let bounds_av =
                image_bounds(&r.layout_and_render("AV", fs, Point::ZERO, Color::WHITE, None));

            let _ = r.layout_and_render("AB", fs, Point::ZERO, Color::WHITE, None);
            let _ = r.take_pending_uploads();
            let bounds_ab =
                image_bounds(&r.layout_and_render("AB", fs, Point::ZERO, Color::WHITE, None));

            if bounds_av.len() >= 2 && bounds_ab.len() >= 2 {
                let gap_av = bounds_av[1].x - (bounds_av[0].x + bounds_av[0].width);
                let gap_ab = bounds_ab[1].x - (bounds_ab[0].x + bounds_ab[0].width);
                assert!(
                    gap_av - gap_ab <= fs * 0.15 + 3.0,
                    "At {}px: AV gap={:.2} vs AB gap={:.2} — AV should not be significantly wider. Kerning may be broken.",
                    fs, gap_av, gap_ab
                );
            }
        }
    }

    // ── Subpixel Positioning Tests ──────────────────────────────────────

    /// Subpixel x offset must shift the bitmap position.
    #[test]
    fn subpixel_positioning_shifts_bitmap() {
        let mut r = TextRenderer::new();
        let _ = r.layout_and_render("A", 16.0, Point::new(0.0, 0.0), Color::WHITE, None);
        let _ = r.take_pending_uploads();

        let bounds_int =
            image_bounds(&r.layout_and_render("A", 16.0, Point::new(0.0, 0.0), Color::WHITE, None));
        let bounds_frac =
            image_bounds(&r.layout_and_render("A", 16.0, Point::new(0.5, 0.0), Color::WHITE, None));

        if !bounds_int.is_empty() && !bounds_frac.is_empty() {
            let dx = bounds_frac[0].x - bounds_int[0].x;
            assert!(
                (dx - 0.5).abs() < 1.0,
                "Subpixel 0.5px shift should move bitmap by ~0.5px, got {:.2}px. Int: {:?}, Frac: {:?}",
                dx, bounds_int[0], bounds_frac[0]
            );
        }
    }

    /// UV rect should be identical regardless of subpixel offset.
    #[test]
    fn subpixel_retains_same_uv() {
        let mut r = TextRenderer::new();
        let _ = r.layout_and_render("A", 16.0, Point::ZERO, Color::WHITE, None);
        let _ = r.take_pending_uploads();

        let uv1 =
            first_uv(&r.layout_and_render("A", 16.0, Point::new(0.0, 0.0), Color::WHITE, None));
        let uv2 =
            first_uv(&r.layout_and_render("A", 16.0, Point::new(0.25, 0.0), Color::WHITE, None));
        let uv3 =
            first_uv(&r.layout_and_render("A", 16.0, Point::new(0.75, 0.0), Color::WHITE, None));

        if let (Some(uv1), Some(uv2), Some(uv3)) = (uv1, uv2, uv3) {
            assert_eq!(uv1.x, uv2.x, "UV x changed with subpixel offset");
            assert_eq!(uv1.y, uv2.y, "UV y changed with subpixel offset");
            assert_eq!(uv1.x, uv3.x, "UV x changed with subpixel offset");
            assert_eq!(uv1.y, uv3.y, "UV y changed with subpixel offset");
        }
    }

    // ── Problematic Character Tests ──────────────────────────────────────

    /// Test 'i', 'e', 'l' at all common font sizes for basic sanity.
    #[test]
    fn problematic_chars_have_reasonable_bounds() {
        let mut r = TextRenderer::new();
        for &fs in &[
            12.0, 13.0, 14.0, 15.0, 16.0, 18.0, 20.0, 24.0, 28.0, 32.0, 36.0,
        ] {
            for &ch in &['i', 'e', 'l'] {
                let text = ch.to_string();
                let _ =
                    r.layout_and_render(&text, fs, Point::new(100.0, 100.0), Color::WHITE, None);
                let _ = r.take_pending_uploads();
                let cmds =
                    r.layout_and_render(&text, fs, Point::new(100.0, 100.0), Color::WHITE, None);
                let bounds = image_bounds(&cmds);
                if bounds.is_empty() {
                    continue;
                }
                let b = bounds[0];
                assert!(
                    b.x >= 90.0 && b.x <= 120.0,
                    "'{}' at {}px: bitmap_x={:.1} far from expected ~100.0",
                    ch,
                    fs,
                    b.x
                );
                assert!(
                    b.width >= 1.0,
                    "'{}' at {}px: bitmap width {:.1} too small",
                    ch,
                    fs,
                    b.width
                );
                assert!(
                    b.width <= fs * 3.0,
                    "'{}' at {}px: bitmap width {:.1} suspiciously large",
                    ch,
                    fs,
                    b.width
                );
                assert!(
                    b.height >= fs * 0.3,
                    "'{}' at {}px: bitmap height {:.1} too small",
                    ch,
                    fs,
                    b.height
                );
                assert!(
                    b.height <= fs * 2.5,
                    "'{}' at {}px: bitmap height {:.1} suspiciously large",
                    ch,
                    fs,
                    b.height
                );
            }
        }
    }

    /// 'i' and 'l' should have similar center positions relative to their advance origin.
    #[test]
    fn i_and_l_have_consistent_centers() {
        let mut r = TextRenderer::new();
        for &fs in &[14.0, 16.0, 20.0, 24.0] {
            let mut centers: Vec<(char, f32)> = Vec::new();
            for &ch in &['i', 'l', 'I', 'f', 't'] {
                let text = ch.to_string();
                let _ = r.layout_and_render(&text, fs, Point::new(0.0, 0.0), Color::WHITE, None);
                let _ = r.take_pending_uploads();
                let cmds = r.layout_and_render(&text, fs, Point::new(0.0, 0.0), Color::WHITE, None);
                let bounds = image_bounds(&cmds);
                if !bounds.is_empty() {
                    centers.push((ch, bounds[0].x + bounds[0].width / 2.0));
                }
            }
            if centers.len() >= 2 {
                let avg: f32 = centers.iter().map(|(_, c)| c).sum::<f32>() / centers.len() as f32;
                for (ch, c) in &centers {
                    assert!(
                        (c - avg).abs() <= fs * 0.2 + 1.5,
                        "At {}px, '{}' center={:.1} deviates from avg {:.1}. All centers: {:?}",
                        fs,
                        ch,
                        c,
                        avg,
                        centers
                    );
                }
            }
        }
    }

    // ── Bounds Integrity Tests ───────────────────────────────────────────

    /// All glyph bounds must be non-zero and within reasonable range.
    #[test]
    fn all_bounds_are_reasonable() {
        let mut r = TextRenderer::new();
        let text = "The quick brown fox jumps over the lazy dog 0123456789";
        for &fs in &[10.0, 12.0, 13.0, 14.0, 16.0, 20.0, 24.0, 36.0] {
            let _ = r.layout_and_render(text, fs, Point::ZERO, Color::WHITE, None);
            let _ = r.take_pending_uploads();
            let cmds = r.layout_and_render(text, fs, Point::ZERO, Color::WHITE, None);
            for cmd in &cmds {
                if let DrawCommand::Image { bounds, .. } = cmd {
                    assert!(
                        bounds.width > 0.0 && bounds.height > 0.0,
                        "Empty glyph at {}px: {:?}",
                        fs,
                        bounds
                    );
                    assert!(
                        bounds.width <= fs * 5.0,
                        "Glyph too wide at {}px: {:?}",
                        fs,
                        bounds
                    );
                    assert!(
                        bounds.height <= fs * 4.0,
                        "Glyph too tall at {}px: {:?}",
                        fs,
                        bounds
                    );
                }
            }
        }
    }

    // ── Font Size Continuity ─────────────────────────────────────────────

    /// Adjacent font sizes should produce roughly proportional widths.
    #[test]
    fn adjacent_sizes_produce_proportional_metrics() {
        let mut r = TextRenderer::new();
        let text = "Hello";
        let sizes: &[f32] = &[
            10.0, 11.0, 12.0, 13.0, 14.0, 15.0, 16.0, 18.0, 20.0, 24.0, 28.0, 32.0, 36.0,
        ];
        let mut prev: Option<(f32, f32)> = None; // (size, width)
        for &fs in sizes {
            let (w, h) = r.measure_text(text, fs);
            assert!(w > 0.0, "Zero width at {}px", fs);
            assert!(h > 0.0, "Zero height at {}px", fs);
            if let Some((prev_size, prev_w)) = prev {
                let expected_ratio = fs / prev_size;
                let actual_ratio = w / prev_w;
                assert!(
                    (actual_ratio - expected_ratio).abs() <= 0.4,
                    "Width scaling {:.0}→{:.0}px: expected ratio ~{:.2}, got {:.2} ({:.1} → {:.1})",
                    prev_size,
                    fs,
                    expected_ratio,
                    actual_ratio,
                    prev_w,
                    w
                );
            }
            prev = Some((fs, w));
        }
    }

    // ── Full Throughput: Text → Image Commands ───────────────────────────

    /// Verify that `resolve_text_commands` correctly transforms Text→Image.
    #[test]
    fn resolve_text_commands_produces_image_commands() {
        let mut r = TextRenderer::new();
        let commands = vec![DrawCommand::Text {
            text: "Hi".to_string(),
            style: mondrian_ui_theme::typography::TextStyle {
                font_size: 16.0,
                line_height: 1.3,
                font_weight: mondrian_ui_theme::typography::FontWeight::Regular,
                letter_spacing: 0.0,
            },
            position: Point::new(10.0, 20.0),
            color: Color::WHITE,
        }];
        // First pass: rasterizes
        let _ = resolve_text_commands(commands.clone(), &mut r);
        let _ = r.take_pending_uploads();
        // Second pass: should return Image commands
        let resolved = resolve_text_commands(commands, &mut r);
        let image_count =
            resolved.iter().filter(|c| matches!(c, DrawCommand::Image { .. })).count();
        assert!(
            image_count > 0,
            "Expected Image commands, got none. Commands: {:?}",
            resolved
        );
        // No Text commands should remain
        let text_remaining =
            resolved.iter().filter(|c| matches!(c, DrawCommand::Text { .. })).count();
        assert_eq!(
            text_remaining, 0,
            "Text commands should all be resolved to Image"
        );
    }
}
