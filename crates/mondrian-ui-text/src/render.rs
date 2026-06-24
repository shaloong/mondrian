//! 文本渲染器
//!
//! 将文字排版并生成 DrawCommand::Image 列表（每个字形指向 GlyphAtlas UV）。

use mondrian_core::Color;
use mondrian_ui_core::types::{Point, Rect};
use mondrian_ui_renderer::command::DrawCommand;

use crate::atlas::{GlyphAtlas, GlyphRasterizeOutcome};
use crate::font::FontManager;
use crate::layout::TextLayout;

pub struct TextRenderer {
    pub font_manager: FontManager,
    pub atlas: GlyphAtlas,
}

/// Diagnostics collected while resolving text commands into glyph images.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TextResolveStats {
    /// Number of text commands resolved in this pass.
    pub text_commands: u32,
    /// Number of laid-out glyphs that requested an atlas image.
    pub glyphs_requested: u32,
    /// Number of laid-out glyphs resolved to image draw commands.
    pub glyphs_resolved: u32,
    /// Number of laid-out glyphs that could not be rasterized or atlas-allocated.
    pub missing_glyphs: u32,
    /// Atlas page resets triggered while resolving this frame.
    pub atlas_page_resets: u32,
    /// Glyph atlas generation after this resolve pass.
    pub atlas_generation: u64,
}

impl TextResolveStats {
    fn add(&mut self, other: Self) {
        self.text_commands = self.text_commands.saturating_add(other.text_commands);
        self.glyphs_requested = self.glyphs_requested.saturating_add(other.glyphs_requested);
        self.glyphs_resolved = self.glyphs_resolved.saturating_add(other.glyphs_resolved);
        self.missing_glyphs = self.missing_glyphs.saturating_add(other.missing_glyphs);
        self.atlas_page_resets = self.atlas_page_resets.saturating_add(other.atlas_page_resets);
        self.atlas_generation = other.atlas_generation;
    }
}

/// Text resolve output for one frame.
#[derive(Debug, Clone, Default)]
pub struct ResolvedTextCommands {
    /// Draw commands after every text command has been replaced by glyph images.
    pub commands: Vec<DrawCommand>,
    /// Diagnostics gathered while resolving text in this frame.
    pub stats: TextResolveStats,
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
        self.layout_and_render_with_stats(text, font_size, position, color, max_width, true)
            .commands
    }

    fn layout_and_render_with_stats(
        &mut self,
        text: &str,
        font_size: f32,
        position: Point,
        color: Color,
        max_width: Option<f32>,
        allow_atlas_page_reset: bool,
    ) -> ResolvedTextCommands {
        let attrs = cosmic_text::Attrs::new()
            .family(cosmic_text::Family::SansSerif)
            .weight(cosmic_text::Weight::NORMAL);

        let font_system = &mut self.font_manager.font_system;
        let layout = if let Some(w) = max_width {
            TextLayout::new_multiline(font_system, text, attrs, font_size, w)
        } else {
            TextLayout::new_single_line(font_system, text, attrs, font_size)
        };

        let mut commands = Vec::new();
        let mut stats = TextResolveStats::default();
        for (line_y, glyph) in layout.positioned_glyphs() {
            stats.glyphs_requested = stats.glyphs_requested.saturating_add(1);
            match self.atlas.get_or_rasterize_with_policy(
                font_system,
                glyph,
                allow_atlas_page_reset,
            ) {
                GlyphRasterizeOutcome::Resolved(uv_rect, bmp_w, bmp_h, top, left) => {
                    let bitmap_x = position.x + glyph.x + left as f32;
                    let bitmap_top = position.y + line_y - top as f32;
                    commands.push(DrawCommand::Image {
                        bounds: Rect::new(bitmap_x, bitmap_top, bmp_w as f32, bmp_h as f32),
                        uv_rect,
                        tint: color,
                    });
                    stats.glyphs_resolved = stats.glyphs_resolved.saturating_add(1);
                }
                GlyphRasterizeOutcome::PageReset => {
                    stats.atlas_page_resets = stats.atlas_page_resets.saturating_add(1);
                    stats.atlas_generation = self.atlas.generation();
                    return ResolvedTextCommands { commands: Vec::new(), stats };
                }
                GlyphRasterizeOutcome::Missing => {
                    stats.missing_glyphs = stats.missing_glyphs.saturating_add(1);
                }
            }
        }
        stats.atlas_generation = self.atlas.generation();
        ResolvedTextCommands { commands, stats }
    }

    pub fn measure_text(&mut self, text: &str, font_size: f32) -> (f32, f32) {
        let attrs = cosmic_text::Attrs::new()
            .family(cosmic_text::Family::SansSerif)
            .weight(cosmic_text::Weight::NORMAL);
        let layout =
            TextLayout::new_single_line(&mut self.font_manager.font_system, text, attrs, font_size);
        layout.size()
    }

    /// Measure wrapped paragraph text using the same cosmic-text layout path
    /// used by `layout_and_render`.
    pub fn measure_text_box(&mut self, text: &str, font_size: f32, max_width: f32) -> (f32, f32) {
        let attrs = cosmic_text::Attrs::new()
            .family(cosmic_text::Family::SansSerif)
            .weight(cosmic_text::Weight::NORMAL);
        let layout = TextLayout::new_multiline(
            &mut self.font_manager.font_system,
            text,
            attrs,
            font_size,
            max_width.max(1.0),
        );
        layout.size()
    }

    /// Take pending uploads for GPU texture upload
    pub fn take_pending_uploads(&mut self) -> Vec<crate::atlas::GlyphUpload> {
        std::mem::take(&mut self.atlas.pending_uploads)
    }
}

/// 后处理：将 DrawCommand::Text 替换为字形 DrawCommand::Image
///
/// 在每个渲染帧调用 `TreeWalker::paint()` 之后、
/// `UiRenderer::render_resolved_commands()` 之前使用。
/// 调用者应在提交本帧前上传 `TextRenderer::take_pending_uploads()`，这样首次出现
/// 的字形也能在同一帧可见。
pub fn resolve_text_commands(
    commands: Vec<DrawCommand>,
    text_renderer: &mut TextRenderer,
) -> ResolvedTextCommands {
    let mut allow_atlas_page_reset = true;
    let mut atlas_page_resets = 0u32;
    loop {
        let mut resolved = Vec::with_capacity(commands.len());
        let mut stats = TextResolveStats::default();
        let mut restart_after_page_reset = false;
        for cmd in commands.iter().cloned() {
            match cmd {
                DrawCommand::Text { text, style, position, max_width, color } => {
                    let mut glyph_result = text_renderer.layout_and_render_with_stats(
                        &text,
                        style.font_size,
                        position,
                        color,
                        max_width,
                        allow_atlas_page_reset,
                    );
                    glyph_result.stats.text_commands =
                        glyph_result.stats.text_commands.saturating_add(1);
                    let page_reset = glyph_result.stats.atlas_page_resets > 0;
                    stats.add(glyph_result.stats);
                    if page_reset && allow_atlas_page_reset {
                        restart_after_page_reset = true;
                        break;
                    }
                    resolved.append(&mut glyph_result.commands);
                }
                _ => resolved.push(cmd),
            }
        }
        stats.atlas_generation = text_renderer.atlas.generation();
        if restart_after_page_reset {
            atlas_page_resets = atlas_page_resets.saturating_add(stats.atlas_page_resets);
            allow_atlas_page_reset = false;
            continue;
        }
        stats.atlas_page_resets = stats.atlas_page_resets.saturating_add(atlas_page_resets);
        return ResolvedTextCommands { commands: resolved, stats };
    }
}

impl Default for TextRenderer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_ui_renderer::{GlyphUpload as RendererGlyphUpload, UiRenderer};

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

    fn renderer_glyph_uploads(uploads: Vec<crate::atlas::GlyphUpload>) -> Vec<RendererGlyphUpload> {
        uploads
            .into_iter()
            .map(|upload| RendererGlyphUpload {
                x: upload.x,
                y: upload.y,
                width: upload.width,
                height: upload.height,
                data: upload.data,
            })
            .collect()
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
    fn offscreen_text_renderer_uploads_glyphs_and_draws_visible_pixels() {
        let Some(mut harness) = OffscreenHarness::new(128, 48) else {
            return;
        };
        let mut text_renderer = TextRenderer::new();
        let commands = vec![DrawCommand::Text {
            text: "UI".to_string(),
            style: mondrian_ui_theme::typography::TextStyle {
                font_size: 28.0,
                line_height: 34.0,
                font_weight: mondrian_ui_theme::typography::FontWeight::Regular,
                letter_spacing: 0.0,
            },
            position: Point::new(12.0, 34.0),
            max_width: None,
            color: Color::WHITE,
        }];

        let resolved = resolve_text_commands(commands, &mut text_renderer);
        assert_eq!(resolved.stats.text_commands, 1);
        assert!(resolved.stats.glyphs_resolved > 0);
        assert_eq!(resolved.stats.missing_glyphs, 0);

        let uploads = renderer_glyph_uploads(text_renderer.take_pending_uploads());
        assert!(
            !uploads.is_empty(),
            "first text render should upload glyphs"
        );
        harness.renderer.upload_glyphs(&harness.queue, &uploads);

        let pixels = harness.render(resolved.commands);
        let visible_alpha = pixels.chunks_exact(4).filter(|px| px[3] >= 24).count();
        assert!(
            visible_alpha > 24,
            "text glyph rendering should produce visible alpha pixels, got {visible_alpha}"
        );

        let stats = harness.last_stats.expect("render should record stats");
        assert_eq!(stats.unresolved_text_commands, 0);
        assert_eq!(stats.failed_raster_images, 0);
    }

    #[test]
    fn resolve_text_commands_restarts_frame_after_atlas_page_reset() {
        let mut text_renderer = TextRenderer::new();
        text_renderer.atlas.fill_page_for_test();

        let style = mondrian_ui_theme::typography::TextStyle {
            font_size: 18.0,
            line_height: 22.0,
            font_weight: mondrian_ui_theme::typography::FontWeight::Regular,
            letter_spacing: 0.0,
        };
        let commands = vec![
            DrawCommand::Text {
                text: "A".to_string(),
                style: style.clone(),
                position: Point::new(0.0, 20.0),
                max_width: None,
                color: Color::WHITE,
            },
            DrawCommand::Text {
                text: "B".to_string(),
                style,
                position: Point::new(20.0, 20.0),
                max_width: None,
                color: Color::WHITE,
            },
        ];

        let resolved = resolve_text_commands(commands, &mut text_renderer);
        let image_count = resolved
            .commands
            .iter()
            .filter(|command| matches!(command, DrawCommand::Image { .. }))
            .count();

        assert_eq!(resolved.stats.atlas_page_resets, 1);
        assert_eq!(resolved.stats.atlas_generation, 1);
        assert_eq!(resolved.stats.text_commands, 2);
        assert_eq!(resolved.stats.missing_glyphs, 0);
        assert!(image_count >= 2);
        assert!(text_renderer.atlas.has_pending());
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
    fn measure_text_box_wraps_to_max_width() {
        let mut r = TextRenderer::new();
        let text = "Tooltip text that should wrap into multiple lines";
        let (single_w, single_h) = r.measure_text(text, 16.0);
        let (wrapped_w, wrapped_h) = r.measure_text_box(text, 16.0, 120.0);

        assert!(single_w > 120.0);
        assert!(wrapped_w <= 120.0);
        assert!(wrapped_h > single_h);
    }

    #[test]
    fn layout_and_render_text_box_uses_multiple_baselines() {
        let mut r = TextRenderer::new();
        let text = "Tooltip text that should wrap into multiple lines";
        let _ = r.layout_and_render(text, 16.0, Point::ZERO, Color::WHITE, Some(120.0));
        let _ = r.take_pending_uploads();
        let cmds = r.layout_and_render(text, 16.0, Point::ZERO, Color::WHITE, Some(120.0));
        let mut ys: Vec<i32> =
            image_bounds(&cmds).iter().map(|bounds| bounds.y.round() as i32).collect();
        ys.sort_unstable();
        ys.dedup();

        assert!(
            ys.len() > 1,
            "wrapped text should render on multiple y positions"
        );
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

    #[test]
    fn small_mixed_script_and_emoji_text_resolves_without_missing_glyphs() {
        let mut r = TextRenderer::new();
        let text = "A你🙂B";
        for &fs in &[10.0, 12.0, 14.0] {
            let resolved = r.layout_and_render_with_stats(
                text,
                fs,
                Point::new(16.0, 24.0),
                Color::WHITE,
                None,
                true,
            );
            assert_eq!(
                resolved.stats.missing_glyphs, 0,
                "mixed Latin/CJK/emoji text should resolve through font fallback at {fs}px"
            );
            assert!(
                resolved.stats.glyphs_requested >= 4,
                "expected at least one glyph per user-visible mixed-script character at {fs}px"
            );
            assert_eq!(
                resolved.stats.glyphs_requested, resolved.stats.glyphs_resolved,
                "all requested glyphs should resolve to renderer image commands at {fs}px"
            );

            let bounds = image_bounds(&resolved.commands);
            assert!(
                bounds.len() >= 4,
                "expected visible glyph image bounds for mixed-script text at {fs}px"
            );
            for bounds in bounds {
                assert!(bounds.x.is_finite() && bounds.y.is_finite());
                assert!(bounds.width > 0.0 && bounds.height > 0.0);
                assert!(
                    bounds.width <= fs * 6.0 && bounds.height <= fs * 6.0,
                    "fallback glyph bounds should stay proportional at {fs}px: {bounds:?}"
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
            max_width: None,
            color: Color::WHITE,
        }];
        let result = resolve_text_commands(commands, &mut r);
        let resolved = result.commands;
        let image_count =
            resolved.iter().filter(|c| matches!(c, DrawCommand::Image { .. })).count();
        assert!(
            image_count > 0,
            "Expected Image commands, got none. Commands: {:?}",
            resolved
        );
        assert!(
            !r.take_pending_uploads().is_empty(),
            "first text resolve should expose glyph uploads before the frame is submitted"
        );
        // No Text commands should remain
        let text_remaining =
            resolved.iter().filter(|c| matches!(c, DrawCommand::Text { .. })).count();
        assert_eq!(
            text_remaining, 0,
            "Text commands should all be resolved to Image"
        );
        assert_eq!(result.stats.text_commands, 1);
        assert!(result.stats.glyphs_requested >= image_count as u32);
        assert_eq!(result.stats.glyphs_resolved, image_count as u32);
        assert_eq!(result.stats.missing_glyphs, 0);
    }

    #[test]
    fn resolve_text_commands_keeps_glyph_images_inside_clip_scope() {
        let mut r = TextRenderer::new();
        let clip = Rect::new(4.0, 5.0, 80.0, 24.0);
        let commands = vec![
            DrawCommand::PushClip { bounds: clip },
            DrawCommand::Text {
                text: "Hi".to_string(),
                style: mondrian_ui_theme::typography::TextStyle {
                    font_size: 16.0,
                    line_height: 20.8,
                    font_weight: mondrian_ui_theme::typography::FontWeight::Regular,
                    letter_spacing: 0.0,
                },
                position: Point::new(10.0, 20.0),
                max_width: None,
                color: Color::WHITE,
            },
            DrawCommand::PopClip,
        ];

        let result = resolve_text_commands(commands, &mut r);
        let first = result.commands.first().expect("push clip should remain first");
        let last = result.commands.last().expect("pop clip should remain last");

        assert!(matches!(first, DrawCommand::PushClip { bounds } if *bounds == clip));
        assert!(matches!(last, DrawCommand::PopClip));
        assert!(
            result.commands[1..result.commands.len() - 1]
                .iter()
                .all(|command| matches!(command, DrawCommand::Image { .. })),
            "resolved glyph images must remain between the original clip commands: {:?}",
            result.commands
        );
    }

    #[test]
    fn resolve_text_commands_preserves_renderer_primitives() {
        let mut r = TextRenderer::new();
        let shadow = DrawCommand::SoftShadow {
            bounds: Rect::new(12.0, 18.0, 96.0, 40.0),
            color: Color::from_rgba8(0, 0, 0, 90),
            corner_radius: 8.0,
            blur_radius: 24.0,
            spread: 2.0,
            offset: Point::new(0.0, 8.0).to_vec2(),
        };

        let result = resolve_text_commands(vec![shadow.clone()], &mut r);

        assert_eq!(result.commands.len(), 1);
        assert!(
            matches!(
                &result.commands[0],
                DrawCommand::SoftShadow {
                    bounds,
                    color,
                    corner_radius,
                    blur_radius,
                    spread,
                    offset,
                } if *bounds == Rect::new(12.0, 18.0, 96.0, 40.0)
                    && *color == Color::from_rgba8(0, 0, 0, 90)
                    && (*corner_radius - 8.0).abs() < f32::EPSILON
                    && (*blur_radius - 24.0).abs() < f32::EPSILON
                    && (*spread - 2.0).abs() < f32::EPSILON
                    && *offset == Point::new(0.0, 8.0).to_vec2()
            ),
            "renderer primitive should pass through unchanged: {:?}",
            result.commands
        );
        assert_eq!(result.stats.text_commands, 0);
        assert_eq!(result.stats.glyphs_requested, 0);
    }

    struct OffscreenHarness {
        device: wgpu::Device,
        queue: wgpu::Queue,
        renderer: UiRenderer,
        texture: wgpu::Texture,
        size: (u32, u32),
        last_stats: Option<mondrian_ui_renderer::UiRenderFrameStats>,
    }

    impl OffscreenHarness {
        fn new(width: u32, height: u32) -> Option<Self> {
            let instance = wgpu::Instance::new(
                wgpu::InstanceDescriptor::new_without_display_handle_from_env(),
            );
            let adapter =
                pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                    compatible_surface: None,
                    power_preference: wgpu::PowerPreference::LowPower,
                    force_fallback_adapter: false,
                }))
                .ok()?;
            let (device, queue) =
                pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
                    .ok()?;
            let format = wgpu::TextureFormat::Rgba8Unorm;
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("ui_text_offscreen_test_target"),
                size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let renderer = UiRenderer::new(&device, format);

            Some(Self {
                device,
                queue,
                renderer,
                texture,
                size: (width, height),
                last_stats: None,
            })
        }

        fn render(&mut self, commands: Vec<DrawCommand>) -> Vec<u8> {
            let view = self.texture.create_view(&wgpu::TextureViewDescriptor::default());
            let stats = self.renderer.render_resolved_commands(
                &self.device,
                &self.queue,
                &view,
                &commands,
                self.size,
            );
            self.last_stats = Some(stats);
            self.readback()
        }

        fn readback(&self) -> Vec<u8> {
            let (width, height) = self.size;
            let bytes_per_pixel = 4u32;
            let unpadded_bytes_per_row = width * bytes_per_pixel;
            let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
            let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(align) * align;
            let buffer_size = padded_bytes_per_row as u64 * height as u64;

            let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("ui_text_offscreen_test_readback"),
                size: buffer_size,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("ui_text_offscreen_test_readback_encoder"),
            });
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &readback,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(padded_bytes_per_row),
                        rows_per_image: Some(height),
                    },
                },
                wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            );
            self.queue.submit([encoder.finish()]);

            let (tx, rx) = std::sync::mpsc::channel();
            let slice = readback.slice(..);
            slice.map_async(wgpu::MapMode::Read, move |result| {
                let _ = tx.send(result);
            });
            let _ =
                self.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None });
            rx.recv()
                .expect("readback map callback should run")
                .expect("readback map should succeed");

            let mapped = slice.get_mapped_range();
            let mut out = vec![0u8; width as usize * height as usize * 4];
            for row in 0..height as usize {
                let src_start = row * padded_bytes_per_row as usize;
                let src_end = src_start + unpadded_bytes_per_row as usize;
                let dst_start = row * unpadded_bytes_per_row as usize;
                let dst_end = dst_start + unpadded_bytes_per_row as usize;
                out[dst_start..dst_end].copy_from_slice(&mapped[src_start..src_end]);
            }
            drop(mapped);
            readback.unmap();
            out
        }
    }
}
